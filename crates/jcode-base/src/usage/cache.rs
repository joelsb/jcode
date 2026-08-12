use super::openai_helpers::{classify_openai_limits, usage_percent_to_ratio};
use super::{AccountUsageSnapshot, OpenAIUsageData, ProviderUsage, UsageData, UsageLimit};
use std::collections::HashMap;
use std::time::Instant;

/// Shared Anthropic usage cache used by the info widget, `/usage`, and
/// multi-account fallback logic so they don't hammer the same endpoint through
/// separate code paths.
static ANTHROPIC_USAGE_CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<String, UsageData>>> =
    std::sync::OnceLock::new();

/// Shared OpenAI usage cache keyed by account label/token prefix.
static OPENAI_ACCOUNT_USAGE_CACHE: std::sync::OnceLock<
    std::sync::Mutex<HashMap<String, OpenAIUsageData>>,
> = std::sync::OnceLock::new();

fn anthropic_usage_cache() -> &'static std::sync::Mutex<HashMap<String, UsageData>> {
    ANTHROPIC_USAGE_CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

fn openai_usage_cache() -> &'static std::sync::Mutex<HashMap<String, OpenAIUsageData>> {
    OPENAI_ACCOUNT_USAGE_CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

pub(super) fn anthropic_usage_cache_key(access_token: &str, account_label: Option<&str>) -> String {
    if let Some(label) = account_label
        .map(str::trim)
        .filter(|label| !label.is_empty())
    {
        return format!("label:{}", label);
    }

    let prefix = access_token
        .get(..20)
        .unwrap_or(access_token)
        .trim()
        .to_string();
    format!("token:{}", prefix)
}

pub(super) fn openai_usage_cache_key(access_token: &str, account_label: Option<&str>) -> String {
    if let Some(label) = account_label
        .map(str::trim)
        .filter(|label| !label.is_empty())
    {
        return format!("label:{}", label);
    }

    let prefix = access_token
        .get(..20)
        .unwrap_or(access_token)
        .trim()
        .to_string();
    format!("token:{}", prefix)
}

pub(super) fn cached_anthropic_usage(cache_key: &str) -> Option<UsageData> {
    let cache = anthropic_usage_cache();
    let map = cache.lock().ok()?;
    let cached = map.get(cache_key)?.clone();
    (!cached.is_stale()).then_some(cached)
}

pub(super) fn store_anthropic_usage(cache_key: String, data: UsageData) {
    if let Ok(mut map) = anthropic_usage_cache().lock() {
        map.insert(cache_key, data);
    }
}

/// Forget the cached usage snapshot for one Anthropic account.
///
/// This cache is what [`crate::usage::account_usage_probe_sync`] reads, and so
/// it is what decides whether rotation considers an account usable. Until
/// 2026-08-12 nothing could invalidate it: no account switch, no 429, nothing.
/// Its only expiry was the 300s TTL, stretched to 900s by `RATE_LIMIT_BACKOFF`
/// for an entry carrying a rate-limit error, so a "has headroom" verdict
/// outlived the limit that contradicted it and rotation kept selecting a
/// blocked account. The epoch guard from ADR-0010 protects the *other*,
/// single-slot cache, which this decision path never reads. See ADR-0017.
pub fn invalidate_anthropic_account_usage(label: &str) {
    let label = label.trim();
    if label.is_empty() {
        return;
    }
    if let Ok(mut map) = anthropic_usage_cache().lock() {
        map.remove(&anthropic_usage_cache_key("", Some(label)));
    }
}

