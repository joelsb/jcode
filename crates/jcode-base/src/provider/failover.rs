use super::*;
use jcode_provider_core::{FailoverDecision, ProviderFailoverPrompt};

impl MultiProvider {
    /// Whether crossing to another provider must stop for client confirmation.
    ///
    /// Automatic mode is the account-first contract: the caller has already
    /// tried every eligible account on the active provider, so the next
    /// configured provider is the continuation, not a separate user decision.
    pub(super) fn cross_provider_failover_requires_prompt(
        mode: crate::config::CrossProviderFailoverMode,
    ) -> bool {
        !matches!(mode, crate::config::CrossProviderFailoverMode::Automatic)
    }

    pub(super) fn provider_is_configured(&self, provider: ActiveProvider) -> bool {
        self.reconcile_auth_if_provider_missing(provider)
    }

    pub(super) fn provider_precheck_unavailable_reason(
        &self,
        provider: ActiveProvider,
    ) -> Option<String> {
        // Anthropic keeps its own aggregate check (five-hour + seven-day ratios
        // from the session usage snapshot), which predates the per-account
        // probe and covers the single-account case.
        if provider == ActiveProvider::Claude && self.is_claude_usage_exhausted() {
            return Some(crate::provider::account_failover::usage_exhausted_reason(
                provider,
            ));
        }

        // Every other provider is checked through the same per-account usage
        // probe, so "this provider has no account with credit left" is decided
        // by one rule rather than a per-provider branch. A provider that gains
        // a probe later starts being skipped here with no change to this
        // function; one without a probe reports `false` and is still attempted,
        // which is the correct answer for "we cannot know until we ask".
        //
        // Without this, exhaustion was a Claude-only concept: a spent OpenAI
        // account was never recognised as spent, so the loop could not move
        // past it to the next provider that still had credit.
        if crate::provider::account_failover::all_accounts_quota_exhausted(provider) {
            return Some(crate::provider::account_failover::usage_exhausted_reason(
                provider,
            ));
        }

        None
    }

    pub(super) fn build_failover_prompt(
        &self,
        from: ActiveProvider,
        to: ActiveProvider,
        reason: String,
        estimated_input_chars: usize,
        estimated_input_tokens: usize,
    ) -> ProviderFailoverPrompt {
        ProviderFailoverPrompt {
            from_provider: Self::provider_key(from).to_string(),
            from_label: Self::provider_label(from).to_string(),
            to_provider: Self::provider_key(to).to_string(),
            to_label: Self::provider_label(to).to_string(),
            reason,
            estimated_input_chars,
            estimated_input_tokens,
        }
    }

    pub(super) fn fallback_sequence(active: ActiveProvider) -> Vec<ActiveProvider> {
        jcode_provider_core::fallback_sequence(active)
    }

    pub(super) fn summarize_error(err: &anyhow::Error) -> String {
        err.to_string()
            .lines()
            .next()
            .unwrap_or("unknown error")
            .trim()
            .to_string()
    }

    pub(super) fn classify_failover_error(err: &anyhow::Error) -> FailoverDecision {
        jcode_provider_core::classify_failover_error_message(&err.to_string())
    }

    pub(super) fn additional_no_provider_guidance(&self) -> Vec<String> {
        // Ask every provider in the fallback order, not a hard-coded pair, so a
        // provider that gains multi-account support is reported without editing
        // this list. `account_switch_guidance` yields `None` for providers with
        // no account identity, so the filter does the selection.
        Self::fallback_sequence(self.active_provider())
            .into_iter()
            .filter_map(crate::provider::account_failover::account_switch_guidance)
            .collect()
    }

    pub(super) fn no_provider_available_error(&self, notes: &[String]) -> anyhow::Error {
        let mut msg = "No tokens/providers left: no usable provider right now. Anthropic/OpenAI usage may be exhausted and GitHub Copilot is not authenticated or currently unavailable.".to_string();
        if !notes.is_empty() {
            msg.push_str(" Details: ");
            msg.push_str(&notes.join(" | "));
        }
        let extra_guidance = self.additional_no_provider_guidance();
        if !extra_guidance.is_empty() {
            msg.push(' ');
            msg.push_str(&extra_guidance.join(" "));
        }
        msg.push_str(" Use `/usage` to check limits and `/login <provider>` to re-authenticate.");
        anyhow::anyhow!(msg)
    }
}
