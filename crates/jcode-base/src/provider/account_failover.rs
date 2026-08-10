use super::ActiveProvider;

/// How long an account stays on cooldown after it answered 429.
///
/// Anthropic's burst limit clears in seconds, so this is deliberately short: it
/// exists to stop the *next* request re-proving what the last one just learned,
/// not to take an account out of service.
const RATE_LIMIT_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(60);

/// Accounts observed rate-limited recently, as `(provider, label) -> observed`.
///
/// `tried_accounts` inside the retry loop lives for one request. Nothing carried
/// a 429 across requests, so every new request would rotate straight back into
/// the account that had just refused it, and a burst on one account showed up
/// as `claude-2 → claude-1 → claude-2 → claude-1`, spending real quota on each
/// hop to rediscover the same fact.
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

/// Record that `label` answered with a rate limit just now.
pub fn note_account_rate_limited(provider: ActiveProvider, label: &str) {
    if let Ok(mut seen) = RATE_LIMITED_ACCOUNTS.lock() {
        seen.insert(
            (cooldown_provider_key(provider), label.to_string()),
            std::time::Instant::now(),
        );
    }
}

/// Whether `label` refused a request within the cooldown window.
pub fn account_is_cooling_down(provider: ActiveProvider, label: &str) -> bool {
    let Ok(mut seen) = RATE_LIMITED_ACCOUNTS.lock() else {
        return false;
    };
    let key = (cooldown_provider_key(provider), label.to_string());
    match seen.get(&key) {
        Some(observed) if observed.elapsed() < RATE_LIMIT_COOLDOWN => true,
        Some(_) => {
            // Expired: drop it so the map cannot grow without bound and the
            // account is immediately eligible again.
            seen.remove(&key);
            false
        }
        None => false,
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

    // Accounts that refused a request moments ago go last rather than being
    // dropped: they are the correct choice when everything else is limited too,
    // but trying them first is what produced the observed
    // `claude-2 → claude-1 → claude-2` ping-pong.
    labels.sort_by_key(|label| account_is_cooling_down(provider, label));
    labels
}

/// Test seam: apply only the cooldown ordering to a fixed candidate list.
///
/// The real `same_provider_account_candidates` also calls the network usage
/// probe and reads the on-disk account store, neither of which a unit test can
/// drive. This exposes the ordering rule on its own, which is the part that
/// fixes the ping-pong.
pub fn same_provider_account_candidates_for_tests(
    provider: ActiveProvider,
    mut labels: Vec<String>,
) -> Vec<String> {
    labels.sort_by_key(|label| account_is_cooling_down(provider, label));
    labels
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

fn error_looks_like_usage_limit(summary: &str) -> bool {
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