/// Record that `label` answered with a rate limit, so the next rotation
/// decision sees the limit rather than a stale "has headroom" snapshot.
///
/// A 429 is the strongest possible evidence that a cached verdict is wrong, and
/// it is free: it needs no network round trip, unlike the usage probe it
/// corrects.
///
/// `resets_at` decides how strong a claim this makes, and the asymmetry is
/// deliberate:
///
/// - **Known reset** (the provider named one): write a spent 5-hour window
///   carrying that reset. `exhausted` is the field every consumer reads, so an
///   entry that merely carried an error string would still report headroom.
///   `is_stale` treats a passed reset as stale, so the entry expires exactly
///   when the account recovers rather than on a fixed timer.
/// - **Unknown reset**: only *invalidate*, never assert exhaustion. A 429 with
///   no reset may be a burst limit that clears in seconds, and an entry whose
///   `last_error` mentions a rate limit gets the 900s `RATE_LIMIT_BACKOFF` TTL,
///   which would sideline a recovered account for fifteen minutes. Forgetting
///   the stale verdict is enough; the short-lived rotation cooldown covers the
///   "do not immediately try it again" part. See ADR-0017.
pub fn note_anthropic_account_rate_limited(label: &str, resets_at: Option<String>) {
    let label = label.trim();
    if label.is_empty() {
        return;
    }

    let Some(resets_at) = resets_at else {
        invalidate_anthropic_account_usage(label);
        return;
    };

    let evidence = UsageData {
        five_hour: 1.0,
        five_hour_resets_at: Some(resets_at),
        // Left at 0: a 429 proves *a* window is full, not which one, and
        // claiming the weekly quota is spent would misreport the reset horizon.
        // With `exhausted` now a disjunction (ADR-0016), one full window is
        // enough to keep this account out of rotation.
        seven_day: 0.0,
        seven_day_resets_at: None,
        seven_day_opus: None,
        extra_usage_enabled: false,
        fetched_at: Some(Instant::now()),
        last_error: None,
    };
    store_anthropic_usage(anthropic_usage_cache_key("", Some(label)), evidence);
}

/// Test seam: plant a usage snapshot for one account, as if a probe had just
/// fetched it.
///
/// Exists so a test can set up the *usage verdict* on its own, with no rotation
/// cooldown involved. Without this separation a test that seeds a limit via
/// [`note_anthropic_account_rate_limited`] also sets a cooldown, and the cooldown
/// alone is enough to steer rotation, which silently hides whether the
/// `exhausted` predicate is right. That is exactly how the first version of the
/// end-to-end test passed against the very bug it was written to catch.
pub fn seed_anthropic_account_usage_for_tests(
    label: &str,
    five_hour: f32,
    seven_day: f32,
    resets_at: Option<String>,
) {
    store_anthropic_usage(
        anthropic_usage_cache_key("", Some(label)),
        UsageData {
            five_hour,
            five_hour_resets_at: resets_at.clone(),
            seven_day,
            seven_day_resets_at: resets_at,
            seven_day_opus: None,
            extra_usage_enabled: false,
            fetched_at: Some(Instant::now()),
            last_error: None,
        },
    );
}

/// Test seam: forget every per-account Anthropic usage snapshot.
///
/// The cache is process-global while tests run in one process, so a test that
/// seeds a verdict for `claude-1`/`claude-2` leaks it into every later test using
/// the same fixture labels. Observed as `a_burst_limit_that_clears_lets_the_turn_finish`
/// and the rotation-bound test failing only when run after the quota test.
/// Tests that seed usage must clear it, at both ends.
pub fn clear_anthropic_account_usage_for_tests() {
    if let Ok(mut map) = anthropic_usage_cache().lock() {
        map.clear();
    }
}

/// Read the cached usage for one account by label, respecting the freshness TTL.
///
/// This is the reader every *decision* should use: it is keyed the same way the
/// writers are, so a 429 recorded against a label is visible here.
pub(super) fn cached_anthropic_account_usage(label: &str) -> Option<UsageData> {
    let label = label.trim();
    if label.is_empty() {
        return None;
    }
    cached_anthropic_usage(&anthropic_usage_cache_key("", Some(label)))
}

