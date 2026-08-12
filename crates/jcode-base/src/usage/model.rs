use super::display::{format_reset_time, usage_reset_passed};
use super::{CACHE_DURATION, ERROR_BACKOFF, RATE_LIMIT_BACKOFF};
use serde::Deserialize;
use std::time::Instant;

pub(super) fn mask_email(email: &str) -> String {
    let trimmed = email.trim();
    let Some((local, domain)) = trimmed.split_once('@') else {
        return trimmed.to_string();
    };

    if local.is_empty() {
        return format!("***@{}", domain);
    }

    let mut chars = local.chars();
    let first = chars.next().unwrap_or('*');
    let last = chars.last().unwrap_or(first);

    let masked_local = if local.chars().count() <= 2 {
        format!("{}*", first)
    } else {
        format!("{}***{}", first, last)
    };

    format!("{}@{}", masked_local, domain)
}

pub(super) fn openai_provider_display_name(
    label: &str,
    email: Option<&str>,
    account_count: usize,
    is_active: bool,
) -> String {
    let email_suffix = email
        .map(mask_email)
        .map(|masked| format!(" ({})", masked))
        .unwrap_or_default();

    if account_count <= 1 {
        format!("OpenAI (ChatGPT){}", email_suffix)
    } else {
        let active_marker = if is_active { " ✦" } else { "" };
        format!("OpenAI - {}{}{}", label, email_suffix, active_marker)
    }
}

/// Usage data from the API
#[derive(Debug, Clone, Default)]
pub struct UsageData {
    /// Five-hour window utilization (0.0-1.0)
    pub five_hour: f32,
    /// Five-hour reset time (ISO timestamp)
    pub five_hour_resets_at: Option<String>,
    /// Seven-day window utilization (0.0-1.0)
    pub seven_day: f32,
    /// Seven-day reset time (ISO timestamp)
    pub seven_day_resets_at: Option<String>,
    /// Seven-day Opus utilization (0.0-1.0)
    pub seven_day_opus: Option<f32>,
    /// Model-specific weekly windows returned by newer OAuth usage responses.
    pub model_scoped: Vec<ModelScopedUsageWindow>,
    /// Whether extra usage (long context, etc.) is enabled
    pub extra_usage_enabled: bool,
    /// Last fetch time
    pub fetched_at: Option<Instant>,
    /// Last error (if any)
    pub last_error: Option<String>,
}

impl UsageData {
    /// Whether Anthropic reports an exhausted model-scoped weekly window for
    /// `model`. The API currently uses display names such as `Fable`, while
    /// callers use catalog ids such as `claude-fable-5`, so compare normalized
    /// model families rather than requiring the strings to be identical.
    pub fn model_scoped_exhausted(&self, model: &str) -> bool {
        let model = model.to_ascii_lowercase();
        self.model_scoped.iter().any(|window| {
            let scope = window.model_name.to_ascii_lowercase();
            window.utilization >= 0.99
                && ((model.contains("fable") && scope.contains("fable"))
                    || (model.contains("opus") && scope.contains("opus"))
                    || (model.contains("sonnet") && scope.contains("sonnet"))
                    || (model.contains("haiku") && scope.contains("haiku")))
        })
    }

    /// Utilization at or above which Anthropic refuses further requests on a
    /// window. Not 1.0: the reported ratio is rounded, and a window displayed
    /// as 100% has already started refusing.
    pub const EXHAUSTED_RATIO: f32 = 0.99;

    /// Whether *any* window is spent, and so the account cannot serve a request
    /// until that window resets.
    ///
    /// Anthropic applies the 5-hour and 7-day limits independently and blocks a
    /// request when **either** is full, so this is a disjunction. It read
    /// `five_hour >= 0.99 && seven_day >= 0.99` until 2026-08-12, which made an
    /// account at 5h=100% / 7d=49% report as having headroom: rotation kept
    /// selecting the blocked account, the "every account is exhausted"
    /// fast-fail never fired, and a turn spent ~4 minutes waiting out a limit
    /// that could not clear for hours. See ADR-0016.
    pub fn exhausted(&self) -> bool {
        self.five_hour >= Self::EXHAUSTED_RATIO || self.seven_day >= Self::EXHAUSTED_RATIO
    }

