use super::*;
use crate::provider::models::{ensure_model_allowed_for_subscription, filtered_display_models};

fn with_clean_provider_test_env<T>(f: impl FnOnce() -> T) -> T {
    let _guard = crate::storage::lock_test_env();
    // Concrete provider runtimes live downstream (jcode-provider-*-runtime),
    // so base tests register shared stubs through the same composition-root
    // registry the binary uses. Registration is idempotent (last write wins),
    // and per-test overrides can re-register a different stub.
    register_test_external_runtimes();
    let temp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("JCODE_HOME");
    let prev_subscription =
        std::env::var_os(crate::subscription_catalog::JCODE_SUBSCRIPTION_ACTIVE_ENV);
    let mut profile_env_keys = vec![
        "OPENROUTER_API_KEY",
        "DEEPSEEK_API_KEY",
        "KIMI_API_KEY",
        "JCODE_OPENROUTER_API_BASE",
        "JCODE_OPENROUTER_API_KEY_NAME",
        "JCODE_OPENROUTER_ENV_FILE",
        "JCODE_OPENROUTER_CACHE_NAMESPACE",
        "JCODE_OPENROUTER_PROVIDER_FEATURES",
        "JCODE_OPENROUTER_TRANSPORT_STATE",
        "JCODE_OPENROUTER_ALLOW_NO_AUTH",
        "JCODE_OPENROUTER_MODEL_CATALOG",
        "JCODE_OPENROUTER_MODEL",
        "JCODE_OPENROUTER_STATIC_MODELS",
        "JCODE_OPENAI_COMPAT_API_BASE",
        "JCODE_OPENAI_COMPAT_API_KEY_NAME",
        "JCODE_OPENAI_COMPAT_ENV_FILE",
        "JCODE_OPENAI_COMPAT_DEFAULT_MODEL",
        "JCODE_OPENAI_COMPAT_LOCAL_ENABLED",
        "OPENAI_COMPAT_API_KEY",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "JCODE_RUNTIME_PROVIDER",
        "JCODE_ACTIVE_PROVIDER",
        "JCODE_INITIAL_PROVIDER_EXPLICIT",
        "JCODE_OPENAI_MODEL",
        "JCODE_NAMED_PROVIDER_PROFILE",
        "JCODE_PROVIDER_PROFILE_ACTIVE",
        "JCODE_PROVIDER_PROFILE_NAME",
    ];
    for profile in crate::provider_catalog::openai_compatible_profiles() {
        if !profile_env_keys.contains(&profile.api_key_env) {
            profile_env_keys.push(profile.api_key_env);
        }
    }
    let saved_profile_env = profile_env_keys
        .into_iter()
        .map(|key| (key, std::env::var_os(key)))
        .collect::<Vec<_>>();
    crate::env::set_var("JCODE_HOME", temp.path());
    for (key, _) in &saved_profile_env {
        crate::env::remove_var(key);
    }
    crate::subscription_catalog::clear_runtime_env();
    crate::auth::claude::set_active_account_override(None);
    crate::auth::codex::set_active_account_override(None);
    // The in-memory model catalog services are process-global; earlier tests
    // may have hydrated scopes (fixture models) that would corrupt this test's
    // known_*_model_ids() validation, and vice versa. Reset on entry and exit
    // so neither direction leaks.
    crate::provider::models::reset_model_catalog_services_for_tests();

    let result = f();

    crate::provider::models::reset_model_catalog_services_for_tests();
    crate::auth::claude::set_active_account_override(None);
    crate::auth::codex::set_active_account_override(None);
    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
    if let Some(prev_subscription) = prev_subscription {
        crate::env::set_var(
            crate::subscription_catalog::JCODE_SUBSCRIPTION_ACTIVE_ENV,
            prev_subscription,
        );
    } else {
        crate::env::remove_var(crate::subscription_catalog::JCODE_SUBSCRIPTION_ACTIVE_ENV);
    }
    for (key, value) in saved_profile_env {
        if let Some(value) = value {
            crate::env::set_var(key, value);
        } else {
            crate::env::remove_var(key);
        }
    }
    crate::subscription_catalog::clear_runtime_env();
    result
}

fn enter_test_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime")
}

#[test]
fn openai_compatible_profile_catalog_cache_is_fresh_before_soft_refresh_boundary() {
    assert!(!openai_compatible_profile_catalog_cache_is_stale(
        1_000,
        1_000 + OPENAI_COMPATIBLE_PROFILE_CATALOG_SOFT_REFRESH_SECS - 1,
    ));
}

#[test]
fn openai_compatible_profile_catalog_cache_is_stale_at_soft_refresh_boundary() {
    assert!(openai_compatible_profile_catalog_cache_is_stale(
        1_000,
        1_000 + OPENAI_COMPATIBLE_PROFILE_CATALOG_SOFT_REFRESH_SECS,
    ));
    assert!(!openai_compatible_profile_catalog_cache_is_stale(
        2_000, 1_000
    ));
}

fn with_env_var<T>(key: &str, value: &str, f: impl FnOnce() -> T) -> T {
    let prev = std::env::var_os(key);
    crate::env::set_var(key, value);
    let result = f();
    if let Some(prev) = prev {
        crate::env::set_var(key, prev);
    } else {
        crate::env::remove_var(key);
    }
    result
}

fn save_test_openai_compatible_login_config(default_model: &str) {
    let env_file = crate::provider_catalog::OPENAI_COMPAT_PROFILE.env_file;
    crate::provider_catalog::save_env_value_to_env_file(
        "JCODE_OPENAI_COMPAT_API_BASE",
        env_file,
        Some("https://example-openai-compatible.test/v1"),
    )
    .expect("save api base");
    crate::provider_catalog::save_env_value_to_env_file(
        "OPENAI_COMPAT_API_KEY",
        env_file,
        Some("sk-test-openai-compatible"),
    )
    .expect("save api key");
    crate::provider_catalog::save_env_value_to_env_file(
        "JCODE_OPENAI_COMPAT_DEFAULT_MODEL",
        env_file,
        Some(default_model),
    )
    .expect("save default model");
}

fn save_test_openrouter_model_cache(namespace: &str, source_api_base: &str, model_ids: &[&str]) {
    let jcode_home = std::env::var_os("JCODE_HOME").expect("test JCODE_HOME should be set");
    let cache_dir = std::path::PathBuf::from(jcode_home).join("cache");
    std::fs::create_dir_all(&cache_dir).expect("create model cache dir");
    let cache = jcode_provider_openrouter::DiskCache {
        cached_at: jcode_provider_openrouter::current_unix_secs().expect("current unix time"),
        source_api_base: Some(source_api_base.to_string()),
        models: model_ids
            .iter()
            .map(|id| jcode_provider_openrouter::ModelInfo {
                id: (*id).to_string(),
                name: String::new(),
                context_length: None,
                pricing: jcode_provider_openrouter::ModelPricing::default(),
                created: None,
            })
            .collect(),
    };
    let path = cache_dir.join(format!("{namespace}_models.json"));
    std::fs::write(
        path,
        serde_json::to_string(&cache).expect("serialize model cache"),
    )
    .expect("write model cache");
}