/// Read the cached usage for one account, ignoring the freshness TTL.
///
/// Only for reporting a known reset time on a path that must not block on a
/// network fetch. Decisions use the TTL-respecting readers.
pub fn cached_anthropic_account_usage_any_age(label: &str) -> Option<UsageData> {
    let label = label.trim();
    if label.is_empty() {
        return None;
    }
    let map = anthropic_usage_cache().lock().ok()?;
    map.get(&anthropic_usage_cache_key("", Some(label)))
        .cloned()
}

pub(super) fn cached_openai_usage(cache_key: &str) -> Option<OpenAIUsageData> {
    let cache = openai_usage_cache();
    let map = cache.lock().ok()?;
    let cached = map.get(cache_key)?.clone();
    (!cached.is_stale()).then_some(cached)
}

pub(super) fn store_openai_usage(cache_key: String, data: OpenAIUsageData) {
    if let Ok(mut map) = openai_usage_cache().lock() {
        let previous = map.get(&cache_key).cloned();
        let previous_exhausted = previous
            .as_ref()
            .map(OpenAIUsageData::exhausted)
            .unwrap_or(false);
        let current_exhausted = data.exhausted();
        let previous_hard_limit = previous
            .as_ref()
            .map(|usage| usage.hard_limit_reached)
            .unwrap_or(false);
        if previous.is_none()
            || previous_exhausted != current_exhausted
            || previous_hard_limit != data.hard_limit_reached
        {
            crate::logging::info(&format!(
                "OpenAI limit diag: usage cache update key={} prev_exhausted={} new_exhausted={} prev_hard_limit={} new_hard_limit={} snapshot=({})",
                cache_key,
                previous_exhausted,
                current_exhausted,
                previous_hard_limit,
                data.hard_limit_reached,
                data.diagnostic_fields()
            ));
        }
        map.insert(cache_key, data);
    }
}

pub(super) fn anthropic_usage_error(err_msg: String) -> UsageData {
    UsageData {
        fetched_at: Some(Instant::now()),
        last_error: Some(err_msg),
        ..Default::default()
    }
}

pub(super) fn provider_report_from_usage_data(
    display_name: String,
    data: &UsageData,
) -> ProviderUsage {
    if let Some(error) = &data.last_error {
        return ProviderUsage {
            provider_name: display_name,
            error: Some(error.clone()),
            ..Default::default()
        };
    }

    let mut limits = Vec::new();
    limits.push(UsageLimit {
        name: "5-hour window".to_string(),
        usage_percent: data.five_hour * 100.0,
        resets_at: data.five_hour_resets_at.clone(),
    });
    limits.push(UsageLimit {
        name: "7-day window".to_string(),
        usage_percent: data.seven_day * 100.0,
        resets_at: data.seven_day_resets_at.clone(),
    });
    if let Some(opus) = data.seven_day_opus {
        limits.push(UsageLimit {
            name: "7-day Opus window".to_string(),
            usage_percent: opus * 100.0,
            resets_at: data.seven_day_resets_at.clone(),
        });
    }

    let mut extra_info = Vec::new();
    extra_info.push((
        "Extra usage (long context)".to_string(),
        if data.extra_usage_enabled {
            "enabled".to_string()
        } else {
            "disabled".to_string()
        },
    ));

    ProviderUsage {
        provider_name: display_name,
        limits,
        extra_info,
        hard_limit_reached: false,
        error: None,
        last_used_unix_secs: None,
    }
}

