// Account-first failover, end to end through `complete_with_failover`.
//
// The contract Joel asked for, in one sentence: *whatever account a session
// starts on, every other account of that provider is tried before any other
// provider is, and when the sequence does cross to another provider it must
// land on an account of that provider which still has credit.*
//
// Both halves are exercised against the real failover loop rather than against
// a helper, because the two bugs this file guards were invisible at helper
// level: `same_provider_account_candidates` returned the right list all along,
// and the loop simply never called it for a provider it was crossing *into*.

/// Provider stub whose answer depends on which account is currently active.
///
/// The existing `StubExternalRuntime` fails or succeeds unconditionally, so it
/// cannot express "this account is spent, that one is funded" - which is the
/// only thing these tests are about. Every `complete()` records the account it
/// was called with, so a test can assert the exact ladder that was climbed
/// (`["claude-1", "claude-2"]`) instead of merely that the call eventually
/// succeeded.
struct AccountAwareStub {
    name: &'static str,
    provider_label: &'static str,
    models: &'static [&'static str],
    /// Which provider's account store this runtime reads.
    provider: ActiveProvider,
    /// Labels that answer with a usage-limit error.
    spent: Vec<String>,
    /// Every account this runtime was asked to serve, in order, as
    /// `provider_key:label`.
    attempts: Arc<std::sync::Mutex<Vec<String>>>,
    model: std::sync::RwLock<String>,
    credential_mode: std::sync::RwLock<jcode_provider_core::CredentialMode>,
}

impl AccountAwareStub {
    fn new(
        provider: ActiveProvider,
        spent: &[&str],
        attempts: Arc<std::sync::Mutex<Vec<String>>>,
    ) -> Self {
        let (name, provider_label, models): (_, _, &'static [&'static str]) = match provider {
            ActiveProvider::Claude => ("anthropic", "Anthropic", anthropic::AVAILABLE_MODELS),
            ActiveProvider::OpenAI => ("openai", "OpenAI", ALL_OPENAI_MODELS),
            other => panic!("AccountAwareStub only models the multi-account providers, got {other:?}"),
        };
        Self {
            name,
            provider_label,
            models,
            provider,
            spent: spent.iter().map(|label| (*label).to_string()).collect(),
            attempts,
            model: std::sync::RwLock::new(models[0].to_string()),
            credential_mode: std::sync::RwLock::new(jcode_provider_core::CredentialMode::Auto),
        }
    }
}

#[async_trait::async_trait]
impl Provider for AccountAwareStub {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        let label = account_failover::active_account_label_for_provider(self.provider)
            .unwrap_or_else(|| "<none>".to_string());
        self.attempts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(format!(
                "{}:{}",
                MultiProvider::provider_key(self.provider),
                label
            ));
        if self.spent.iter().any(|spent| spent == &label) {
            // Shaped like the real thing: a 429 whose text the failover
            // classifier recognises as retryable, and which
            // `error_looks_like_usage_limit` also matches.
            anyhow::bail!(
                "429 Too Many Requests: rate_limit_error - usage limit reached for account {label}"
            );
        }
        Ok(Box::pin(futures::stream::empty()))
    }
    fn name(&self) -> &'static str {
        self.name
    }
    fn model(&self) -> String {
        self.model
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
    fn set_model(&self, model: &str) -> anyhow::Result<()> {
        let trimmed = model.trim();
        if !self.models.contains(&trimmed) {
            anyhow::bail!("Unsupported {} model '{}'", self.provider_label, trimmed);
        }
        *self
            .model
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = trimmed.to_string();
        Ok(())
    }
    fn available_models(&self) -> Vec<&'static str> {
        self.models.to_vec()
    }
    fn available_models_display(&self) -> Vec<String> {
        self.models.iter().map(|model| model.to_string()).collect()
    }
    fn available_models_for_switching(&self) -> Vec<String> {
        self.available_models_display()
    }
    fn model_routes(&self) -> Vec<ModelRoute> {
        self.available_models_display()
            .into_iter()
            .map(|model| ModelRoute {
                model,
                provider: self.provider_label.to_string(),
                api_method: "https".to_string(),
                available: true,
                detail: String::new(),
                cheapness: None,
            })
            .collect()
    }
    fn credential_mode(&self) -> jcode_provider_core::CredentialMode {
        *self
            .credential_mode
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
    fn set_credential_mode(&self, mode: jcode_provider_core::CredentialMode) -> anyhow::Result<()> {
        *self
            .credential_mode
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = mode;
        Ok(())
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(AccountAwareStub::new(
            self.provider,
            &self
                .spent
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            self.attempts.clone(),
        ))
    }
}