fn clear_openai_compatible_runtime_env() {
    for key in [
        "JCODE_OPENAI_COMPAT_API_BASE",
        "JCODE_OPENAI_COMPAT_API_KEY_NAME",
        "JCODE_OPENAI_COMPAT_ENV_FILE",
        "JCODE_OPENAI_COMPAT_DEFAULT_MODEL",
        "JCODE_OPENAI_COMPAT_LOCAL_ENABLED",
        "OPENAI_COMPAT_API_KEY",
        "JCODE_OPENROUTER_CACHE_NAMESPACE",
    ] {
        crate::env::remove_var(key);
    }
}

fn save_test_openai_oauth_credentials() {
    crate::auth::codex::upsert_account_from_tokens(
        &crate::auth::codex::primary_account_label(),
        "test-oauth-access-token",
        "test-oauth-refresh-token",
        None,
        Some(chrono::Utc::now().timestamp_millis() + 86_400_000),
    )
    .expect("save test OpenAI OAuth credentials");
}

fn test_multi_provider_with_openai() -> MultiProvider {
    save_test_openai_oauth_credentials();
    crate::env::set_var("OPENAI_API_KEY", "sk-test-openai-api-key");
    MultiProvider {
        claude: RwLock::new(None),
        anthropic: RwLock::new(None),
        openai: RwLock::new(Some(test_openai_runtime() as Arc<dyn Provider>)),
        copilot_api: RwLock::new(None),
        antigravity: RwLock::new(None),
        gemini: RwLock::new(None),
        cursor: RwLock::new(None),
        bedrock: RwLock::new(None),
        openrouter: RwLock::new(None),
        openai_compatible_profiles: RwLock::new(std::collections::HashMap::new()),
        active_openai_compatible_profile: RwLock::new(None),
        active: RwLock::new(ActiveProvider::OpenAI),
        use_claude_cli: false,
        startup_notices: RwLock::new(Vec::new()),
        initial_provider: None,
        routes_memo: std::sync::Mutex::new(None),
        post_auth_refreshes_pending: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    }
}

#[test]
fn openai_model_switch_prefixes_preserve_oauth_vs_api_state_space() {
    with_clean_provider_test_env(|| {
        let rt = enter_test_runtime();
        let _runtime_guard = rt.enter();
        let provider = test_multi_provider_with_openai();
        let models = known_openai_model_ids();
        let primary = models.first().expect("at least one OpenAI model").as_str();
        let alternate = models.get(1).map(String::as_str).unwrap_or(primary);

        let cases = [
            vec![
                (
                    format!("openai-api:{primary}"),
                    openai::OpenAICredentialMode::ApiKey,
                    primary,
                ),
                (
                    format!("openai-oauth:{alternate}"),
                    openai::OpenAICredentialMode::OAuth,
                    alternate,
                ),
            ],
            vec![
                (
                    format!("openai-oauth:{primary}"),
                    openai::OpenAICredentialMode::OAuth,
                    primary,
                ),
                (
                    format!("openai-api:{alternate}"),
                    openai::OpenAICredentialMode::ApiKey,
                    alternate,
                ),
                (
                    format!("openai-oauth:{primary}"),
                    openai::OpenAICredentialMode::OAuth,
                    primary,
                ),
            ],
        ];

        for sequence in cases {
            for (request, expected_mode, expected_model) in sequence {
                provider
                    .set_model(&request)
                    .unwrap_or_else(|err| panic!("switch {request} should succeed: {err}"));
                assert_eq!(
                    provider.active_provider(),
                    ActiveProvider::OpenAI,
                    "{request}"
                );
                assert_eq!(provider.model(), expected_model, "{request}");
                assert_eq!(
                    provider
                        .openai_provider()
                        .expect("OpenAI provider")
                        .credential_mode(),
                    expected_mode,
                    "{request}"
                );
            }
        }
    });
}

#[test]
fn openai_model_route_roundtrip_preserves_auth_method_for_model_switches() {
    with_clean_provider_test_env(|| {
        let rt = enter_test_runtime();
        let _runtime_guard = rt.enter();
        let provider = test_multi_provider_with_openai();
        let models = known_openai_model_ids();
        let primary = models.first().expect("at least one OpenAI model").as_str();
        let alternate = models.get(1).map(String::as_str).unwrap_or(primary);

        // This mirrors the /model picker path: the selected ModelRoute becomes a
        // default/session model + provider key, then a future /model switch uses
        // that persisted provider key to reconstruct the provider-prefixed
        // request. The important invariant is that OpenAI OAuth and API key are
        // distinct states even though both execute in ActiveProvider::OpenAI.
        let route_cases = [
            (
                primary,
                "openai-oauth",
                "openai-oauth",
                "openai-oauth",
                openai::OpenAICredentialMode::OAuth,
            ),
            (
                alternate,
                "openai-api-key",
                "openai-api",
                "openai-api",
                openai::OpenAICredentialMode::ApiKey,
            ),
            (
                primary,
                "openai-api",
                "openai-api",
                "openai-api",
                openai::OpenAICredentialMode::ApiKey,
            ),
        ];

        for (bare_model, api_method, expected_provider_key, expected_prefix, expected_mode) in
            route_cases
        {
            let selection =
                MultiProvider::default_model_selection_from_route(bare_model, api_method, "OpenAI");
            assert_eq!(
                selection.model_spec,
                format!("{expected_prefix}:{bare_model}")
            );
            assert_eq!(
                selection.provider_key.as_deref(),
                Some(expected_provider_key)
            );

            let request = MultiProvider::model_switch_request_for_session_model(
                bare_model,
                selection.provider_key.as_deref(),
            );
            assert_eq!(request, format!("{expected_prefix}:{bare_model}"));

            provider
                .set_model(&request)
                .unwrap_or_else(|err| panic!("/model switch {request} should succeed: {err}"));
            assert_eq!(
                provider.active_provider(),
                ActiveProvider::OpenAI,
                "{request}"
            );
            assert_eq!(provider.model(), bare_model, "{request}");
            assert_eq!(
                provider
                    .openai_provider()
                    .expect("OpenAI provider")
                    .credential_mode(),
                expected_mode,
                "{request}"
            );
        }
    });
}

#[test]
fn active_explicit_credential_reflects_openai_switch_immediately_and_none_for_auto() {
    use jcode_provider_core::{Provider, ResolvedCredential};
    with_clean_provider_test_env(|| {
        let rt = enter_test_runtime();
        let _runtime_guard = rt.enter();
        let provider = test_multi_provider_with_openai();
        let model = known_openai_model_ids()
            .first()
            .expect("at least one OpenAI model")
            .clone();

        // Fresh provider with both credentials present defaults to auto, which
        // has no explicit pin: the info widget must fall back to its cached
        // heuristic instead of asserting an OAuth-vs-API choice the user never
        // made.
        assert_eq!(
            provider.active_explicit_credential(),
            None,
            "auto mode must not report an explicit pin"
        );

        // Switching to the API-key route pins the credential in memory, so the
        // widget must report API key on the very next read with no cache delay.
        provider
            .set_model(&format!("openai-api:{model}"))
            .expect("switch to OpenAI API key");
        assert_eq!(
            provider.active_explicit_credential(),
            Some(ResolvedCredential::ApiKey),
            "explicit API-key switch must be visible immediately"
        );

        // Switching back to OAuth flips it back just as immediately.
        provider
            .set_model(&format!("openai-oauth:{model}"))
            .expect("switch to OpenAI OAuth");
        assert_eq!(
            provider.active_explicit_credential(),
            Some(ResolvedCredential::Oauth),
            "explicit OAuth switch must be visible immediately"
        );
    });
}

