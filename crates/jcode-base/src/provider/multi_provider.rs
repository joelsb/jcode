use super::*;
use anyhow::Result;

impl MultiProvider {
    /// Make `provider` the session's active provider after its credential was
    /// re-pointed at a different account, returning the model now in force.
    ///
    /// Switching an account only changes *which credential* a provider slot
    /// uses. It says nothing about which slot the next turn is routed to, so
    /// `/account openai switch openai-1` from a Claude session used to leave the
    /// session on Claude: the turn went straight back to the rate-limited
    /// Anthropic account and 429'd again, while the UI had just reported a
    /// successful switch to OpenAI. Picking an account is a statement about
    /// where work should run, so activate the provider that owns it.
    ///
    /// Returns `Ok(None)` when the provider was already active (nothing to do)
    /// so callers can stay quiet rather than announce a no-op switch.
    ///
    /// Deliberately does *not* run `auto_select_multi_account_for_provider`,
    /// unlike [`Self::switch_active_provider_to`]. That helper re-picks the
    /// account by usage probe, which would immediately overrule the account the
    /// user just chose by hand.
    pub(super) fn activate_provider_for_account_switch_inner(
        &self,
        provider_key: &str,
    ) -> Result<Option<String>> {
        let target = Self::parse_provider_hint(provider_key)
            .ok_or_else(|| anyhow::anyhow!("Unknown provider `{}`", provider_key))?;
        if self.active_provider() == target {
            return Ok(None);
        }
        if !self.provider_is_configured(target) {
            anyhow::bail!(
                "Provider `{}` is not configured in this session",
                Self::provider_key(target)
            );
        }
        self.set_active_provider(target);
        Ok(Some(self.model()))
    }

    pub(super) async fn try_same_provider_account_failover(
        &self,
        provider: ActiveProvider,
        messages: &[Message],
        tools: &[ToolDefinition],
        mode: CompletionMode<'_>,
        initial_reason: &str,
        notes: &mut Vec<String>,
    ) -> Result<Option<EventStream>> {
        if !same_provider_account_failover_enabled() {
            return Ok(None);
        }

        let original_label = active_account_label_for_provider(provider);
        let Some(original_label) = original_label else {
            return Ok(None);
        };

        let alternatives = same_provider_account_candidates(provider);
        if alternatives.is_empty() {
            return Ok(None);
        }

        let provider_key = Self::provider_key(provider);
        let provider_label = Self::provider_label(provider);

        for alternative_label in &alternatives {
            crate::logging::info(&format!(
                "Same-provider failover{}: retrying {} using account '{}'",
                mode.log_suffix(),
                provider_label,
                alternative_label
            ));

            set_account_override_for_provider(provider, Some(alternative_label.clone()));
            clear_provider_unavailable_for_account(provider_key);
            if provider == ActiveProvider::OpenAI {
                clear_all_model_unavailability_for_account();
            }
            self.invalidate_provider_credentials_for_account_switch(provider)
                .await;

            let attempt = match mode {
                CompletionMode::Unified { system } => {
                    self.complete_on_provider(provider, messages, tools, system, None)
                        .await
                }
                CompletionMode::Split {
                    system_static,
                    system_dynamic,
                } => {
                    self.complete_split_on_provider(
                        provider,
                        messages,
                        tools,
                        system_static,
                        system_dynamic,
                        None,
                    )
                    .await
                }
            };

            match attempt {
                Ok(stream) => {
                    self.startup_notices
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(format!(
                        "⚡ Auto-switched {} account: {} → {}. To turn this off, set `[provider].same_provider_account_failover = false` in `~/.jcode/config.toml` or export `JCODE_SAME_PROVIDER_ACCOUNT_FAILOVER=false`.",
                        provider_label, original_label, alternative_label
                    ));
                    return Ok(Some(stream));
                }
                Err(err) => {
                    let summary =
                        maybe_annotate_limit_summary(provider, Self::summarize_error(&err));
                    let decision = Self::classify_failover_error(&err);
                    crate::logging::info(&format!(
                        "Same-provider account {} failed{}: {} (failover={} decision={})",
                        alternative_label,
                        mode.log_suffix(),
                        summary,
                        decision.should_failover(),
                        decision.as_str()
                    ));
                    notes.push(format!(
                        "{} account {}: {}",
                        provider_label, alternative_label, summary
                    ));
                    if decision.should_mark_provider_unavailable() {
                        record_provider_unavailable_for_account(provider_key, &summary);
                    }
                }
            }
        }

        set_account_override_for_provider(provider, Some(original_label));
        self.invalidate_provider_credentials_for_account_switch(provider)
            .await;
        if provider == ActiveProvider::OpenAI {
            clear_all_model_unavailability_for_account();
        }

        crate::logging::info(&format!(
            "Same-provider failover{} exhausted all alternate {} accounts after: {}",
            mode.log_suffix(),
            provider_label,
            initial_reason
        ));

        Ok(None)
    }
}