/// Write the failover settings into the test's own `JCODE_HOME` instead of
/// relying on struct defaults, so a default change cannot silently turn these
/// tests into no-ops, and invalidate the process config cache so the new file
/// is what the loop reads.
fn write_account_first_failover_config() {
    let home = std::env::var_os("JCODE_HOME").expect("test JCODE_HOME");
    let path = std::path::PathBuf::from(home).join("config.toml");
    std::fs::write(
        &path,
        "[provider]\n\
         cross_provider_failover = \"automatic\"\n\
         same_provider_account_failover = true\n\
         prefer_account_failover = true\n",
    )
    .expect("write test config");
    crate::config::invalidate_config_cache();
}

/// Forget every piece of process-global failover memory.
///
/// `RATE_LIMITED_ACCOUNTS`, the per-account provider-unavailability map and the
/// model-unavailability map all outlive a single test, and each of these tests
/// deliberately *creates* entries in them. Without this reset the second test to
/// run sees the first test's 429 for `claude-1`, skips the account entirely and
/// records a different (still passing, still wrong) ladder - a clean, plausible,
/// wrong answer that hid the real ordering.
fn reset_failover_memory() {
    crate::provider::clear_all_account_cooldowns();
    crate::provider::models::clear_all_provider_unavailability_for_tests();
    crate::provider::clear_all_model_unavailability_for_account();
    crate::usage::clear_anthropic_account_usage_for_tests();
}

/// Seed `labels` as Anthropic OAuth accounts and make `active` the active one.
fn seed_anthropic_accounts(labels: &[&str], active: &str) {
    for label in labels {
        crate::auth::claude::upsert_account(crate::auth::claude::AnthropicAccount {
            label: (*label).to_string(),
            access: format!("test-access-{label}"),
            refresh: format!("test-refresh-{label}"),
            expires: chrono::Utc::now().timestamp_millis() + 86_400_000,
            email: Some(format!("{label}@example.test")),
            subscription_type: Some("max".to_string()),
            scopes: Vec::new(),
        })
        .expect("seed anthropic account");
    }
    crate::auth::claude::set_active_account(active).expect("select active anthropic account");
}

fn account_first_test_provider(
    active: ActiveProvider,
    anthropic_stub: Option<Arc<AccountAwareStub>>,
    openai_stub: Option<Arc<AccountAwareStub>>,
) -> MultiProvider {
    MultiProvider {
        claude: RwLock::new(None),
        anthropic: RwLock::new(anthropic_stub.map(|stub| stub as Arc<dyn Provider>)),
        openai: RwLock::new(openai_stub.map(|stub| stub as Arc<dyn Provider>)),
        copilot_api: RwLock::new(None),
        antigravity: RwLock::new(None),
        gemini: RwLock::new(None),
        cursor: RwLock::new(None),
        bedrock: RwLock::new(None),
        openrouter: RwLock::new(None),
        openai_compatible_profiles: RwLock::new(std::collections::HashMap::new()),
        active_openai_compatible_profile: RwLock::new(None),
        active: RwLock::new(active),
        use_claude_cli: false,
        startup_notices: RwLock::new(Vec::new()),
        initial_provider: None,
        routes_memo: std::sync::Mutex::new(None),
        post_auth_refreshes_pending: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    }
}

