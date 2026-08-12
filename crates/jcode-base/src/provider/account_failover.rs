use super::ActiveProvider;

/// How long an account stays on cooldown after it answered 429 with no reset
/// time attached.
///
/// Only the fallback for an *unexplained* 429: Anthropic's burst limit clears in
/// seconds, so this is deliberately short and exists to stop the next request
/// re-proving what the last one just learned. When the provider or the usage
/// probe names a real reset, [`note_account_rate_limited_until`] uses that
/// instead, because a 60s memory of a 3-hour quota wall is useless.
const RATE_LIMIT_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(60);

/// Accounts observed rate-limited recently, as `(provider, label) -> until`.
///
/// `tried_accounts` inside the retry loop lives for one request. Nothing carried
/// a 429 across requests, so every new request would rotate straight back into
/// the account that had just refused it, and a burst on one account showed up
/// as `claude-2 → claude-1 → claude-2 → claude-1`, spending real quota on each
/// hop to rediscover the same fact.
///
/// The value is the instant the cooldown *expires*, not when it started: a
/// quota limit's expiry comes from the provider's reset time and can be hours
/// out, while a burst limit's is seconds. Storing the start forced a single
/// fixed window on both. See ADR-0016.
/// Keyed by `(provider name, label)` rather than the enum, so this does not
/// force `Hash`/`Eq` onto a type shared across the whole provider layer.
static RATE_LIMITED_ACCOUNTS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<(&'static str, String), std::time::Instant>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Stable key for a provider.
fn cooldown_provider_key(provider: ActiveProvider) -> &'static str {
    match provider {
        ActiveProvider::Claude => "claude",
        ActiveProvider::OpenAI => "openai",
        ActiveProvider::Copilot => "copilot",
        ActiveProvider::Antigravity => "antigravity",
        ActiveProvider::Gemini => "gemini",
        ActiveProvider::Cursor => "cursor",
        ActiveProvider::Bedrock => "bedrock",
        ActiveProvider::OpenRouter => "openrouter",
    }
}

/// Record that `label` answered with a rate limit just now, with no known reset.
///
/// Uses the short burst-shaped fallback window. Prefer
/// [`note_account_rate_limited_until`] whenever a reset time is known.
pub fn note_account_rate_limited(provider: ActiveProvider, label: &str) {
    note_account_rate_limited_for(provider, label, RATE_LIMIT_COOLDOWN);
}

/// Record a rate limit that is known to last until `reset_at`.
///
/// A spent 5-hour quota can be hours from resetting, and the fixed 60s window
/// made jcode forget that within a minute and select the blocked account again.
/// The duration is clamped: never shorter than the burst fallback (a reset in
/// the past or seconds away must still stop an immediate retry) and never longer
/// than `MAX_RATE_LIMIT_COOLDOWN`, so a malformed or hostile reset timestamp
/// cannot take an account out of service indefinitely.
pub fn note_account_rate_limited_until(
    provider: ActiveProvider,
    label: &str,
    reset_at: Option<chrono::DateTime<chrono::Utc>>,
) {
    let cooldown = reset_at
        .map(|reset| {
            let remaining = reset.signed_duration_since(chrono::Utc::now());
            remaining
                .to_std()
                .unwrap_or(RATE_LIMIT_COOLDOWN)
                .clamp(RATE_LIMIT_COOLDOWN, MAX_RATE_LIMIT_COOLDOWN)
        })
        .unwrap_or(RATE_LIMIT_COOLDOWN);
    note_account_rate_limited_for(provider, label, cooldown);
}

/// Longest a cooldown may last, however far out the provider's reset claims.
///
/// A 7-day window's reset is days away, but an account can also recover early
/// (a switch to a different plan, another window rolling over), and the usage
/// probe is the thing that should notice. Capping keeps a wrong or stale reset
/// from permanently hiding an account.
const MAX_RATE_LIMIT_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

fn note_account_rate_limited_for(
    provider: ActiveProvider,
    label: &str,
    cooldown: std::time::Duration,
) {
    if let Ok(mut seen) = RATE_LIMITED_ACCOUNTS.lock() {
        let until = std::time::Instant::now() + cooldown;
        seen.entry((cooldown_provider_key(provider), label.to_string()))
            // Keep the later deadline: a burst-shaped 60s note arriving after a
            // known 3-hour reset must not shorten it back to a minute.
            .and_modify(|existing| {
                if until > *existing {
                    *existing = until;
                }
            })
            .or_insert(until);
    }
}