#[test]
fn openai_model_routes_cover_oauth_api_and_no_auth_state_space() {
    with_clean_provider_test_env(|| {
        let rt = enter_test_runtime();
        let _runtime_guard = rt.enter();
        let model = known_openai_model_ids()
            .first()
            .expect("at least one OpenAI model")
            .clone();

        let provider = test_multi_provider_with_openai();
        let routes = provider.model_routes();
        let methods = routes
            .iter()
            .filter(|route| route.provider == "OpenAI" && route.model == model)
            .map(|route| (route.api_method.as_str(), route.available))
            .collect::<Vec<_>>();
        assert!(
            methods.contains(&("openai-oauth", true)),
            "routes: {methods:?}"
        );
        assert!(
            methods.contains(&("openai-api-key", true)),
            "routes: {methods:?}"
        );
        let web_routes = routes
            .iter()
            .filter(|route| route.model == CHATGPT_WEB_MODEL)
            .collect::<Vec<_>>();
        assert_eq!(web_routes.len(), 1, "routes: {web_routes:?}");
        assert_eq!(web_routes[0].api_method, "chatgpt-web");
        assert!(web_routes[0].available);

        crate::env::remove_var("OPENAI_API_KEY");
        crate::auth::AuthStatus::invalidate_cache();
        let oauth_only = provider.model_routes();
        let oauth_only_methods = oauth_only
            .iter()
            .filter(|route| route.provider == "OpenAI" && route.model == model)
            .map(|route| route.api_method.as_str())
            .collect::<Vec<_>>();
        assert_eq!(oauth_only_methods, vec!["openai-oauth"]);

        crate::env::set_var("OPENAI_API_KEY", "sk-test-openai-api-key");
        std::fs::remove_file(
            crate::storage::jcode_dir()
                .unwrap()
                .join("openai-auth.json"),
        )
        .expect("remove oauth credentials");
        crate::auth::AuthStatus::invalidate_cache();
        let api_only = provider.model_routes();
        let api_only_methods = api_only
            .iter()
            .filter(|route| route.provider == "OpenAI" && route.model == model)
            .map(|route| route.api_method.as_str())
            .collect::<Vec<_>>();
        assert_eq!(api_only_methods, vec!["openai-api-key"]);
    });
}

/// The route-catalog memo must serve repeated `model_routes()` calls without
/// rebuilding (a shared server fans one ModelsUpdated event out to every
/// connection, each of which snapshots the catalog), while auth invalidation
/// and model switches must bypass it immediately.
#[test]
fn model_routes_memo_serves_repeats_and_invalidates_on_auth_and_model_changes() {
    with_clean_provider_test_env(|| {
        let rt = enter_test_runtime();
        let _runtime_guard = rt.enter();
        let provider = test_multi_provider_with_openai();

        let first = provider.model_routes();
        let second = provider.model_routes();
        assert_eq!(
            first, second,
            "memoized catalog must be identical across back-to-back reads"
        );
        assert!(
            provider
                .routes_memo
                .lock()
                .expect("routes memo lock")
                .is_some(),
            "memo should be populated after a build"
        );

        // Auth invalidation bumps the pricing generation, which must make the
        // memo stale even within the TTL window (verified behaviorally by the
        // oauth/api state-space test above; here we check the memo mechanism).
        let generation_before = crate::provider::pricing::auth_pricing_generation();
        crate::auth::AuthStatus::invalidate_cache();
        assert!(
            crate::provider::pricing::auth_pricing_generation() > generation_before,
            "auth invalidation must advance the pricing generation"
        );

        // A model switch drops the memo outright.
        let model = known_openai_model_ids()
            .first()
            .expect("at least one OpenAI model")
            .clone();
        provider.model_routes();
        let _ = provider.set_model(&format!("openai-api:{model}"));
        assert!(
            provider
                .routes_memo
                .lock()
                .expect("routes memo lock")
                .is_none(),
            "set_model must invalidate the routes memo"
        );

        // A second instance with identical catalog-relevant state must reuse
        // the shared process-wide memo (this is what collapses shared-server
        // connect bursts down to one build), and instances with different
        // state must not share a key.
        let first_instance = test_multi_provider_with_openai();
        let second_instance = test_multi_provider_with_openai();
        assert_eq!(
            first_instance.routes_memo_key(),
            second_instance.routes_memo_key(),
            "identical forks must share a memo key"
        );
        let baseline = first_instance.model_routes();
        assert!(
            second_instance
                .routes_memo
                .lock()
                .expect("routes memo lock")
                .is_none(),
            "second instance has not built anything yet"
        );
        let shared = second_instance.model_routes();
        assert_eq!(baseline, shared, "shared memo must serve identical routes");
        assert!(
            second_instance
                .routes_memo
                .lock()
                .expect("routes memo lock")
                .is_some(),
            "shared hit should hydrate the instance memo"
        );
        let _ = second_instance.set_model(&format!("openai-oauth:{model}"));
        // Credential-mode switches don't change catalog content (routes come
        // from global auth status), so the key may legitimately stay equal.
        // A different *model* must change it: the active model gets special
        // treatment (endpoint refresh priority) during the build.
        if let Some(alternate) = known_openai_model_ids().get(1) {
            let _ = second_instance.set_model(&format!("openai-oauth:{alternate}"));
            assert_ne!(
                first_instance.routes_memo_key(),
                second_instance.routes_memo_key(),
                "a different active model must change the memo key"
            );
        }
    });
}

fn assert_openai_compatible_route_available(provider: &MultiProvider, model: &str) {
    let routes = provider.model_routes();
    assert!(
        routes.iter().any(|route| {
            route.provider == "OpenAI-compatible"
                && matches!(
                    route.api_method.as_str(),
                    "openai-compatible" | "openai-compatible:openai-compatible"
                )
                && route.model == model
                && route.available
        }),
        "configured OpenAI-compatible model should be immediately visible after API-key setup; routes: {routes:?}"
    );
}

#[test]
fn openai_compatible_api_key_setup_makes_configured_model_route_available() {
    with_clean_provider_test_env(|| {
        save_test_openai_compatible_login_config("glm-test-login-flow");

        assert!(
            crate::provider_catalog::openai_compatible_profile_is_configured(
                crate::provider_catalog::OPENAI_COMPAT_PROFILE,
            )
        );

        let provider = MultiProvider::new();
        assert_openai_compatible_route_available(&provider, "glm-test-login-flow");

        provider
            .set_model_on_openai_compatible_profile(
                crate::provider_catalog::OPENAI_COMPAT_PROFILE,
                "glm-test-login-flow",
            )
            .expect("configured OpenAI-compatible model should select without requiring another provider login");

        assert_eq!(provider.model(), "glm-test-login-flow");
    });
}