fn account_first_test_messages() -> Vec<Message> {
    vec![Message {
        role: jcode_message_types::Role::User,
        content: vec![jcode_message_types::ContentBlock::Text {
            text: "hello".to_string(),
            cache_control: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    }]
}

/// A usage probe answer built by hand, so the decision under test is driven by
/// known account states instead of a live HTTP call with fake tokens.
fn probe(
    kind: crate::usage::MultiAccountProviderKind,
    current_label: &str,
    accounts: &[(&str, bool)],
) -> crate::usage::AccountUsageProbe {
    crate::usage::AccountUsageProbe {
        provider: kind,
        current_label: current_label.to_string(),
        accounts: accounts
            .iter()
            .map(|(label, exhausted)| crate::usage::AccountUsageSnapshot {
                label: (*label).to_string(),
                email: None,
                exhausted: *exhausted,
                primary_label: Some("5h".to_string()),
                five_hour_ratio: Some(if *exhausted { 1.0 } else { 0.1 }),
                secondary_label: Some("7d".to_string()),
                seven_day_ratio: Some(if *exhausted { 1.0 } else { 0.1 }),
                resets_at: None,
                error: None,
            })
            .collect(),
    }
}

/// `block_in_place` inside the usage layer panics on a current-thread runtime,
/// so the failover path needs the multi-threaded flavour even though the test
/// itself is sequential.
fn enter_multi_thread_test_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("build multi-threaded tokio runtime")
}

fn recorded_attempts(attempts: &Arc<std::sync::Mutex<Vec<String>>>) -> Vec<String> {
    attempts
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn notices(provider: &MultiProvider) -> String {
    provider
        .startup_notices
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .join(" | ")
}

/// Case A: the session starts on a spent Anthropic account, so the funded
/// sibling account must serve the turn - without leaving Anthropic.
#[test]
fn a_spent_claude_account_rotates_to_the_other_claude_account() {
    with_clean_provider_test_env(|| {
        // Env guard first, then the cooldown guard. That order is not a
        // preference: `a_spent_provider_is_skipped_whichever_provider_it_is` and
        // its neighbours already take the two locks this way, so acquiring them
        // in the opposite order here deadlocked the whole `provider::` suite -
        // every test sat at "running for over 60 seconds" and `--lib` never
        // finished. Cross-test state is handled by `reset_failover_memory`, not
        // by widening the lock.
        let _cooldown_guard = crate::provider::account_failover::lock_account_cooldown_tests();
        reset_failover_memory();
        write_account_first_failover_config();
        seed_anthropic_accounts(&["claude-1", "claude-2"], "claude-1");

        let attempts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let anthropic = Arc::new(AccountAwareStub::new(
            ActiveProvider::Claude,
            &["claude-1"],
            attempts.clone(),
        ));
        let provider =
            account_first_test_provider(ActiveProvider::Claude, Some(anthropic), None);

        let runtime = enter_multi_thread_test_runtime();
        let messages = account_first_test_messages();
        let result = crate::provider::account_failover::with_account_usage_probe_override(
            vec![(
                ActiveProvider::Claude,
                probe(
                    crate::usage::MultiAccountProviderKind::Anthropic,
                    "claude-1",
                    &[("claude-1", true), ("claude-2", false)],
                ),
            )],
            || {
                runtime.block_on(provider.complete_with_failover(
                    &messages,
                    &[],
                    CompletionMode::Unified { system: "" },
                    None,
                ))
            },
        );

        assert!(
            result.is_ok(),
            "claude-2 has credit, so the turn must be served: {:?}",
            result.err().map(|err| err.to_string())
        );
        assert_eq!(
            recorded_attempts(&attempts),
            vec!["claude:claude-1".to_string(), "claude:claude-2".to_string()],
            "the spent account is tried once, then the sibling account - no third \
             request and no return trip"
        );
        assert_eq!(
            crate::auth::claude::active_account_label().as_deref(),
            Some("claude-2"),
            "the session must stay on the account that actually served the turn"
        );
        assert_eq!(
            provider.active_provider(),
            ActiveProvider::Claude,
            "an account rotation is not a provider change"
        );
        let notices = notices(&provider);
        assert!(
            notices.contains("claude-1") && notices.contains("claude-2"),
            "the user must be told which account is now serving, got: {notices}"
        );

        reset_failover_memory();
    });
}

/// Case B: the session starts on a spent OpenAI account and crosses to
/// Anthropic, whose *stored* active account is also spent.
///
/// The bug: `complete_with_failover` only ran the account ladder for the
/// provider it was already on (`if candidate == active`). Crossing into
/// Anthropic therefore tried exactly one account - whichever `auth.json` last
/// recorded as active - and, finding it spent, walked on to Copilot, Gemini,
/// Cursor and finally reported "No tokens/providers left" while a funded
/// `claude-2` sat one rotation away.
///
/// This is precisely the case Joel described: "if it switches to claude-1 it
/// should switch back to claude-2".
#[test]
fn crossing_from_openai_lands_on_a_claude_account_with_credit() {
    with_clean_provider_test_env(|| {
        // Env guard first, then the cooldown guard. That order is not a
        // preference: `a_spent_provider_is_skipped_whichever_provider_it_is` and
        // its neighbours already take the two locks this way, so acquiring them
        // in the opposite order here deadlocked the whole `provider::` suite -
        // every test sat at "running for over 60 seconds" and `--lib` never
        // finished. Cross-test state is handled by `reset_failover_memory`, not
        // by widening the lock.
        let _cooldown_guard = crate::provider::account_failover::lock_account_cooldown_tests();
        reset_failover_memory();
        write_account_first_failover_config();
        save_test_openai_oauth_credentials();
        let openai_label = crate::auth::codex::primary_account_label();
        // Deliberately the spent account: the stored choice must not decide the
        // outcome.
        seed_anthropic_accounts(&["claude-1", "claude-2"], "claude-1");

        let attempts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let openai = Arc::new(AccountAwareStub::new(
            ActiveProvider::OpenAI,
            &[openai_label.as_str()],
            attempts.clone(),
        ));
        let anthropic = Arc::new(AccountAwareStub::new(
            ActiveProvider::Claude,
            &["claude-1"],
            attempts.clone(),
        ));
        let provider = account_first_test_provider(
            ActiveProvider::OpenAI,
            Some(anthropic),
            Some(openai),
        );

        let runtime = enter_multi_thread_test_runtime();
        let messages = account_first_test_messages();
        let result = crate::provider::account_failover::with_account_usage_probe_override(
            vec![
                (
                    ActiveProvider::Claude,
                    probe(
                        crate::usage::MultiAccountProviderKind::Anthropic,
                        "claude-1",
                        &[("claude-1", true), ("claude-2", false)],
                    ),
                ),
                (
                    ActiveProvider::OpenAI,
                    probe(
                        crate::usage::MultiAccountProviderKind::OpenAI,
                        openai_label.as_str(),
                        // Not pre-marked as spent: this test wants the real
                        // request to happen and the real 429 to drive the
                        // cross-provider step, which is what Joel observed.
                        &[(openai_label.as_str(), false)],
                    ),
                ),
            ],
            || {
                runtime.block_on(provider.complete_with_failover(
                    &messages,
                    &[],
                    CompletionMode::Unified { system: "" },
                    None,
                ))
            },
        );

        assert!(
            result.is_ok(),
            "claude-2 has credit, so crossing providers must succeed rather than \
             report no usable provider: {:?}",
            result.err().map(|err| err.to_string())
        );

        let recorded = recorded_attempts(&attempts);
        assert_eq!(
            recorded,
            vec![
                format!("openai:{openai_label}"),
                "claude:claude-1".to_string(),
                "claude:claude-2".to_string(),
            ],
            "OpenAI is tried once, then Anthropic's stored account, then its \
             funded sibling. Stopping after claude-1 is the bug."
        );
        assert_eq!(
            provider.active_provider(),
            ActiveProvider::Claude,
            "the session must follow the turn onto the provider that served it"
        );
        assert_eq!(
            crate::auth::claude::active_account_label().as_deref(),
            Some("claude-2"),
            "and onto the account that served it, so the next turn does not \
             rediscover claude-1's limit"
        );

        reset_failover_memory();
    });
}