pub(super) fn usage_data_from_provider_report(report: &ProviderUsage) -> UsageData {
    if let Some(error) = &report.error {
        return UsageData {
            fetched_at: Some(Instant::now()),
            last_error: Some(error.clone()),
            ..Default::default()
        };
    }

    let five_hour = report
        .limits
        .iter()
        .find(|limit| limit.name == "5-hour window");
    let seven_day = report
        .limits
        .iter()
        .find(|limit| limit.name == "7-day window");
    let seven_day_opus = report
        .limits
        .iter()
        .find(|limit| limit.name == "7-day Opus window");
    let extra_usage_enabled = report.extra_info.iter().find_map(|(key, value)| {
        if key == "Extra usage (long context)" {
            Some(value == "enabled")
        } else {
            None
        }
    });

    UsageData {
        five_hour: five_hour
            .map(|limit| usage_percent_to_ratio(limit.usage_percent))
            .unwrap_or(0.0),
        five_hour_resets_at: five_hour.and_then(|limit| limit.resets_at.clone()),
        seven_day: seven_day
            .map(|limit| usage_percent_to_ratio(limit.usage_percent))
            .unwrap_or(0.0),
        seven_day_resets_at: seven_day.and_then(|limit| limit.resets_at.clone()),
        seven_day_opus: seven_day_opus.map(|limit| usage_percent_to_ratio(limit.usage_percent)),
        extra_usage_enabled: extra_usage_enabled.unwrap_or(false),
        fetched_at: Some(Instant::now()),
        last_error: None,
    }
}

pub(super) fn openai_usage_data_from_provider_report(report: &ProviderUsage) -> OpenAIUsageData {
    let mut data = classify_openai_limits(&report.limits);
    data.hard_limit_reached = report.hard_limit_reached;
    data.fetched_at = Some(Instant::now());
    data.last_error = report.error.clone();
    data
}

pub(super) fn provider_report_from_openai_usage_data(
    display_name: String,
    data: &OpenAIUsageData,
) -> ProviderUsage {
    if let Some(error) = &data.last_error {
        return ProviderUsage {
            provider_name: display_name,
            error: Some(error.clone()),
            ..Default::default()
        };
    }

    let mut limits = Vec::new();
    if let Some(window) = &data.five_hour {
        limits.push(UsageLimit {
            name: window.name.clone(),
            usage_percent: window.usage_ratio * 100.0,
            resets_at: window.resets_at.clone(),
        });
    }
    if let Some(window) = &data.seven_day {
        limits.push(UsageLimit {
            name: window.name.clone(),
            usage_percent: window.usage_ratio * 100.0,
            resets_at: window.resets_at.clone(),
        });
    }
    if let Some(window) = &data.spark {
        limits.push(UsageLimit {
            name: window.name.clone(),
            usage_percent: window.usage_ratio * 100.0,
            resets_at: window.resets_at.clone(),
        });
    }

    ProviderUsage {
        provider_name: display_name,
        limits,
        extra_info: Vec::new(),
        hard_limit_reached: data.hard_limit_reached,
        error: None,
        last_used_unix_secs: None,
    }
}

pub(super) fn openai_snapshot_from_usage(
    label: String,
    email: Option<String>,
    usage: &OpenAIUsageData,
) -> AccountUsageSnapshot {
    let five_hour_ratio = usage.five_hour.as_ref().map(|window| window.usage_ratio);
    let seven_day_ratio = usage.seven_day.as_ref().map(|window| window.usage_ratio);
    let exhausted = usage.exhausted();

    AccountUsageSnapshot {
        label,
        email,
        exhausted,
        primary_label: usage
            .five_hour
            .as_ref()
            .map(|window| window.name.trim_end_matches(" window").to_string()),
        five_hour_ratio,
        secondary_label: usage
            .seven_day
            .as_ref()
            .map(|window| window.name.trim_end_matches(" window").to_string()),
        seven_day_ratio,
        resets_at: usage.blocking_reset_at(),
        error: usage.last_error.clone(),
    }
}

pub(super) fn anthropic_snapshot_from_usage(
    label: String,
    email: Option<String>,
    usage: &UsageData,
) -> AccountUsageSnapshot {
    AccountUsageSnapshot {
        label,
        email,
        exhausted: usage.exhausted(),
        primary_label: Some("5h".to_string()),
        five_hour_ratio: Some(usage.five_hour),
        secondary_label: Some("7d".to_string()),
        seven_day_ratio: Some(usage.seven_day),
        resets_at: usage.blocking_reset_at(),
        error: usage.last_error.clone(),
    }
}