#[test]
fn openai_compatible_api_key_setup_survives_process_restart_without_relogin() {
    with_clean_provider_test_env(|| {
        save_test_openai_compatible_login_config("restart-visible-model");

        // Simulate a fresh process: the login command wrote the config file, but
        // none of the runtime env vars from the login process remain populated.
        clear_openai_compatible_runtime_env();

        let resolved = crate::provider_catalog::resolve_openai_compatible_profile(
            crate::provider_catalog::OPENAI_COMPAT_PROFILE,
        );
        assert_eq!(
            resolved.api_base,
            "https://example-openai-compatible.test/v1"
        );
        assert_eq!(
            resolved.default_model.as_deref(),
            Some("restart-visible-model")
        );
        assert!(
            crate::provider_catalog::openai_compatible_profile_is_configured(
                crate::provider_catalog::OPENAI_COMPAT_PROFILE,
            )
        );

        let provider = MultiProvider::new();
        assert_openai_compatible_route_available(&provider, "restart-visible-model");
        provider
            .set_model_on_openai_compatible_profile(
                crate::provider_catalog::OPENAI_COMPAT_PROFILE,
                "restart-visible-model",
            )
            .expect("saved credentials should be selectable after a fresh process restart");
        assert_eq!(provider.model(), "restart-visible-model");
    });
}

#[test]
fn configured_openai_compatible_profile_routes_use_live_cache_when_not_active_provider() {
    with_clean_provider_test_env(|| {
        crate::provider_catalog::save_env_value_to_env_file(
            "OPENROUTER_API_KEY",
            "openrouter.env",
            Some("sk-test-openrouter"),
        )
        .expect("save openrouter key");
        crate::provider_catalog::save_env_value_to_env_file(
            "OPENCODE_API_KEY",
            "opencode.env",
            Some("oc-test-opencode"),
        )
        .expect("save opencode key");
        save_test_openrouter_model_cache(
            "opencode",
            "https://opencode.ai/zen/v1",
            &["kimi-k2.6", "zen-live-only-model"],
        );

        let provider = MultiProvider::new();
        let routes = provider.model_routes();
        let opencode_routes = routes
            .iter()
            .filter(|route| route.provider == "OpenCode Zen")
            .collect::<Vec<_>>();

        assert!(
            opencode_routes
                .iter()
                .any(|route| route.model == "zen-live-only-model"
                    && route.api_method == "openai-compatible:opencode"
                    && !route
                        .detail
                        .contains("fallback: static provider model list")),
            "non-active configured direct profile should expose its live /models cache, routes: {opencode_routes:?}"
        );
        assert!(
            !opencode_routes.iter().any(|route| route.model == "glm-4.7"),
            "static fallback models should drop out once a live profile catalog is available, routes: {opencode_routes:?}"
        );
    });
}

#[test]
fn standard_openrouter_catalog_refresh_is_noop_when_cache_fresh() {
    with_clean_provider_test_env(|| {
        let runtime = enter_test_runtime();
        runtime.block_on(async {
            crate::provider_catalog::save_env_value_to_env_file(
                "OPENROUTER_API_KEY",
                "openrouter.env",
                Some("sk-test-openrouter"),
            )
            .expect("save openrouter key");
            // A fresh, non-empty standard OpenRouter cache should suppress the
            // background refresh entirely so we never fire a needless network
            // request on every picker render.
            save_test_openrouter_model_cache(
                "openrouter",
                "https://openrouter.ai/api/v1",
                &["openrouter/owl-alpha"],
            );

            assert!(
                !openrouter::maybe_schedule_standard_openrouter_catalog_refresh(
                    "unit test fresh cache"
                ),
                "a fresh non-empty standard OpenRouter cache must not trigger a refresh"
            );
        });
    });
}

#[test]
fn standard_openrouter_catalog_refresh_skips_without_key() {
    with_clean_provider_test_env(|| {
        let runtime = enter_test_runtime();
        runtime.block_on(async {
            // No OPENROUTER_API_KEY configured: the refresh must not be
            // scheduled regardless of cache state.
            assert!(
                !openrouter::maybe_schedule_standard_openrouter_catalog_refresh(
                    "unit test missing key"
                ),
                "standard OpenRouter refresh must be skipped when no key is configured"
            );
        });
    });
}

#[test]
fn standard_openrouter_catalog_refresh_fires_when_named_profile_owns_slot() {
    with_clean_provider_test_env(|| {
        let runtime = enter_test_runtime();
        runtime.block_on(async {
            crate::provider_catalog::save_env_value_to_env_file(
                "OPENROUTER_API_KEY",
                "openrouter.env",
                Some("sk-test-openrouter"),
            )
            .expect("save openrouter key");
            // Simulate an active named profile (e.g. NVIDIA NIM) occupying the
            // shared OpenRouter/OpenAI-compatible slot: it sets the runtime env
            // vars to point at a non-openrouter.ai endpoint. The standard
            // OpenRouter catalog refresh must STILL fire so `/model` can list
            // openrouter.ai models (issue #292). Cache is missing -> not fresh.
            crate::env::set_var(
                "JCODE_OPENROUTER_API_BASE",
                "https://integrate.api.nvidia.com/v1",
            );
            crate::env::set_var("JCODE_OPENROUTER_CACHE_NAMESPACE", "mynvidia");

            // Other tests in this process may already have attempted (or be
            // running) an `openrouter` catalog refresh; clear the process-wide
            // backoff/in-flight tracker or this assertion is flaky under
            // parallel test execution.
            jcode_provider_openrouter_runtime::reset_profile_catalog_refresh_tracker_for_tests();

            assert!(
                openrouter::maybe_schedule_standard_openrouter_catalog_refresh(
                    "unit test named profile owns slot"
                ),
                "standard OpenRouter refresh must fire even when a named profile sets JCODE_OPENROUTER_* env"
            );
        });
    });
}

/// Parameterized test stand-in for provider runtimes that live downstream
/// (jcode-provider-{gemini,cursor,antigravity}-runtime) and therefore cannot
/// be constructed from base tests. Mirrors each runtime's catalog surface
/// (static model list plus `ModelRoute`s) so routing/fallback tests stay
/// meaningful.
struct StubExternalRuntime {
    name: &'static str,
    provider_label: &'static str,
    api_method: &'static str,
    models: &'static [&'static str],
    model: std::sync::RwLock<String>,
    credential_mode: std::sync::RwLock<jcode_provider_core::CredentialMode>,
}