    /// When the window that is actually blocking will reset.
    ///
    /// The reset time only means something when paired with the window it
    /// belongs to. Reporting the 5-hour reset for an account blocked by its
    /// 7-day window would promise recovery hours before it can happen, so this
    /// picks the reset of a spent window, preferring the sooner of the two when
    /// both are spent, and falls back to whatever reset is known otherwise.
    pub fn blocking_reset_at(&self) -> Option<String> {
        let five_hour_spent = self.five_hour >= Self::EXHAUSTED_RATIO;
        let seven_day_spent = self.seven_day >= Self::EXHAUSTED_RATIO;

        match (five_hour_spent, seven_day_spent) {
            // Both spent: the account is usable again only once the later one
            // clears, but the sooner reset is the honest "check back at" time,
            // and `usage_reset_passed` re-probes from there.
            (true, true) => self
                .five_hour_resets_at
                .clone()
                .or_else(|| self.seven_day_resets_at.clone()),
            (true, false) => self.five_hour_resets_at.clone(),
            (false, true) => self.seven_day_resets_at.clone(),
            (false, false) => self
                .five_hour_resets_at
                .clone()
                .or_else(|| self.seven_day_resets_at.clone()),
        }
    }

    /// Check if data is stale and should be refreshed
    pub fn is_stale(&self) -> bool {
        if usage_reset_passed([
            self.five_hour_resets_at.as_deref(),
            self.seven_day_resets_at.as_deref(),
        ]) || self
            .model_scoped
            .iter()
            .any(|window| usage_reset_passed([window.resets_at.as_deref()]))
        {
            return true;
        }

        match self.fetched_at {
            Some(t) => {
                let ttl = if self.is_rate_limited() {
                    RATE_LIMIT_BACKOFF
                } else if self.last_error.is_some() {
                    ERROR_BACKOFF
                } else {
                    CACHE_DURATION
                };
                t.elapsed() > ttl
            }
            None => true,
        }
    }

    /// Drop the freshness stamp so the next `get`/`get_sync` refetches.
    ///
    /// `UsageData` carries no account identity and the cache is global, so a
    /// snapshot taken before an account switch is not stale by time but is
    /// wrong by account. Callers that change the active account must call this.
    pub fn mark_stale(&mut self) {
        self.fetched_at = None;
    }

    /// Check if the last error was a rate limit (429)
    fn is_rate_limited(&self) -> bool {
        self.last_error
            .as_ref()
            .map(|e| e.contains("429") || e.contains("rate limit") || e.contains("Rate limited"))
            .unwrap_or(false)
    }

    /// Format five-hour usage as percentage string
    pub fn five_hour_percent(&self) -> String {
        format!("{:.0}%", self.five_hour * 100.0)
    }