/// Whether `label` is still inside a rate-limit cooldown.
pub fn account_is_cooling_down(provider: ActiveProvider, label: &str) -> bool {
    let Ok(mut seen) = RATE_LIMITED_ACCOUNTS.lock() else {
        return false;
    };
    let key = (cooldown_provider_key(provider), label.to_string());
    match seen.get(&key) {
        Some(until) if *until > std::time::Instant::now() => true,
        Some(_) => {
            // Expired: drop it so the map cannot grow without bound and the
            // account is immediately eligible again.
            seen.remove(&key);
            false
        }
        None => false,
    }
}

/// Record everything a 429 on `label` teaches, in one call.
///
/// Three separate pieces of state used to be updated by hand at the call site,
/// and the provider runtime only ever updated one of them, which is how a
/// "has headroom" verdict survived the limit that disproved it:
///
/// 1. the rotation cooldown, sized to the real reset when one is known;
/// 2. the per-account usage cache, so *any* later decision in *any* session sees
///    the limit instead of a stale snapshot (this is the cache the rotation
///    decision actually reads);
/// 3. nothing else - deliberately no network fetch, because this runs on the
///    failure path of a live request.
///
/// `reset_at` is the provider's reset for the limit that fired, when known. With
/// `None` this records a short burst-shaped cooldown and merely *forgets* the
/// cached verdict rather than asserting exhaustion, since an unexplained 429 may
/// clear in seconds. See ADR-0017.
pub fn note_account_rate_limited_with_reset(
    provider: ActiveProvider,
    label: &str,
    reset_at: Option<String>,
) {
    let parsed = reset_at
        .as_deref()
        .and_then(crate::usage::parse_reset_at)
        // A reset already in the past says nothing about the future.
        .filter(|reset| *reset > chrono::Utc::now());

    note_account_rate_limited_until(provider, label, parsed);

    if provider == ActiveProvider::Claude {
        crate::usage::note_anthropic_account_rate_limited(
            label,
            parsed.map(|reset| reset.to_rfc3339()),
        );
    }
}

/// Forget a cooldown and the cached limit verdict, e.g. after a request on that
/// account succeeds and so proves it is serving again.
pub fn clear_account_rate_limit(provider: ActiveProvider, label: &str) {
    clear_account_cooldown(provider, label);
    if provider == ActiveProvider::Claude {
        crate::usage::invalidate_anthropic_account_usage(label);
    }
}

/// Clear a cooldown, e.g. after a request on that account succeeds.
pub fn clear_account_cooldown(provider: ActiveProvider, label: &str) {
    if let Ok(mut seen) = RATE_LIMITED_ACCOUNTS.lock() {
        seen.remove(&(cooldown_provider_key(provider), label.to_string()));
    }
}

/// Test-only: forget every cooldown so tests start from a known state.
pub fn clear_all_account_cooldowns() {
    if let Ok(mut seen) = RATE_LIMITED_ACCOUNTS.lock() {
        seen.clear();
    }
}

/// Serialises tests that mutate the process-global cooldown map.
///
/// `RATE_LIMITED_ACCOUNTS` is one map for the whole process, and cargo runs
/// tests in parallel threads, so two tests that each `clear_all_account_cooldowns`
/// and then assert on candidate ordering interleave and fail each other roughly
/// one run in three. Observed as `cooling_down_accounts_are_skipped_but_remain_a_last_resort`
/// failing in the full suite while passing when run alone.
///
/// Not `cfg(test)`: these tests live in a sibling module compiled as part of the
/// same crate, but the guard is also useful to any future integration test.
pub fn lock_account_cooldown_tests() -> std::sync::MutexGuard<'static, ()> {
    static COOLDOWN_TEST_LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(()));
    COOLDOWN_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(super) fn multi_account_provider_kind(
    provider: ActiveProvider,
) -> Option<crate::usage::MultiAccountProviderKind> {
    match provider {
        ActiveProvider::Claude => Some(crate::usage::MultiAccountProviderKind::Anthropic),
        ActiveProvider::OpenAI => Some(crate::usage::MultiAccountProviderKind::OpenAI),
        _ => None,
    }
}

pub(super) fn account_usage_probe(
    provider: ActiveProvider,
) -> Option<crate::usage::AccountUsageProbe> {
    let kind = multi_account_provider_kind(provider)?;
    crate::usage::account_usage_probe_sync(kind)
}

pub(super) fn same_provider_account_failover_enabled() -> bool {
    crate::config::Config::load()
        .provider
        .same_provider_account_failover
}

pub fn prefer_account_failover_enabled() -> bool {
    let config = crate::config::Config::load();
    config.provider.prefer_account_failover && config.provider.same_provider_account_failover
}