impl StubExternalRuntime {
    fn new(
        name: &'static str,
        provider_label: &'static str,
        api_method: &'static str,
        models: &'static [&'static str],
    ) -> Self {
        Self {
            name,
            provider_label,
            api_method,
            models,
            model: std::sync::RwLock::new(models[0].to_string()),
            credential_mode: std::sync::RwLock::new(jcode_provider_core::CredentialMode::Auto),
        }
    }

    fn cursor() -> Self {
        Self::new("cursor", "Cursor", "cursor", cursor::AVAILABLE_MODELS)
    }

    fn antigravity() -> Self {
        Self::new(
            "antigravity",
            "Antigravity",
            "https",
            antigravity::AVAILABLE_MODELS,
        )
    }

    fn copilot() -> Self {
        Self::new(
            "copilot",
            "GitHub Copilot",
            "copilot",
            copilot::FALLBACK_MODELS,
        )
    }

    fn anthropic() -> Self {
        Self::new(
            "anthropic",
            "Anthropic",
            "https",
            anthropic::AVAILABLE_MODELS,
        )
    }

    fn openai() -> Self {
        Self::new("openai", "OpenAI", "https", ALL_OPENAI_MODELS)
    }
}

#[async_trait::async_trait]
impl Provider for StubExternalRuntime {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        anyhow::bail!("stub {} runtime does not stream", self.name)
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
        if trimmed.is_empty() {
            anyhow::bail!("{} model cannot be empty", self.provider_label);
        }
        // Mirror the real runtimes' family validation: the registry is
        // process-global, so hot-init can hand this stub to tests that expect
        // cross-provider models to be rejected (e.g. a Claude model under a
        // forced-OpenAI selection).
        if !self.models.contains(&trimmed) {
            anyhow::bail!(
                "Unsupported {} model '{}'. Use /model to choose from the models available to your account.",
                self.provider_label,
                trimmed,
            );
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
                api_method: self.api_method.to_string(),
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
        Arc::new(StubExternalRuntime::new(
            self.name,
            self.provider_label,
            self.api_method,
            self.models,
        ))
    }
}

fn test_cursor_runtime() -> Arc<dyn Provider> {
    Arc::new(StubExternalRuntime::cursor())
}

fn test_antigravity_runtime() -> Arc<dyn Provider> {
    Arc::new(StubExternalRuntime::antigravity())
}

fn test_copilot_runtime() -> Arc<dyn Provider> {
    Arc::new(StubExternalRuntime::copilot())
}

fn test_anthropic_runtime() -> Arc<StubExternalRuntime> {
    Arc::new(StubExternalRuntime::anthropic())
}

fn test_openai_runtime() -> Arc<StubExternalRuntime> {
    Arc::new(StubExternalRuntime::openai())
}

/// Register the shared external-runtime stubs for every downstream provider
/// slot base can hot-initialize. Called by `with_clean_provider_test_env` so
/// hot-init/startup tests find a runtime the way the real binary does.
fn register_test_external_runtimes() {
    external::register_external_provider(external::ANTHROPIC_RUNTIME, || {
        test_anthropic_runtime() as Arc<dyn Provider>
    });
    external::register_external_provider(external::OPENAI_RUNTIME, || {
        test_openai_runtime() as Arc<dyn Provider>
    });
    external::register_external_provider(external::CURSOR_RUNTIME, test_cursor_runtime);
    external::register_external_provider(external::ANTIGRAVITY_RUNTIME, test_antigravity_runtime);
    external::register_external_provider(external::COPILOT_RUNTIME, test_copilot_runtime);
    // OpenRouter tests exercise the real runtime (profile-scoped catalogs,
    // transport identities), so register the real factory like the binary's
    // composition root does. The dev-dependency cycle is test-only.
    external::register_openrouter_factory(|spec| {
        use external::OpenRouterRuntimeSpec;
        use jcode_provider_openrouter_runtime::OpenRouterProvider;
        let provider: Arc<dyn Provider> = match spec {
            OpenRouterRuntimeSpec::Default => Arc::new(OpenRouterProvider::new()?),
            OpenRouterRuntimeSpec::OpenRouterApiKey => {
                Arc::new(OpenRouterProvider::new_openrouter_api_key_runtime()?)
            }
            OpenRouterRuntimeSpec::CompatibleProfile(profile) => Arc::new(
                OpenRouterProvider::new_openai_compatible_profile_runtime(profile)?,
            ),
            OpenRouterRuntimeSpec::NamedProfile { name, config } => Arc::new(
                OpenRouterProvider::new_named_openai_compatible(&name, &config)?,
            ),
        };
        Ok(provider)
    });
    external::register_profile_catalog_refresh(
        jcode_provider_openrouter_runtime::maybe_schedule_openai_compatible_profile_catalog_refresh,
    );
    external::register_standard_openrouter_catalog_refresh(
        jcode_provider_openrouter_runtime::maybe_schedule_standard_openrouter_catalog_refresh,
    );
}

/// Construct a real OpenRouter/OpenAI-compatible runtime for tests through
/// the registry, mirroring production construction.
fn test_openrouter_runtime() -> anyhow::Result<Arc<dyn Provider>> {
    external::instantiate_openrouter_runtime(external::OpenRouterRuntimeSpec::Default)
}

fn test_multi_provider_with_cursor() -> MultiProvider {
    MultiProvider {
        claude: RwLock::new(None),
        anthropic: RwLock::new(None),
        openai: RwLock::new(None),
        copilot_api: RwLock::new(None),
        antigravity: RwLock::new(None),
        gemini: RwLock::new(None),
        cursor: RwLock::new(Some(test_cursor_runtime())),
        bedrock: RwLock::new(None),
        openrouter: RwLock::new(None),
        openai_compatible_profiles: RwLock::new(std::collections::HashMap::new()),
        active_openai_compatible_profile: RwLock::new(None),
        active: RwLock::new(ActiveProvider::Cursor),
        use_claude_cli: false,
        startup_notices: RwLock::new(Vec::new()),
        initial_provider: None,
        routes_memo: std::sync::Mutex::new(None),
        post_auth_refreshes_pending: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    }
}

#[test]
fn new_session_fork_reloads_changed_config_provider_and_model() {
    with_clean_provider_test_env(|| {
        let runtime = enter_test_runtime();
        runtime.block_on(async {
            crate::env::set_var("ANTHROPIC_API_KEY", "sk-ant-test");
            crate::env::set_var("OPENAI_API_KEY", "sk-openai-test");

            crate::config::Config::set_default_model(Some("claude-fable-5"), Some("anthropic-api"))
                .expect("save initial Claude default");
            let template = MultiProvider::new_fast();
            assert_eq!(template.name(), "Claude");
            assert_eq!(template.model(), "claude-fable-5");

            crate::config::Config::set_default_model(Some("gpt-5.5"), Some("openai-api"))
                .expect("save changed OpenAI default");

            let fresh = template.fork_for_new_session();
            assert_eq!(fresh.name(), "OpenAI");
            assert_eq!(fresh.model(), "gpt-5.5");
            assert_eq!(
                fresh.active_resolved_credential(),
                Some(jcode_provider_core::ResolvedCredential::ApiKey)
            );

            // Ordinary forks still preserve the existing session's selection.
            let preserved = template.fork();
            assert_eq!(preserved.name(), "Claude");
            assert_eq!(preserved.model(), "claude-fable-5");
        });
    });
}

include!("tests/auth_refresh.rs");
include!("tests/model_resolution.rs");
include!("tests/issue_534_profile_preservation.rs");
include!("tests/fallback_failover.rs");
include!("tests/account_first_failover.rs");
include!("tests/catalog_subscription.rs");