    /// Format seven-day usage as percentage string
    pub fn seven_day_percent(&self) -> String {
        format!("{:.0}%", self.seven_day * 100.0)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelScopedUsageWindow {
    pub model_name: String,
    /// Utilization as a fraction in [0.0, 1.0].
    pub utilization: f32,
    pub resets_at: Option<String>,
}

/// API response structures
#[derive(Deserialize, Debug)]
pub(super) struct UsageResponse {
    pub(super) five_hour: Option<UsageWindow>,
    pub(super) seven_day: Option<UsageWindow>,
    pub(super) seven_day_opus: Option<UsageWindow>,
    #[serde(default)]
    pub(super) limits: Vec<StructuredUsageLimit>,
    pub(super) extra_usage: Option<ExtraUsageResponse>,
}

#[derive(Deserialize, Debug)]
pub(super) struct StructuredUsageLimit {
    pub(super) kind: Option<String>,
    pub(super) percent: Option<f32>,
    pub(super) resets_at: Option<String>,
    pub(super) scope: Option<UsageLimitScope>,
}

#[derive(Deserialize, Debug)]
pub(super) struct UsageLimitScope {
    pub(super) model: Option<UsageLimitModel>,
}

#[derive(Deserialize, Debug)]
pub(super) struct UsageLimitModel {
    pub(super) display_name: Option<String>,
}

#[derive(Deserialize, Debug)]
pub(super) struct UsageWindow {
    pub(super) utilization: Option<f32>,
    pub(super) resets_at: Option<String>,
}

#[derive(Deserialize, Debug)]
pub(super) struct ExtraUsageResponse {
    pub(super) is_enabled: Option<bool>,
}

// ─── Combined usage for /usage command ───────────────────────────────────────

/// Normalized OpenAI/Codex usage window info used by the TUI widget.
#[derive(Debug, Clone, Default)]
pub struct OpenAIUsageWindow {
    pub name: String,
    /// Utilization as a fraction in [0.0, 1.0].
    pub usage_ratio: f32,
    pub resets_at: Option<String>,
}

/// Cached OpenAI/Codex usage snapshot for info widgets.
#[derive(Debug, Clone, Default)]
pub struct OpenAIUsageData {
    pub five_hour: Option<OpenAIUsageWindow>,
    pub seven_day: Option<OpenAIUsageWindow>,
    pub spark: Option<OpenAIUsageWindow>,
    pub hard_limit_reached: bool,
    pub fetched_at: Option<Instant>,
    pub last_error: Option<String>,
}

impl OpenAIUsageData {
    pub fn age_ms(&self) -> Option<u128> {
        self.fetched_at.map(|t| t.elapsed().as_millis())
    }

    pub fn freshness_state(&self) -> &'static str {
        if self.fetched_at.is_none() {
            "unknown"
        } else if self.is_stale() {
            "stale"
        } else {
            "fresh"
        }
    }

    /// Whether *any* reported window is spent.
    ///
    /// Same disjunction as [`UsageData::exhausted`] and for the same reason: a
    /// provider refuses the request as soon as one window fills, so requiring
    /// every window to be full reports a blocked account as usable. See
    /// ADR-0016.
    ///
    /// Only windows the provider actually reported are considered, so a missing
    /// window is not read as either full or free.
    pub fn exhausted(&self) -> bool {
        if self.hard_limit_reached {
            return true;
        }

        if !self.has_limits() {
            return false;
        }

        [self.five_hour.as_ref(), self.seven_day.as_ref()]
            .into_iter()
            .flatten()
            .any(|window| window.usage_ratio >= UsageData::EXHAUSTED_RATIO)
    }

    /// When the window that is actually blocking will reset, preferring a spent
    /// window's reset over an unspent one's. Same reasoning as
    /// [`UsageData::blocking_reset_at`].
    pub fn blocking_reset_at(&self) -> Option<String> {
        let windows = [
            self.five_hour.as_ref(),
            self.seven_day.as_ref(),
            self.spark.as_ref(),
        ];

        windows
            .into_iter()
            .flatten()
            .filter(|window| window.usage_ratio >= UsageData::EXHAUSTED_RATIO)
            .find_map(|window| window.resets_at.clone())
            .or_else(|| {
                windows
                    .into_iter()
                    .flatten()
                    .find_map(|window| window.resets_at.clone())
            })
    }

    pub fn diagnostic_fields(&self) -> String {
        let fmt_ratio = |window: Option<&OpenAIUsageWindow>| {
            window
                .map(|w| format!("{:.1}%", w.usage_ratio * 100.0))
                .unwrap_or_else(|| "unknown".to_string())
        };

        format!(
            "freshness={} age_ms={} exhausted={} hard_limit_reached={} has_limits={} five_hour={} seven_day={} spark={} last_error={}",
            self.freshness_state(),
            self.age_ms()
                .map(|age| age.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            self.exhausted(),
            self.hard_limit_reached,
            self.has_limits(),
            fmt_ratio(self.five_hour.as_ref()),
            fmt_ratio(self.seven_day.as_ref()),
            fmt_ratio(self.spark.as_ref()),
            self.last_error.as_deref().unwrap_or("none")
        )
    }

    pub fn is_stale(&self) -> bool {
        if usage_reset_passed([
            self.five_hour.as_ref().and_then(|w| w.resets_at.as_deref()),
            self.seven_day.as_ref().and_then(|w| w.resets_at.as_deref()),
            self.spark.as_ref().and_then(|w| w.resets_at.as_deref()),
        ]) {
            return true;
        }

        match self.fetched_at {
            Some(t) => {
                let ttl = if self.is_rate_limited() {
                    RATE_LIMIT_BACKOFF
                } else if self.last_error.is_some() {
                    ERROR_BACKOFF
                } else {
                    CACHE_DURATION
                };
                t.elapsed() > ttl
            }
            None => true,
        }
    }

    fn is_rate_limited(&self) -> bool {
        self.last_error
            .as_ref()
            .map(|e| e.contains("429") || e.contains("rate limit") || e.contains("Rate limited"))
            .unwrap_or(false)
    }

    pub fn has_limits(&self) -> bool {
        self.five_hour.is_some() || self.seven_day.is_some() || self.spark.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MultiAccountProviderKind {
    Anthropic,
    OpenAI,
}

impl MultiAccountProviderKind {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic",
            Self::OpenAI => "OpenAI",
        }
    }

    pub fn switch_command(self, label: &str) -> String {
        match self {
            Self::Anthropic => format!("/account switch {}", label),
            Self::OpenAI => format!("/account openai switch {}", label),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountUsageSnapshot {
    pub label: String,
    pub email: Option<String>,
    pub exhausted: bool,
    pub primary_label: Option<String>,
    pub five_hour_ratio: Option<f32>,
    pub secondary_label: Option<String>,
    pub seven_day_ratio: Option<f32>,
    pub resets_at: Option<String>,
    pub error: Option<String>,
}

impl AccountUsageSnapshot {
    pub fn summary(&self) -> String {
        if let Some(error) = &self.error {
            return error.clone();
        }

        let mut parts = Vec::new();
        if let Some(ratio) = self.five_hour_ratio {
            parts.push(format!(
                "{} {:.0}%",
                self.primary_label.as_deref().unwrap_or("primary"),
                ratio * 100.0
            ));
        }
        if let Some(ratio) = self.seven_day_ratio {
            parts.push(format!(
                "{} {:.0}%",
                self.secondary_label.as_deref().unwrap_or("secondary"),
                ratio * 100.0
            ));
        }
        if let Some(reset) = &self.resets_at {
            parts.push(format!("resets {}", format_reset_time(reset)));
        }

        if parts.is_empty() {
            "limits unknown".to_string()
        } else {
            parts.join(", ")
        }
    }

    fn preference_score(&self) -> f32 {
        if self.error.is_some() {
            return f32::INFINITY;
        }
        self.five_hour_ratio
            .unwrap_or(0.0)
            .max(self.seven_day_ratio.unwrap_or(0.0))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountUsageProbe {
    pub provider: MultiAccountProviderKind,
    pub current_label: String,
    pub accounts: Vec<AccountUsageSnapshot>,
}

impl AccountUsageProbe {
    pub fn current_account(&self) -> Option<&AccountUsageSnapshot> {
        self.accounts
            .iter()
            .find(|account| account.label == self.current_label)
    }

    pub fn current_exhausted(&self) -> bool {
        self.current_account()
            .map(|account| account.exhausted)
            .unwrap_or(false)
    }

    pub fn has_multiple_accounts(&self) -> bool {
        self.accounts.len() > 1
    }

    pub fn best_available_alternative(&self) -> Option<&AccountUsageSnapshot> {
        if !self.current_exhausted() {
            return None;
        }

        self.accounts
            .iter()
            .filter(|account| account.label != self.current_label)
            .filter(|account| !account.exhausted && account.error.is_none())
            .min_by(|a, b| a.preference_score().total_cmp(&b.preference_score()))
    }

    pub fn all_accounts_exhausted(&self) -> bool {
        if !self.has_multiple_accounts() {
            return false;
        }
        let mut probed = self
            .accounts
            .iter()
            .filter(|account| account.error.is_none())
            .peekable();
        // No account could be probed, so nothing is known. `all()` is vacuously
        // true on an empty set, which used to turn "the usage endpoint is
        // unreachable" into "every account is exhausted": the most pessimistic
        // possible reading of no evidence, and it drives both the startup
        // warning and the retry loop's fast-fail path.
        if probed.peek().is_none() {
            return false;
        }
        probed.all(|account| account.exhausted)
    }

    pub fn switch_guidance(&self) -> Option<String> {
        let alternative = self.best_available_alternative()?;
        Some(format!(
            "Another {} account has headroom: `{}` ({}). Use `{}`.",
            self.provider.display_name(),
            alternative.label,
            alternative.summary(),
            self.provider.switch_command(&alternative.label)
        ))
    }
}

#[cfg(test)]
mod mark_stale_tests {
    use super::UsageData;

    #[test]
    fn mark_stale_forces_the_next_read_to_refetch() {
        let mut usage = UsageData {
            five_hour: 0.42,
            fetched_at: Some(std::time::Instant::now()),
            ..Default::default()
        };
        assert!(
            !usage.is_stale(),
            "a just-fetched snapshot is fresh, which is why an account switch must invalidate it explicitly"
        );

        usage.mark_stale();

        assert!(
            usage.is_stale(),
            "after an account switch the cached numbers belong to the previous account and must not be served"
        );
    }
}