/// Whether the usage probe says every account's *quota* is spent, as opposed to
/// requests merely bouncing right now.
///
/// Anthropic answers 429 for two different situations: a short-window burst
/// limit that clears in seconds, and a 5-hour/weekly quota that does not clear
/// until its reset. They are indistinguishable from the error alone, so the
/// retry loop asks the usage probe: waiting out a burst is worth ~75s, waiting
/// out a spent quota is not, since the reset can be hours away. See ADR-0015.
///
/// `false` when the probe is unavailable or reports headroom, so an unknown
/// state keeps the (recoverable) waiting behavior rather than failing fast.
pub fn all_accounts_quota_exhausted(provider: ActiveProvider) -> bool {
    if let Some(forced) = QUOTA_EXHAUSTED_OVERRIDE.with(|cell| cell.get()) {
        return forced;
    }
    account_usage_probe(provider)
        .map(|probe| {
            if probe.has_multiple_accounts() {
                probe.all_accounts_exhausted()
            } else {
                // With a single account there is nothing to compare against, so
                // `all_accounts_exhausted` is false by construction; the only
                // meaningful signal is that account's own state.
                probe.current_exhausted()
            }
        })
        .unwrap_or(false)
}

thread_local! {
    /// Scoped override for the quota verdict.
    ///
    /// Always compiled, not `cfg(test)`, because the consumer under test lives
    /// in another crate (`jcode-provider-anthropic-runtime`) and a dependency's
    /// `cfg(test)` is not active there. Thread-local and scoped, so parallel
    /// tests cannot observe each other's value, and production never sets it.
    static QUOTA_EXHAUSTED_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// Run `f` with the quota verdict forced, or `None` to restore real probing.
/// Test-only seam; nothing in production calls this.
pub fn with_quota_exhausted_override<T>(forced: Option<bool>, f: impl FnOnce() -> T) -> T {
    QUOTA_EXHAUSTED_OVERRIDE.with(|cell| {
        let previous = cell.replace(forced);
        let result = f();
        cell.set(previous);
        result
    })
}

/// Alternate account labels for `provider`, best-first: accounts the usage
/// probe reports as neither exhausted nor erroring come before the rest, and
/// the currently active account is always excluded.
///
/// Public so a provider runtime can rotate accounts itself. A streaming rate
/// limit is delivered as an item *inside* the event stream, long after
/// `complete()` handed that stream back, so
/// [`MultiProvider::try_same_provider_account_failover`] never observes it and
/// cannot rotate on the runtime's behalf.
pub fn account_failover_candidates(provider: ActiveProvider) -> Vec<String> {
    same_provider_account_candidates(provider)
}

pub(super) fn active_account_label_for_provider(provider: ActiveProvider) -> Option<String> {
    match provider {
        ActiveProvider::Claude => crate::auth::claude::active_account_label(),
        ActiveProvider::OpenAI => crate::auth::codex::active_account_label(),
        _ => None,
    }
}

pub(super) fn set_account_override_for_provider(provider: ActiveProvider, label: Option<String>) {
    match provider {
        ActiveProvider::Claude => crate::auth::claude::set_active_account_override(label),
        ActiveProvider::OpenAI => crate::auth::codex::set_active_account_override(label),
        _ => {}
    }
}

pub(super) fn same_provider_account_candidates(provider: ActiveProvider) -> Vec<String> {
    let current_label = active_account_label_for_provider(provider);
    let mut labels = Vec::new();

    let mut push_unique = |label: String| {
        if current_label.as_deref() == Some(label.as_str()) {
            return;
        }
        if !labels.iter().any(|existing| existing == &label) {
            labels.push(label);
        }
    };

    if let Some(probe) = account_usage_probe(provider) {
        let mut preferred = probe
            .accounts
            .iter()
            .filter(|account| account.label != probe.current_label)
            .filter(|account| !account.exhausted && account.error.is_none())
            .collect::<Vec<_>>();
        preferred.sort_by(|a, b| {
            let a_score = a
                .five_hour_ratio
                .unwrap_or(0.0)
                .max(a.seven_day_ratio.unwrap_or(0.0));
            let b_score = b
                .five_hour_ratio
                .unwrap_or(0.0)
                .max(b.seven_day_ratio.unwrap_or(0.0));
            a_score.total_cmp(&b_score)
        });
        for account in preferred {
            push_unique(account.label.clone());
        }

        for account in probe.accounts {
            push_unique(account.label);
        }
    }

    match provider {
        ActiveProvider::Claude => {
            for account in crate::auth::claude::list_accounts().unwrap_or_default() {
                push_unique(account.label);
            }
        }
        ActiveProvider::OpenAI => {
            for account in crate::auth::codex::list_accounts().unwrap_or_default() {
                push_unique(account.label);
            }
        }
        _ => {}
    }

    // Accounts that refused a request moments ago are dropped while any other
    // candidate remains, and kept as a last resort when they are all that is
    // left: they are the correct choice when everything is limited, but trying
    // one first is what produced the observed `claude-2 → claude-1 → claude-2`
    // ping-pong.
    //
    // Sorting them last was not enough. With two accounts and both cooling, the
    // order among equals is arbitrary, so a spent account could still be tried
    // before a healthy one whose cooldown had not yet expired. See ADR-0016.
    demote_cooling_down_accounts(provider, labels)
}

/// Drop cooling-down accounts while at least one non-cooling candidate remains;
/// otherwise keep every candidate, cooling ones sorted last.
///
/// Never returns an empty list for a non-empty input: exhausting the candidate
/// list turns a recoverable rate limit into a hard failure, which is exactly
/// what ADR-0015 warned against.
fn demote_cooling_down_accounts(provider: ActiveProvider, labels: Vec<String>) -> Vec<String> {
    let (cooling, ready): (Vec<String>, Vec<String>) = labels
        .into_iter()
        .partition(|label| account_is_cooling_down(provider, label));

    if ready.is_empty() {
        return cooling;
    }
    ready
}

/// Test seam: apply only the cooldown demotion to a fixed candidate list.
///
/// The real `same_provider_account_candidates` also calls the network usage
/// probe and reads the on-disk account store, neither of which a unit test can
/// drive. This exposes the ordering rule on its own, which is the part that
/// fixes the ping-pong.
pub fn same_provider_account_candidates_for_tests(
    provider: ActiveProvider,
    labels: Vec<String>,
) -> Vec<String> {
    demote_cooling_down_accounts(provider, labels)
}

/// When the soonest-resetting account will be able to serve a request again,
/// as a human-readable duration plus the account it belongs to.
///
/// Used to replace a silent multi-minute wait with an honest "earliest reset in
/// 2h51m" message. Reads only cached snapshots, at any age, because this runs on
/// the failure path of a request and must not block on the network: a slightly
/// stale reset time is a far better answer than a delay, and the reset itself
/// does not move.
pub fn earliest_account_reset(provider: ActiveProvider) -> Option<(String, String)> {
    if provider != ActiveProvider::Claude {
        return None;
    }

    let labels: Vec<String> = crate::auth::claude::list_accounts()
        .unwrap_or_default()
        .into_iter()
        .map(|account| account.label)
        .collect();

    labels
        .into_iter()
        .filter_map(|label| {
            let usage = crate::usage::cached_anthropic_account_usage_any_age(&label)?;
            let reset_at = usage.blocking_reset_at()?;
            let parsed = crate::usage::parse_reset_at(&reset_at)?;
            Some((label, parsed))
        })
        .min_by_key(|(_, reset_at)| *reset_at)
        .map(|(label, reset_at)| {
            (
                label,
                crate::usage::format_reset_time(&reset_at.to_rfc3339()),
            )
        })
}

pub(super) fn account_switch_guidance(provider: ActiveProvider) -> Option<String> {
    let probe = account_usage_probe(provider)?;
    probe.switch_guidance().or_else(|| {
        (probe.current_exhausted() && probe.all_accounts_exhausted()).then(|| {
            format!(
                "All {} accounts appear exhausted. Use `/usage` to inspect reset times.",
                probe.provider.display_name()
            )
        })
    })
}

pub(super) fn usage_exhausted_reason(provider: ActiveProvider) -> String {
    let mut reason = "OAuth usage exhausted".to_string();
    if let Some(guidance) = account_switch_guidance(provider) {
        reason.push_str(". ");
        reason.push_str(&guidance);
    }
    reason
}

pub fn error_looks_like_usage_limit(summary: &str) -> bool {
    let lower = summary.to_ascii_lowercase();
    [
        "quota",
        "insufficient_quota",
        "rate limit",
        "rate_limit",
        "rate_limit_exceeded",
        "too many requests",
        "billing",
        "credit",
        "payment required",
        "usage exhausted",
        "limit reached",
        "429",
        "402",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

pub(super) fn maybe_annotate_limit_summary(provider: ActiveProvider, summary: String) -> String {
    if !error_looks_like_usage_limit(&summary) {
        return summary;
    }
    let Some(guidance) = account_switch_guidance(provider) else {
        return summary;
    };
    if summary.contains(&guidance) {
        return summary;
    }
    format!("{}. {}", summary, guidance)
}