/// A MultiProvider with both the Anthropic and OpenAI slots filled, active on
/// Anthropic: the shape of a normal Claude session that also has an OpenAI
/// login available.
fn test_multi_provider_with_anthropic_and_openai() -> MultiProvider {
    save_test_openai_oauth_credentials();
    crate::env::set_var("ANTHROPIC_API_KEY", "sk-ant-test");
    crate::env::set_var("OPENAI_API_KEY", "sk-test-openai-api-key");
    MultiProvider {
        claude: RwLock::new(None),
        anthropic: RwLock::new(Some(test_anthropic_runtime() as Arc<dyn Provider>)),
        openai: RwLock::new(Some(test_openai_runtime() as Arc<dyn Provider>)),
        copilot_api: RwLock::new(None),
        antigravity: RwLock::new(None),
        gemini: RwLock::new(None),
        cursor: RwLock::new(None),
        bedrock: RwLock::new(None),
        openrouter: RwLock::new(None),
        openai_compatible_profiles: RwLock::new(std::collections::HashMap::new()),
        active_openai_compatible_profile: RwLock::new(None),
        active: RwLock::new(ActiveProvider::Claude),
        use_claude_cli: false,
        startup_notices: RwLock::new(Vec::new()),
        initial_provider: None,
        routes_memo: std::sync::Mutex::new(None),
        post_auth_refreshes_pending: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    }
}

/// Picking an OpenAI account from a Claude session must move the session onto
/// OpenAI, not merely re-point the OpenAI credential.
///
/// The reported bug: Anthropic hit its 5-hour limit, the user switched to the
/// OpenAI account, the UI printed "Switched to OpenAI account openai-1", and the
/// very next turn went to Anthropic again and returned the same 429. The account
/// store had changed; the active provider had not.
#[test]
fn switching_to_an_openai_account_routes_the_session_to_openai() {
    with_clean_provider_test_env(|| {
        let runtime = enter_test_runtime();
        let _runtime_guard = runtime.enter();
        let provider = test_multi_provider_with_anthropic_and_openai();
        assert_eq!(provider.active_provider(), ActiveProvider::Claude);

        let activated = provider
            .activate_provider_for_account_switch("openai")
            .expect("activating the OpenAI slot should succeed");

        assert!(
            activated.is_some(),
            "a switch away from Anthropic must report the model now in force so \
             the caller can tell the user where the next turn will run"
        );
        assert_eq!(
            provider.active_provider(),
            ActiveProvider::OpenAI,
            "the next turn must go to OpenAI, not back to the rate-limited \
             Anthropic account"
        );
        assert_eq!(
            activated.as_deref(),
            Some(provider.model()).as_deref(),
            "the reported model must be the one actually selected"
        );
    });
}

/// An ordinary claude-1 -> claude-2 switch stays on Anthropic and reports
/// nothing, so the UI does not announce a provider change that did not happen.
#[test]
fn switching_between_accounts_of_the_active_provider_reports_no_change() {
    with_clean_provider_test_env(|| {
        let runtime = enter_test_runtime();
        let _runtime_guard = runtime.enter();
        let provider = test_multi_provider_with_anthropic_and_openai();

        let activated = provider
            .activate_provider_for_account_switch("claude")
            .expect("staying on the active provider is not an error");

        assert!(
            activated.is_none(),
            "already on Anthropic, so there is no provider switch to announce"
        );
        assert_eq!(provider.active_provider(), ActiveProvider::Claude);
    });
}

/// Switching to a provider with no credentials must fail loudly rather than
/// silently leaving the session where it was.
///
/// Reporting success while staying put is what made the original bug invisible:
/// the user believed they had moved off the exhausted account.
#[test]
fn switching_to_an_unconfigured_provider_is_an_error() {
    with_clean_provider_test_env(|| {
        let runtime = enter_test_runtime();
        let _runtime_guard = runtime.enter();
        let provider = test_multi_provider_with_cursor();

        let error = provider
            .activate_provider_for_account_switch("openai")
            .expect_err("OpenAI is not configured in this session");

        assert!(
            error.to_string().contains("not configured"),
            "error should name the real problem, got: {error}"
        );
        assert_eq!(
            provider.active_provider(),
            ActiveProvider::Cursor,
            "a failed switch must not move the session"
        );
    });
}

/// A spent provider must be skipped whichever provider it is, not only Claude.
///
/// The exhaustion precheck used to read `match provider { Claude if ... }`, so
/// "this provider has no credit left" was a Claude-only concept. A spent OpenAI
/// account was never recognised as spent, so the fallback loop could not step
/// past it to the next provider that still had credit - which is the whole
/// point of having more than one account.
#[test]
fn a_spent_provider_is_skipped_whichever_provider_it_is() {
    with_clean_provider_test_env(|| {
        let _cooldown_guard = crate::provider::account_failover::lock_account_cooldown_tests();
        let runtime = enter_test_runtime();
        let _runtime_guard = runtime.enter();
        let provider = test_multi_provider_with_anthropic_and_openai();

        // Empty probe override = "no usage data for anybody", which is what
        // makes this test hermetic. Without it, `usage_exhausted_reason` ->
        // `account_switch_guidance` reaches the *real* probe, which issues a
        // live usage request for the fake OpenAI token and then parks forever:
        // `fetch_openai_usage_for_account` calls `block_on` inside
        // `block_in_place` on this current-thread runtime, so nothing can drive
        // the IO it is waiting for. The test never failed - it hung, holding
        // `lock_test_env`, so every other env-using test in the crate appeared
        // to hang too and `cargo test -p jcode-base --lib` never finished.
        crate::provider::account_failover::with_account_usage_probe_override(Vec::new(), || {
            for candidate in [ActiveProvider::Claude, ActiveProvider::OpenAI] {
                let reason = crate::provider::account_failover::with_quota_exhausted_override(
                    Some(true),
                    || provider.provider_precheck_unavailable_reason(candidate),
                );
                assert!(
                    reason.is_some(),
                    "{:?} reports every account spent, so it must be skipped",
                    candidate
                );
            }
        });
    });
}

/// With credit left, no provider is skipped.
///
/// The failure this guards against is worse than the bug it fixes: a precheck
/// that wrongly reports exhaustion takes a working provider out of service and
/// the turn fails with "no usable provider" while the account is fine.
#[test]
fn a_provider_with_credit_is_not_skipped() {
    with_clean_provider_test_env(|| {
        let _cooldown_guard = crate::provider::account_failover::lock_account_cooldown_tests();
        let runtime = enter_test_runtime();
        let _runtime_guard = runtime.enter();
        let provider = test_multi_provider_with_anthropic_and_openai();

        // Hermetic for the same reason as the test above: no live usage fetch.
        let reason = crate::provider::account_failover::with_account_usage_probe_override(
            Vec::new(),
            || {
                crate::provider::account_failover::with_quota_exhausted_override(Some(false), || {
                    provider.provider_precheck_unavailable_reason(ActiveProvider::OpenAI)
                })
            },
        );
        assert!(
            reason.is_none(),
            "OpenAI has headroom and must stay in the rotation, got: {:?}",
            reason
        );
    });
}

/// A provider with no account identity at all (no usage probe) is attempted
/// rather than skipped.
///
/// "We cannot know until we ask" must fail open. Failing closed would silently
/// remove every API-key provider from the chain, since none of them can report
/// per-account quota.
#[test]
fn a_provider_without_account_identity_is_still_attempted() {
    with_clean_provider_test_env(|| {
        let _cooldown_guard = crate::provider::account_failover::lock_account_cooldown_tests();
        let runtime = enter_test_runtime();
        let _runtime_guard = runtime.enter();
        let provider = test_multi_provider_with_cursor();

        assert!(
            provider
                .provider_precheck_unavailable_reason(ActiveProvider::Cursor)
                .is_none(),
            "Cursor has no per-account usage probe, so it must not be pre-emptively skipped"
        );
    });
}

/// Rendering the route catalog must never schedule network work.
///
/// Regression guard for the "spawning a session refetches every provider
/// catalog" bug: route building used to schedule a background `/models` fetch
/// for each stale or missing profile cache, so every session attach and picker
/// open fanned out dozens of HTTP requests. Refresh cadence now belongs solely
/// to the background catalog scheduler.
#[test]
fn building_direct_profile_routes_does_not_schedule_catalog_refreshes() {
    with_clean_provider_test_env(|| {
        let runtime = enter_test_runtime();
        runtime.block_on(async {
            crate::provider_catalog::save_env_value_to_env_file(
                "OPENROUTER_API_KEY",
                "openrouter.env",
                Some("sk-test-openrouter"),
            )
            .expect("save openrouter key");

            // Deliberately leave every profile cache missing/stale: under the
            // old behavior this was the worst case that scheduled a refresh
            // for each configured profile.
            jcode_provider_openrouter_runtime::reset_profile_catalog_refresh_tracker_for_tests();

            for profile in crate::provider_catalog::openai_compatible_profiles()
                .iter()
                .copied()
            {
                let _ = super::direct_openai_compatible_profile_routes(profile);
            }

            // If route building had scheduled refreshes, the profile tracker
            // would have recorded attempts, and this direct call for the
            // standard OpenRouter namespace would be throttled/in-flight.
            assert!(
                openrouter::maybe_schedule_standard_openrouter_catalog_refresh(
                    "unit test post-render scheduling"
                ),
                "route building must leave the refresh tracker untouched"
            );
        });
    });
}

/// The scheduler's staleness predicate must treat a missing or mismatched
/// cache as needing a refresh, so the sweeper actually populates cold caches.
#[test]
fn profile_catalog_cache_needs_refresh_for_missing_cache() {
    with_clean_provider_test_env(|| {
        let profile = crate::provider_catalog::openai_compatible_profiles()
            .first()
            .copied()
            .expect("at least one OpenAI-compatible profile is defined");
        assert!(
            super::catalog_scheduler::profile_catalog_cache_needs_refresh(profile),
            "a missing catalog cache must be reported as needing a refresh"
        );
    });
}

/// The rate-limit cooldown that stops account ping-pong across requests.
///
/// Reported symptom: `claude-2 → claude-1 → claude-2 → claude-1`, three switches
/// in a row, each one spending a real request to rediscover a limit the previous
/// request had already hit. `tried_accounts` lives for a single request, so
/// nothing carried that knowledge forward.
#[test]
fn a_rate_limited_account_is_remembered_across_requests() {
    let _cooldown_guard = crate::provider::account_failover::lock_account_cooldown_tests();
    use crate::provider::{
        account_is_cooling_down, clear_account_cooldown, clear_all_account_cooldowns,
        note_account_rate_limited,
    };
    use jcode_provider_core::ActiveProvider;

    clear_all_account_cooldowns();

    assert!(
        !account_is_cooling_down(ActiveProvider::Claude, "claude-1"),
        "an account nobody has seen fail must be freely usable"
    );

    note_account_rate_limited(ActiveProvider::Claude, "claude-1");
    assert!(
        account_is_cooling_down(ActiveProvider::Claude, "claude-1"),
        "the next request must remember this account just refused"
    );
    assert!(
        !account_is_cooling_down(ActiveProvider::Claude, "claude-2"),
        "one account's limit must not sideline the others"
    );

    // Cooldowns are per provider: an Anthropic limit says nothing about OpenAI.
    assert!(
        !account_is_cooling_down(ActiveProvider::OpenAI, "claude-1"),
        "cooldowns must not leak across providers"
    );

    // A success proves the account is serving again, so it should not sit out
    // the rest of the window.
    clear_account_cooldown(ActiveProvider::Claude, "claude-1");
    assert!(
        !account_is_cooling_down(ActiveProvider::Claude, "claude-1"),
        "a successful request must clear the cooldown immediately"
    );

    clear_all_account_cooldowns();
}

/// A cooling-down account is never offered as a rotation target.
///
/// Rotation exists to find an account that is *not* limited; once every
/// alternative is known-limited there is nothing to find. ADR-0015's "never turn
/// a recoverable limit into a hard failure" is preserved by the caller waiting
/// on the account it already holds, not by handing it a candidate that will
/// refuse it too.
#[test]
fn cooling_down_accounts_are_skipped_and_not_offered_when_all_are_limited() {
    let _cooldown_guard = crate::provider::account_failover::lock_account_cooldown_tests();
    use crate::provider::account_failover::same_provider_account_candidates_for_tests;
    use crate::provider::{clear_all_account_cooldowns, note_account_rate_limited};
    use jcode_provider_core::ActiveProvider;

    clear_all_account_cooldowns();

    let ordered = same_provider_account_candidates_for_tests(
        ActiveProvider::Claude,
        vec!["claude-1".to_string(), "claude-2".to_string()],
    );
    assert_eq!(
        ordered,
        vec!["claude-1".to_string(), "claude-2".to_string()],
        "with no cooldowns the existing best-first order is preserved"
    );

    note_account_rate_limited(ActiveProvider::Claude, "claude-1");
    let ordered = same_provider_account_candidates_for_tests(
        ActiveProvider::Claude,
        vec!["claude-1".to_string(), "claude-2".to_string()],
    );
    assert_eq!(
        ordered,
        vec!["claude-2".to_string()],
        "the account that just refused must not be offered while a healthy one exists"
    );

    // Every account limited: offer none at all.
    //
    // This assertion previously required the opposite ("keep them as a last
    // resort"), on the reasoning that a limited account beats no candidate. That
    // is wrong for a *shared* limit and is what produced the reported flapping:
    // with two accounts both burst-limited, every turn hopped onto the other,
    // paid a second 429, announced a switch, and was restored. One switch plus
    // one restore per turn, indefinitely.
    //
    // Returning nothing does not fail the turn: the caller's `None` branch is
    // the dead end, which is exactly where ADR-0015's rate-limit wait lives, so
    // the turn backs off on the account it already holds. That property is
    // pinned end-to-end by
    // `no_candidates_still_waits_out_a_burst_rather_than_failing` in
    // jcode-provider-anthropic-runtime, which is the test that makes removing
    // this last-resort rule safe.
    note_account_rate_limited(ActiveProvider::Claude, "claude-2");
    let ordered = same_provider_account_candidates_for_tests(
        ActiveProvider::Claude,
        vec!["claude-1".to_string(), "claude-2".to_string()],
    );
    assert!(
        ordered.is_empty(),
        "when everything is limited, rotation has nothing to find and must offer \
         no candidate rather than hop onto another limited account: {:?}",
        ordered
    );

    clear_all_account_cooldowns();
}

/// A rate limit that names a reset must be remembered until that reset, not for
/// a flat 60s.
///
/// This is the defect behind the observed ping-pong: `claude-2` was blocked for
/// ~3 hours by a spent 5-hour window, the cooldown forgot it after a minute, and
/// rotation selected it again, and again.
#[test]
fn a_known_reset_outlasts_the_burst_cooldown() {
    let _cooldown_guard = crate::provider::account_failover::lock_account_cooldown_tests();
    use crate::provider::account_failover::same_provider_account_candidates_for_tests;
    use crate::provider::{
        clear_all_account_cooldowns, note_account_rate_limited, note_account_rate_limited_until,
    };
    use jcode_provider_core::ActiveProvider;

    clear_all_account_cooldowns();

    // A reset three hours out, i.e. far beyond the 60s burst window.
    note_account_rate_limited_until(
        ActiveProvider::Claude,
        "claude-2",
        Some(chrono::Utc::now() + chrono::Duration::hours(3)),
    );

    assert!(
        crate::provider::account_is_cooling_down(ActiveProvider::Claude, "claude-2"),
        "an account with a 3h reset must still be on cooldown"
    );
    assert_eq!(
        same_provider_account_candidates_for_tests(
            ActiveProvider::Claude,
            vec!["claude-2".to_string(), "claude-1".to_string()],
        ),
        vec!["claude-1".to_string()],
        "the quota-blocked account must be skipped in favour of the healthy one"
    );

    // A later burst-shaped note must not shorten a known long cooldown.
    note_account_rate_limited(ActiveProvider::Claude, "claude-2");
    assert!(
        crate::provider::account_is_cooling_down(ActiveProvider::Claude, "claude-2"),
        "a 60s note arriving later must not shrink the 3h deadline"
    );

    // A reset already in the past carries no information about the future, and
    // must fall back to the short burst window rather than expiring instantly.
    clear_all_account_cooldowns();
    note_account_rate_limited_until(
        ActiveProvider::Claude,
        "claude-1",
        Some(chrono::Utc::now() - chrono::Duration::hours(1)),
    );
    assert!(
        crate::provider::account_is_cooling_down(ActiveProvider::Claude, "claude-1"),
        "a past reset must still produce the short burst cooldown, not zero"
    );

    clear_all_account_cooldowns();
}

/// A quota that has since reset must let the account back in.
///
/// The cross-turn lens applied to the *recovery* direction, which nothing else
/// covers. F1/F2/F6 are all about keeping a blocked account out; every test for
/// them ends while the block is still in force, so none of them can show whether
/// the exclusion ever lifts. If it did not, these fixes would trade the user's
/// flapping for a worse failure: an account silently sidelined for the rest of
/// the process even after its window rolled over.
///
/// Three independent mechanisms have to agree, and each is checked here because
/// any one of them alone could strand the account:
/// 1. `UsageData::is_stale` must treat a passed reset as stale, so the cached
///    "spent" verdict expires rather than living out its 300s/900s TTL;
/// 2. `exhausted` must go false once the ratios are re-read;
/// 3. the rotation cooldown must expire, since it is sized from that reset.
#[test]
fn an_account_becomes_usable_again_once_its_window_resets() {
    let _cooldown_guard = crate::provider::account_failover::lock_account_cooldown_tests();
    crate::provider::clear_all_account_cooldowns();
    crate::usage::clear_anthropic_account_usage_for_tests();

    // A verdict recorded while the account was blocked, whose reset has since
    // passed: exactly the state a turn leaves behind when the user comes back an
    // hour later.
    let past = (chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();
    crate::usage::seed_anthropic_account_usage_for_tests(
        "claude-resettest",
        1.0,
        0.49,
        Some(past.clone()),
    );

    let cached = crate::usage::cached_anthropic_account_usage_any_age("claude-resettest")
        .expect("the seeded verdict must be readable");
    assert!(
        cached.is_stale(),
        "a verdict whose reset has passed must be stale, or the account stays \
         excluded for the rest of the cache TTL even though its quota returned"
    );

    // The cooldown is sized from the reset, and a reset in the past clamps to the
    // short burst window rather than to zero, so it must expire on its own rather
    // than persist for the original multi-hour horizon.
    crate::provider::note_account_rate_limited_until(
        jcode_provider_core::ActiveProvider::Claude,
        "claude-resettest",
        crate::usage::parse_reset_at(&past),
    );
    // The cooldown must be BOUNDED, i.e. sized to the short burst fallback rather
    // than to the multi-hour horizon the original block would have implied. A
    // reset already in the past carries no information about the future.
    //
    // Checked as "the cooldown exists but expires within the burst window", not
    // as "the account is still a candidate": a cooling account is deliberately no
    // longer offered as a rotation target at all (see
    // `cooling_down_accounts_are_skipped_and_not_offered_when_all_are_limited`),
    // so candidacy no longer distinguishes a bounded cooldown from an unbounded
    // one. What matters for recovery is that the cooldown lapses on its own.
    assert!(
        crate::provider::account_is_cooling_down(
            jcode_provider_core::ActiveProvider::Claude,
            "claude-resettest"
        ),
        "a past reset must still produce the short burst cooldown, not none"
    );

    clear_all_account_cooldowns();
    crate::usage::clear_anthropic_account_usage_for_tests();
}

/// The 429 evidence F6 writes must expire by itself once the reset passes.
///
/// Cross-turn lens again, on the write path rather than the read path. A 429
/// with a known reset writes a synthetic "5-hour window fully spent" entry. If
/// that entry did not expire on its own, a single rate limit would exclude the
/// account until the process restarted, which is strictly worse than the
/// flapping it was written to stop.
///
/// The mechanism is `is_stale` treating a passed reset as stale, which is why
/// the synthetic entry deliberately carries the provider's reset rather than
/// just an error string.
#[test]
fn a_429_evidence_entry_expires_when_its_reset_passes() {
    let _cooldown_guard = crate::provider::account_failover::lock_account_cooldown_tests();
    crate::usage::clear_anthropic_account_usage_for_tests();

    // A 429 whose reset is already in the past, i.e. the state the cache holds
    // by the time the user's quota has come back.
    crate::usage::note_anthropic_account_rate_limited(
        "claude-expiretest",
        Some((chrono::Utc::now() - chrono::Duration::minutes(1)).to_rfc3339()),
    );

    let entry = crate::usage::cached_anthropic_account_usage_any_age("claude-expiretest")
        .expect("the evidence entry must have been written");
    assert!(
        entry.exhausted(),
        "precondition: the evidence entry marks the account as spent"
    );
    assert!(
        entry.is_stale(),
        "once the reset has passed the evidence must be stale, so the next probe \
         re-reads reality instead of serving a limit that no longer applies"
    );

    crate::usage::clear_anthropic_account_usage_for_tests();
}
