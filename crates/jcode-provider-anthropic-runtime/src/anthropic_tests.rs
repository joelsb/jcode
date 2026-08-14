use super::*;

struct EnvVarGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(key);
        jcode_base::env::set_var(key, value);
        Self { key, previous }
    }

    fn set_if_missing(key: &'static str, value: &str) -> Option<Self> {
        if std::env::var_os(key).is_some() {
            return None;
        }
        Some(Self::set(key, value))
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            jcode_base::env::set_var(self.key, previous);
        } else {
            jcode_base::env::remove_var(self.key);
        }
    }
}

async fn collect_live_smoke_stream(
    mut stream: EventStream,
    timeout: std::time::Duration,
) -> Result<(usize, usize, bool)> {
    tokio::time::timeout(timeout, async move {
        let mut text_bytes = 0usize;
        let mut thinking_bytes = 0usize;
        let mut saw_message_end = false;
        while let Some(event) = stream.next().await {
            match event? {
                StreamEvent::TextDelta(text) => {
                    text_bytes += text.len();
                }
                StreamEvent::ThinkingDelta(text) => {
                    thinking_bytes += text.len();
                }
                StreamEvent::MessageEnd { .. } => {
                    saw_message_end = true;
                    break;
                }
                StreamEvent::Error { message, .. } => anyhow::bail!(message),
                _ => {}
            }
        }
        Ok((text_bytes, thinking_bytes, saw_message_end))
    })
    .await
    .context("live provider smoke timed out")?
}

#[test]
fn test_parse_sse_event() {
    let mut buffer = "event: message_start\ndata: {\"type\":\"message_start\"}\n\n".to_string();
    let event = parse_sse_event(&mut buffer).unwrap();
    assert_eq!(event.event_type, "message_start");
    assert!(buffer.is_empty());
}

#[tokio::test]
async fn test_available_models() {
    let provider = AnthropicProvider::new();
    let models = provider.available_models();
    assert!(models.contains(&"claude-opus-4-8"));
    // Opus 4.8 is native-1M, so there is no redundant `[1m]` alias.
    assert!(!models.contains(&"claude-opus-4-8[1m]"));
    assert!(models.contains(&"claude-opus-4-6"));
    assert!(models.contains(&"claude-opus-4-6[1m]"));
    assert!(models.contains(&"claude-sonnet-4-6"));
    assert!(models.contains(&"claude-sonnet-4-6[1m]"));
    assert!(models.contains(&"claude-haiku-4-5"));
}

#[test]
fn test_effectively_1m_requires_explicit_suffix() {
    assert!(!effectively_1m("claude-opus-4-6"));
    assert!(!effectively_1m("claude-sonnet-4-6"));
    assert!(effectively_1m("claude-opus-4-6[1m]"));
    assert!(effectively_1m("claude-sonnet-4-6[1m]"));
}

#[test]
fn test_oauth_beta_headers_require_explicit_1m_suffix() {
    assert_eq!(oauth_beta_headers("claude-opus-4-6"), OAUTH_BETA_HEADERS);
    assert_eq!(
        oauth_beta_headers("claude-opus-4-6[1m]"),
        OAUTH_BETA_HEADERS_1M
    );
}

#[test]
fn test_anthropic_reasoning_effort_request_parts() {
    let provider = AnthropicProvider::new();
    provider.set_model("claude-sonnet-4-6").unwrap();
    provider.set_reasoning_effort("none").unwrap();
    assert!(
        provider.set_reasoning_effort("minimal").is_err(),
        "Anthropic must reject rather than silently promote minimal to max"
    );

    assert_eq!(
        provider.available_efforts(),
        vec![
            "none",
            "low",
            "medium",
            "high",
            "max",
            "swarm",
            "swarm-deep"
        ]
    );
    assert_eq!(provider.reasoning_effort().as_deref(), Some("none"));

    // Sonnet 4.6 supports the real `max` API level (but not `xhigh`).
    provider.set_reasoning_effort("max").unwrap();
    assert_eq!(provider.reasoning_effort().as_deref(), Some("max"));

    // `xhigh` is rejected on models that do not support it.
    assert!(provider.set_reasoning_effort("xhigh").is_err());

    provider.set_reasoning_effort("medium").unwrap();
    let (thinking, output_config, temperature) =
        provider.build_reasoning_request_parts("claude-sonnet-4-6", true);

    match thinking.expect("adaptive thinking should be enabled") {
        ApiThinking::Adaptive { display } => assert_eq!(display, Some("summarized")),
        ApiThinking::Enabled { .. } => panic!("Claude 4.6 should use adaptive thinking"),
    }
    assert_eq!(
        output_config.expect("output_config should be set").effort,
        "medium"
    );
    assert_eq!(
        temperature, None,
        "thinking requests must omit OAuth temperature"
    );
}

#[test]
fn test_anthropic_preserves_swarm_sentinels_for_cycling() {
    // Regression: storing a swarm effort must preserve which swarm mode was
    // chosen. Previously both `swarm` and `swarm-deep` collapsed to `swarm`,
    // which capped Alt+Right effort cycling at swarm-light (it could never
    // reach swarm-deep because the readback always reported `swarm`).
    let provider = AnthropicProvider::new();
    provider.set_model("claude-sonnet-4-6").unwrap();

    provider.set_reasoning_effort("swarm").unwrap();
    assert_eq!(provider.reasoning_effort().as_deref(), Some("swarm"));

    provider.set_reasoning_effort("swarm-deep").unwrap();
    assert_eq!(
        provider.reasoning_effort().as_deref(),
        Some("swarm-deep"),
        "swarm-deep must survive the round-trip so cycling can reach it"
    );

    // And cycling back down to swarm-light still works.
    provider.set_reasoning_effort("swarm").unwrap();
    assert_eq!(provider.reasoning_effort().as_deref(), Some("swarm"));
}

#[test]
fn test_anthropic_show_thinking_enables_adaptive_thinking_without_effort() {
    // With no explicit reasoning effort, an adaptive-thinking model should still
    // request summarized thinking when the user has opted into the display.
    // Crucially, `output_config` must stay None so we do not force a stronger
    // (more expensive) reasoning level than the model's default.
    //
    // We use a non-Opus model here because Opus now carries an implicit `xhigh`
    // default (see `test_anthropic_opus_defaults_to_xhigh_effort`); Sonnet keeps
    // the model's own default so this invariant stays meaningful.
    //
    // `build_reasoning_request_parts_inner` takes the model directly, so we do
    // not depend on `set_model` accepting a particular catalog entry. With no
    // effort configured, `self.reasoning_effort()` resolves to None regardless
    // of the default model.
    let provider = AnthropicProvider::new();
    // Make the test independent of the ambient config's anthropic_reasoning_effort
    // by clearing the field directly; we only exercise the show_thinking path.
    *provider.reasoning_effort.write().unwrap() = None;

    // show_thinking = false: nothing requested.
    let (thinking, output_config, _temp) =
        provider.build_reasoning_request_parts_inner("claude-sonnet-4-6", true, false);
    assert!(
        thinking.is_none(),
        "no thinking should be requested when both effort and show_thinking are off"
    );
    assert!(output_config.is_none());

    // show_thinking = true: adaptive thinking requested, no output_config.
    let (thinking, output_config, temperature) =
        provider.build_reasoning_request_parts_inner("claude-sonnet-4-6", true, true);
    match thinking.expect("show_thinking should enable adaptive thinking") {
        ApiThinking::Adaptive { display } => assert_eq!(display, Some("summarized")),
        ApiThinking::Enabled { .. } => panic!("Sonnet 4.6 should use adaptive thinking"),
    }
    assert!(
        output_config.is_none(),
        "show_thinking alone must not force an output reasoning effort"
    );
    assert_eq!(
        temperature, None,
        "thinking requests must omit OAuth temperature"
    );
}

#[test]
fn test_anthropic_explicit_none_effort_disables_thinking_even_with_show_thinking() {
    // Regression: with `display.show_thinking = true` (the default), setting
    // effort to `none` still requested adaptive thinking, so the user kept
    // seeing reasoning on Fable/Sonnet. An explicit `none` must suppress the
    // thinking request entirely, on both adaptive and manual thinking models.
    let provider = AnthropicProvider::new();
    *provider.reasoning_effort.write().unwrap() = Some("none".to_string());

    // Adaptive-thinking model (Fable 5 / Sonnet 4.6 family).
    let (thinking, output_config, temperature) =
        provider.build_reasoning_request_parts_inner("claude-fable-5", true, true);
    assert!(
        thinking.is_none(),
        "explicit effort=none must suppress thinking even when show_thinking is on"
    );
    assert!(output_config.is_none());
    assert_eq!(
        temperature,
        Some(1.0),
        "no thinking means the OAuth path restores temperature"
    );

    // Manual-thinking model.
    let (thinking, output_config, _temp) =
        provider.build_reasoning_request_parts_inner("claude-3-7-sonnet", false, true);
    assert!(
        thinking.is_none(),
        "explicit effort=none must suppress manual thinking budgets too"
    );
    assert!(output_config.is_none());
}

#[test]
fn test_anthropic_fable_defaults_to_high_effort() {
    // Fable 5 defaults to `high` reasoning when no explicit user effort is
    // configured. An explicit override still wins.
    let provider = AnthropicProvider::new();
    *provider.reasoning_effort.write().unwrap() = None;

    assert_eq!(
        AnthropicProvider::default_reasoning_effort_for_model("claude-fable-5").as_deref(),
        Some("high"),
    );

    // The default drives the request: output_config high + adaptive thinking.
    let (thinking, output_config, _temp) =
        provider.build_reasoning_request_parts_inner("claude-fable-5", true, false);
    assert_eq!(
        output_config
            .expect("Fable should default to a forced output effort")
            .effort,
        "high",
    );
    match thinking.expect("Fable default effort should enable adaptive thinking") {
        ApiThinking::Adaptive { display } => assert_eq!(display, Some("summarized")),
        ApiThinking::Enabled { .. } => panic!("Fable 5 should use adaptive thinking"),
    }

    // The surfaced status mirrors the effective default for the active model.
    *provider
        .model
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = "claude-fable-5".to_string();
    assert_eq!(provider.reasoning_effort().as_deref(), Some("high"));

    // An explicit user override still wins over the Fable default.
    provider.set_reasoning_effort("low").unwrap();
    assert_eq!(provider.reasoning_effort().as_deref(), Some("low"));
    provider.set_reasoning_effort("none").unwrap();
    let (thinking, output_config, _temp) =
        provider.build_reasoning_request_parts_inner("claude-fable-5", true, true);
    assert!(
        thinking.is_none(),
        "explicit none must beat the high default and show_thinking"
    );
    assert!(output_config.is_none());
}

#[test]
fn test_anthropic_sonnet_5_supports_full_effort_ladder() {
    // `claude-sonnet-5` accepts `output_config` effort low..xhigh/max and
    // adaptive thinking (verified live 2026-07-07).
    assert!(AnthropicProvider::model_supports_output_effort(
        "claude-sonnet-5"
    ));
    assert!(AnthropicProvider::model_supports_adaptive_thinking(
        "claude-sonnet-5"
    ));
    assert!(AnthropicProvider::model_supports_xhigh_effort(
        "claude-sonnet-5"
    ));
    assert!(AnthropicProvider::model_supports_max_effort(
        "claude-sonnet-5"
    ));

    let provider = AnthropicProvider::new();
    *provider.reasoning_effort.write().unwrap() = None;
    *provider
        .model
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = "claude-sonnet-5".to_string();

    // No forced default: Sonnet keeps the model's own reasoning behavior.
    assert_eq!(
        AnthropicProvider::default_reasoning_effort_for_model("claude-sonnet-5"),
        None,
    );

    // Explicit efforts are accepted and drive the request.
    for effort in ["low", "medium", "high", "xhigh", "max"] {
        provider.set_reasoning_effort(effort).unwrap();
        assert_eq!(provider.reasoning_effort().as_deref(), Some(effort));
        let (thinking, output_config, _temp) =
            provider.build_reasoning_request_parts_inner("claude-sonnet-5", true, false);
        assert_eq!(
            output_config
                .expect("explicit effort should set output_config")
                .effort,
            effort,
        );
        assert!(matches!(thinking, Some(ApiThinking::Adaptive { .. })));
    }

    assert_eq!(
        provider.available_efforts(),
        vec![
            "none",
            "low",
            "medium",
            "high",
            "xhigh",
            "max",
            "swarm",
            "swarm-deep"
        ],
    );
}

#[test]
fn test_anthropic_opus_defaults_to_xhigh_effort() {
    // Opus is a reasoning-heavy flagship, so when the user has *not* configured
    // an explicit effort it should default to its strongest supported level
    // (`xhigh` on Opus 4.7/4.8). This drives both the request `output_config`
    // and the surfaced `reasoning_effort()` status.
    let provider = AnthropicProvider::new();
    // Clear any ambient config-provided effort so we exercise the model default.
    *provider.reasoning_effort.write().unwrap() = None;

    assert_eq!(
        AnthropicProvider::default_reasoning_effort_for_model("claude-opus-4-8").as_deref(),
        Some("xhigh"),
    );
    assert_eq!(
        AnthropicProvider::default_reasoning_effort_for_model("claude-opus-4-7").as_deref(),
        Some("xhigh"),
    );
    // Older Opus does not support xhigh, so it clamps to high.
    assert_eq!(
        AnthropicProvider::default_reasoning_effort_for_model("claude-opus-4-5").as_deref(),
        Some("high"),
    );
    // Non-Opus models keep the model's own default (no forced effort).
    assert_eq!(
        AnthropicProvider::default_reasoning_effort_for_model("claude-sonnet-4-6"),
        None,
    );

    // Even without show_thinking, Opus forces its strongest output effort.
    let (thinking, output_config, _temp) =
        provider.build_reasoning_request_parts_inner("claude-opus-4-8", true, false);
    assert_eq!(
        output_config
            .expect("Opus should default to a forced output effort")
            .effort,
        "xhigh",
    );
    match thinking.expect("Opus default effort should enable adaptive thinking") {
        ApiThinking::Adaptive { display } => assert_eq!(display, Some("summarized")),
        ApiThinking::Enabled { .. } => panic!("Opus 4.8 should use adaptive thinking"),
    }

    // The surfaced status mirrors the effective default for the active model.
    *provider
        .model
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = "claude-opus-4-8".to_string();
    assert_eq!(provider.reasoning_effort().as_deref(), Some("xhigh"));

    // An explicit user override still wins over the Opus default.
    provider.set_reasoning_effort("low").unwrap();
    assert_eq!(provider.reasoning_effort().as_deref(), Some("low"));
}

#[test]
fn test_anthropic_show_thinking_enables_manual_thinking_without_effort() {
    // Manual-thinking models (e.g. Claude 3.7 Sonnet) need a concrete budget;
    // with only the display toggle on we fall back to the minimal budget. We use
    // a non-Opus model here because Opus now carries an implicit strongest-effort
    // default (see `test_anthropic_opus_defaults_to_xhigh_effort`). The model is
    // passed directly so this does not depend on `set_model` validation.
    let provider = AnthropicProvider::new();
    // Independent of ambient config: clear any configured effort.
    *provider.reasoning_effort.write().unwrap() = None;

    let (thinking, _output_config, _temp) =
        provider.build_reasoning_request_parts_inner("claude-3-7-sonnet", false, false);
    assert!(thinking.is_none());

    let (thinking, _output_config, _temperature) =
        provider.build_reasoning_request_parts_inner("claude-3-7-sonnet", false, true);
    match thinking.expect("show_thinking should enable manual thinking") {
        ApiThinking::Enabled { budget_tokens } => assert_eq!(budget_tokens, 1_024),
        ApiThinking::Adaptive { .. } => panic!("Claude 3.7 Sonnet should use manual thinking"),
    }
}

#[test]
fn test_anthropic_max_alias_uses_strongest_real_effort() {
    // `max` is a real API level on output_config effort models.
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-sonnet-4-6", "max"),
        "max"
    );
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-opus-4-7", "max"),
        "max"
    );
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-opus-4-8", "max"),
        "max"
    );
    // Manual-thinking models (no output_config) clamp max to high.
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-opus-4-5", "max"),
        "high"
    );
    // xhigh still clamps to high where unsupported.
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-sonnet-4-6", "xhigh"),
        "high"
    );
    // Swarm rungs pin to the strongest supported level.
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-opus-4-8", "swarm"),
        "max"
    );
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-sonnet-4-6", "swarm-deep"),
        "max"
    );
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-opus-4-5", "swarm"),
        "high"
    );
}

#[test]
fn test_anthropic_opus_48_fast_mode_service_tier_serializes_priority() {
    let provider = AnthropicProvider::new();
    provider.set_model("claude-opus-4-8").unwrap();

    assert_eq!(provider.available_service_tiers(), vec!["off", "priority"]);
    assert_eq!(provider.service_tier(), None);

    provider.set_service_tier("priority").unwrap();
    assert_eq!(provider.service_tier().as_deref(), Some("priority"));

    let request = ApiRequest {
        model: strip_1m_suffix(&provider.model()).to_string(),
        max_tokens: 1024,
        system: None,
        messages: vec![],
        tools: None,
        metadata: None,
        thinking: None,
        output_config: None,
        temperature: None,
        service_tier: provider.current_service_tier_for_model(&provider.model()),
        stream: true,
    };
    let value = serde_json::to_value(&request).unwrap();

    assert_eq!(value["model"], "claude-opus-4-8");
    assert_eq!(value["service_tier"], "auto");
}

#[test]
fn test_anthropic_fast_mode_is_limited_to_opus_48() {
    let provider = AnthropicProvider::new();
    provider.set_model("claude-opus-4-6").unwrap();

    assert!(provider.available_service_tiers().is_empty());
    assert!(provider.set_service_tier("priority").is_err());
    assert_eq!(provider.service_tier(), None);

    // A stale `[1m]` alias for a native-1M model is migrated to canonical form.
    provider.set_model("claude-opus-4-8[1m]").unwrap();
    assert_eq!(provider.model(), "claude-opus-4-8");
    provider.set_service_tier("priority").unwrap();
    assert_eq!(provider.service_tier().as_deref(), Some("priority"));

    provider.set_service_tier("off").unwrap();
    assert_eq!(provider.service_tier(), None);
}

#[test]
fn test_anthropic_manual_thinking_budget_for_opus_45() {
    let provider = AnthropicProvider::new();
    // Keep this request-builder test independent of the live/persisted Anthropic
    // model catalog, which may legitimately omit older Opus 4.5 models.
    *provider
        .model
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = "claude-opus-4-5".to_string();
    provider.set_reasoning_effort("high").unwrap();

    let (thinking, output_config, temperature) =
        provider.build_reasoning_request_parts("claude-opus-4-5", false);

    match thinking.expect("manual thinking should be enabled") {
        ApiThinking::Enabled { budget_tokens } => assert_eq!(budget_tokens, 8_192),
        ApiThinking::Adaptive { .. } => panic!("Claude Opus 4.5 should use manual thinking"),
    }
    assert_eq!(output_config.unwrap().effort, "high");
    assert_eq!(temperature, None);
}

#[test]
fn message_start_warns_when_server_substitutes_a_different_model() {
    // Anthropic can silently alias an unavailable model id to a different model
    // (observed: claude-fable-5 -> claude-haiku-4-5). When the served model
    // differs from the requested base id, we must surface a StatusDetail warning
    // so the user is not misled about which model answered.
    let mut state = SseStreamState {
        requested_model_base: "claude-fable-5".to_string(),
        ..SseStreamState::default()
    };
    let event = SseEvent {
        event_type: "message_start".to_string(),
        data: serde_json::json!({
            "type": "message_start",
            "message": {"model": "claude-haiku-4-5-20251001", "usage": {"input_tokens": 1}}
        })
        .to_string(),
    };
    let events = process_sse_event(&event, &mut state, true);
    let warned = events.iter().any(|e| {
        matches!(e, StreamEvent::StatusDetail { detail }
            if detail.contains("claude-haiku-4-5") && detail.contains("claude-fable-5"))
    });
    assert!(
        warned,
        "expected a substitution StatusDetail, got {events:?}"
    );
    assert!(state.warned_model_substitution);

    // A matching served model must NOT warn.
    let mut state = SseStreamState {
        requested_model_base: "claude-opus-4-8".to_string(),
        ..SseStreamState::default()
    };
    let event = SseEvent {
        event_type: "message_start".to_string(),
        data: serde_json::json!({
            "type": "message_start",
            "message": {"model": "claude-opus-4-8", "usage": {"input_tokens": 1}}
        })
        .to_string(),
    };
    let events = process_sse_event(&event, &mut state, true);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, StreamEvent::StatusDetail { .. })),
        "served model matched request; must not warn"
    );
    assert!(!state.warned_model_substitution);
}

#[test]
fn test_anthropic_thinking_sse_events() {
    let mut state = SseStreamState::default();
    let start = SseEvent {
        event_type: "content_block_start".to_string(),
        data: serde_json::json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "thinking", "thinking": "", "signature": "sig"}
        })
        .to_string(),
    };
    let events = process_sse_event(&start, &mut state, false);
    assert!(matches!(events.as_slice(), [StreamEvent::ThinkingStart]));
    assert!(state.current_thinking_block);

    let delta = SseEvent {
        event_type: "content_block_delta".to_string(),
        data: serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "thinking_delta", "thinking": "reasoning text"}
        })
        .to_string(),
    };
    let events = process_sse_event(&delta, &mut state, false);
    assert!(
        matches!(events.as_slice(), [StreamEvent::ThinkingDelta(text)] if text == "reasoning text")
    );

    let signature = SseEvent {
        event_type: "content_block_delta".to_string(),
        data: serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "signature_delta", "signature": "signed"}
        })
        .to_string(),
    };
    let events = process_sse_event(&signature, &mut state, false);
    assert!(
        matches!(events.as_slice(), [StreamEvent::ThinkingSignatureDelta(sig)] if sig == "signed")
    );

    let stop = SseEvent {
        event_type: "content_block_stop".to_string(),
        data: serde_json::json!({"type": "content_block_stop", "index": 0}).to_string(),
    };
    let events = process_sse_event(&stop, &mut state, false);
    assert!(matches!(events.as_slice(), [StreamEvent::ThinkingEnd]));
    assert!(!state.current_thinking_block);
}

#[test]
fn test_anthropic_signed_thinking_replayed_in_request_blocks() {
    let provider = AnthropicProvider::new();
    let blocks = provider.format_content_blocks(
        &[ContentBlock::AnthropicThinking {
            thinking: "reasoning text".to_string(),
            signature: "signed".to_string(),
        }],
        false,
    );

    let value = serde_json::to_value(&blocks).expect("serialize content blocks");
    assert_eq!(
        value,
        serde_json::json!([
            {
                "type": "thinking",
                "thinking": "reasoning text",
                "signature": "signed"
            }
        ])
    );
}

#[tokio::test]
#[ignore = "live smoke: requires ANTHROPIC_API_KEY, or set JCODE_LIVE_ANTHROPIC_ALLOW_OAUTH=1 to use Claude OAuth credentials"]
async fn live_anthropic_reasoning_smoke() -> Result<()> {
    let _env_lock = jcode_base::storage::lock_test_env();
    let using_api_key = std::env::var_os("ANTHROPIC_API_KEY").is_some();
    let allow_oauth = std::env::var_os("JCODE_LIVE_ANTHROPIC_ALLOW_OAUTH").is_some();
    if !using_api_key && !allow_oauth {
        eprintln!(
            "skipping live Anthropic smoke: set ANTHROPIC_API_KEY or JCODE_LIVE_ANTHROPIC_ALLOW_OAUTH=1"
        );
        return Ok(());
    }

    let _max_tokens = EnvVarGuard::set_if_missing("JCODE_ANTHROPIC_MAX_TOKENS", "2048");
    let model = std::env::var("JCODE_LIVE_ANTHROPIC_MODEL")
        .or_else(|_| std::env::var("JCODE_ANTHROPIC_MODEL"))
        .unwrap_or_else(|_| "claude-sonnet-4-6".to_string());
    let effort = std::env::var("JCODE_LIVE_ANTHROPIC_REASONING_EFFORT")
        .unwrap_or_else(|_| "low".to_string());
    let prompt = std::env::var("JCODE_LIVE_ANTHROPIC_PROMPT")
        .unwrap_or_else(|_| "Live smoke test: answer exactly OK.".to_string());
    let system = std::env::var("JCODE_LIVE_ANTHROPIC_SYSTEM").unwrap_or_else(|_| {
        "You are a live provider smoke test. Keep the answer tiny.".to_string()
    });
    let require_thinking = std::env::var_os("JCODE_LIVE_ANTHROPIC_REQUIRE_THINKING").is_some();

    let provider = AnthropicProvider::new();
    provider.set_model(&model)?;
    // Some models (e.g. Fable 5) legitimately reject any reasoning effort. Treat
    // that as "use the model default" so the live call still exercises the model
    // rather than aborting the smoke test before any request is sent.
    if let Err(err) = provider.set_reasoning_effort(&effort) {
        eprintln!(
            "model {model} does not support reasoning effort '{effort}' ({err}); using model default"
        );
    }

    let messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: prompt,
            cache_control: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    }];

    let stream = provider.complete(&messages, &[], &system, None).await?;
    let (text_bytes, thinking_bytes, saw_message_end) =
        collect_live_smoke_stream(stream, std::time::Duration::from_secs(90)).await?;

    eprintln!(
        "live Anthropic reasoning smoke passed: model={model}, effort={effort}, text_bytes={text_bytes}, thinking_bytes={thinking_bytes}, message_end={saw_message_end}"
    );
    assert!(
        text_bytes > 0 || thinking_bytes > 0,
        "live Anthropic response contained neither text nor thinking deltas"
    );
    if require_thinking {
        assert!(
            thinking_bytes > 0,
            "live Anthropic response did not include thinking deltas despite JCODE_LIVE_ANTHROPIC_REQUIRE_THINKING"
        );
    }
    Ok(())
}

#[tokio::test]
async fn test_dangling_tool_use_repair() {
    let provider = AnthropicProvider::new();

    // Create messages with a dangling tool_use (no corresponding tool_result)
    let messages = vec![
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Hello".to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "Let me check".to_string(),
                    cache_control: None,
                },
                ContentBlock::ToolUse {
                    id: "tool_123".to_string(),
                    name: "bash".to_string(),
                    input: serde_json::json!({"command": "ls"}),
                    thought_signature: None,
                },
                ContentBlock::ToolUse {
                    id: "tool_456".to_string(),
                    name: "read".to_string(),
                    input: serde_json::json!({"file_path": "/tmp/test"}),
                    thought_signature: None,
                },
            ],
            timestamp: None,
            tool_duration_ms: None,
        },
        // Missing tool_results for tool_123 and tool_456!
    ];

    let formatted = provider.format_messages(&messages, false);

    // Should have 3 messages:
    // 1. User: "Hello"
    // 2. Assistant: text + tool_uses
    // 3. User: synthetic tool_results for the dangling tool_uses
    assert_eq!(formatted.len(), 3);

    // Check the synthetic tool_result message
    let synthetic_msg = &formatted[2];
    assert_eq!(synthetic_msg.role, "user");
    assert_eq!(synthetic_msg.content.len(), 2);

    // Verify both tool_results are present
    let mut found_ids = std::collections::HashSet::new();
    for block in &synthetic_msg.content {
        if let ApiContentBlock::ToolResult {
            tool_use_id,
            is_error,
            content,
        } = block
        {
            found_ids.insert(tool_use_id.clone());
            assert!(is_error);
            match content {
                ToolResultContent::Text(t) => assert!(t.contains("interrupted")),
                ToolResultContent::Blocks(_) => panic!("Expected text content"),
            }
        } else {
            panic!("Expected ToolResult block");
        }
    }
    assert!(found_ids.contains("tool_123"));
    assert!(found_ids.contains("tool_456"));
}

#[tokio::test]
async fn test_no_repair_when_tool_results_present() {
    let provider = AnthropicProvider::new();

    // Create messages where tool_use has a corresponding tool_result
    let messages = vec![
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Hello".to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "tool_123".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "ls"}),
                thought_signature: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "tool_123".to_string(),
                content: "file1.txt\nfile2.txt".to_string(),
                is_error: Some(false),
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
    ];

    let formatted = provider.format_messages(&messages, false);

    // Should have exactly 3 messages (no synthetic ones added)
    assert_eq!(formatted.len(), 3);

    // The last message should be the actual tool_result, not synthetic
    let last_msg = &formatted[2];
    if let ApiContentBlock::ToolResult { content, .. } = &last_msg.content[0] {
        match content {
            ToolResultContent::Text(t) => assert!(t.contains("file1.txt")),
            ToolResultContent::Blocks(_) => panic!("Expected text content"),
        }
    } else {
        panic!("Expected ToolResult block");
    }
}

#[tokio::test]
async fn test_parallel_image_tool_results_stay_contiguous() {
    // Regression for Anthropic 400: "`tool_use` ids were found without `tool_result`
    // blocks immediately after". When the assistant issues several parallel `read`
    // calls that return images, each tool result is stored as its own user message in
    // the form [tool_result, image, "[Attached image ...]" text]. After merging the
    // consecutive user messages, the sibling label text blocks were wedged between the
    // tool_results, which Anthropic rejects. The label must be folded into the
    // tool_result content so every tool_result stays contiguous.
    let provider = AnthropicProvider::new();

    let make_image_result = |id: &str, label: &str| Message {
        role: Role::User,
        content: vec![
            ContentBlock::ToolResult {
                tool_use_id: id.to_string(),
                content: format!("Image: {label}"),
                is_error: None,
            },
            ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "AAAA".to_string(),
            },
            ContentBlock::Text {
                text: format!(
                    "[Attached image associated with the preceding tool result: {label}]"
                ),
                cache_control: None,
            },
        ],
        timestamp: None,
        tool_duration_ms: None,
    };

    let messages = vec![
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::ToolUse {
                    id: "tool_a".to_string(),
                    name: "read".to_string(),
                    input: serde_json::json!({"file_path": "a.png"}),
                    thought_signature: None,
                },
                ContentBlock::ToolUse {
                    id: "tool_b".to_string(),
                    name: "read".to_string(),
                    input: serde_json::json!({"file_path": "b.png"}),
                    thought_signature: None,
                },
                ContentBlock::ToolUse {
                    id: "tool_c".to_string(),
                    name: "read".to_string(),
                    input: serde_json::json!({"file_path": "c.png"}),
                    thought_signature: None,
                },
            ],
            timestamp: None,
            tool_duration_ms: None,
        },
        make_image_result("tool_a", "a.png"),
        make_image_result("tool_b", "b.png"),
        make_image_result("tool_c", "c.png"),
    ];

    let formatted = provider.format_messages(&messages, false);

    // assistant message + merged user tool_result message
    assert_eq!(formatted.len(), 2);
    let user_msg = &formatted[1];
    assert_eq!(user_msg.role, "user");

    // Every block in the user message must be a tool_result (no sibling text blocks
    // wedged between them), and all three tool_use ids must be present.
    let mut seen = std::collections::HashSet::new();
    for block in &user_msg.content {
        match block {
            ApiContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } => {
                seen.insert(tool_use_id.clone());
                // Each image tool_result should carry its image and folded label text.
                match content {
                    ToolResultContent::Blocks(blocks) => {
                        assert!(
                            blocks
                                .iter()
                                .any(|b| matches!(b, ToolResultContentBlock::Image { .. })),
                            "image tool_result should contain an image block"
                        );
                        assert!(
                            blocks.iter().any(|b| matches!(
                                b,
                                ToolResultContentBlock::Text { text }
                                    if text.contains("[Attached image associated")
                            )),
                            "label text should be folded into the tool_result content"
                        );
                    }
                    ToolResultContent::Text(_) => {
                        panic!("image tool_result should use block content")
                    }
                }
            }
            _ => panic!("expected only tool_result blocks in the user message"),
        }
    }
    assert_eq!(
        seen,
        ["tool_a", "tool_b", "tool_c"]
            .iter()
            .map(|s| s.to_string())
            .collect::<std::collections::HashSet<_>>()
    );
}

#[test]
fn test_cache_breakpoint_no_messages() {
    let mut messages: Vec<ApiMessage> = vec![];
    add_message_cache_breakpoint(&mut messages);
    // Should not panic, just return early
    assert!(messages.is_empty());
}

#[test]
fn test_cache_breakpoint_too_few_messages() {
    let mut messages = vec![
        ApiMessage {
            role: "user".to_string(),
            content: vec![ApiContentBlock::Text {
                text: "Hello".to_string(),
                cache_control: None,
            }],
        },
        ApiMessage {
            role: "user".to_string(),
            content: vec![ApiContentBlock::Text {
                text: "World".to_string(),
                cache_control: None,
            }],
        },
    ];
    add_message_cache_breakpoint(&mut messages);
    // With only 2 messages, should not add cache control
    for msg in &messages {
        for block in &msg.content {
            if let ApiContentBlock::Text { cache_control, .. } = block {
                assert!(cache_control.is_none());
            }
        }
    }
}

#[test]
fn test_cache_breakpoint_adds_to_assistant_message() {
    let mut messages = vec![
        ApiMessage {
            role: "user".to_string(),
            content: vec![ApiContentBlock::Text {
                text: "Identity".to_string(),
                cache_control: None,
            }],
        },
        ApiMessage {
            role: "user".to_string(),
            content: vec![ApiContentBlock::Text {
                text: "Hello".to_string(),
                cache_control: None,
            }],
        },
        ApiMessage {
            role: "assistant".to_string(),
            content: vec![ApiContentBlock::Text {
                text: "Hi there!".to_string(),
                cache_control: None,
            }],
        },
        ApiMessage {
            role: "user".to_string(),
            content: vec![ApiContentBlock::Text {
                text: "How are you?".to_string(),
                cache_control: None,
            }],
        },
    ];

    add_message_cache_breakpoint(&mut messages);

    // Assistant message (index 2) should have cache_control
    if let ApiContentBlock::Text { cache_control, .. } = &messages[2].content[0] {
        assert!(cache_control.is_some());
    } else {
        panic!("Expected Text block");
    }

    // Other messages should NOT have cache_control
    for (i, msg) in messages.iter().enumerate() {
        if i == 2 {
            continue; // Skip the assistant message we just checked
        }
        for block in &msg.content {
            if let ApiContentBlock::Text { cache_control, .. } = block {
                assert!(
                    cache_control.is_none(),
                    "Message {} should not have cache_control",
                    i
                );
            }
        }
    }
}

#[test]
fn test_cache_breakpoint_finds_text_in_mixed_content() {
    // Assistant message with tool_use followed by text
    let mut messages = vec![
        ApiMessage {
            role: "user".to_string(),
            content: vec![ApiContentBlock::Text {
                text: "Identity".to_string(),
                cache_control: None,
            }],
        },
        ApiMessage {
            role: "user".to_string(),
            content: vec![ApiContentBlock::Text {
                text: "Run a command".to_string(),
                cache_control: None,
            }],
        },
        ApiMessage {
            role: "assistant".to_string(),
            content: vec![
                ApiContentBlock::Text {
                    text: "Running command...".to_string(),
                    cache_control: None,
                },
                ApiContentBlock::ToolUse {
                    id: "tool_1".to_string(),
                    name: "bash".to_string(),
                    input: serde_json::json!({"command": "ls"}),
                    cache_control: None,
                },
            ],
        },
        ApiMessage {
            role: "user".to_string(),
            content: vec![ApiContentBlock::Text {
                text: "Thanks".to_string(),
                cache_control: None,
            }],
        },
    ];

    add_message_cache_breakpoint(&mut messages);

    // The last block (ToolUse) in the assistant message should have cache_control
    // (we prefer the last block for maximum cache coverage)
    let assistant_msg = &messages[2];
    let has_cached_block = assistant_msg.content.iter().any(|block| {
        matches!(
            block,
            ApiContentBlock::ToolUse {
                cache_control: Some(_),
                ..
            }
        )
    });
    assert!(
        has_cached_block,
        "Should have added cache_control to last block (ToolUse) in assistant message"
    );
}

#[test]
fn test_system_param_split_oauth() {
    let static_content = "This is static content";
    let dynamic_content = "This is dynamic content";

    let result = build_system_param_split(static_content, dynamic_content, true);

    if let Some(ApiSystem::Blocks(blocks)) = result {
        // Should have 4 blocks: identity, notice, static (cached), dynamic (not cached)
        assert_eq!(blocks.len(), 4);

        // Block 0: identity (no cache)
        assert!(blocks[0].cache_control.is_none());

        // Block 1: notice (no cache)
        assert!(blocks[1].cache_control.is_none());

        // Block 2: static (cached)
        assert!(blocks[2].cache_control.is_some());
        assert!(blocks[2].text.contains("static"));

        // Block 3: dynamic (not cached)
        assert!(blocks[3].cache_control.is_none());
        assert!(blocks[3].text.contains("dynamic"));
    } else {
        panic!("Expected Blocks variant");
    }
}

#[test]
fn test_system_param_split_non_oauth() {
    let static_content = "This is static content";
    let dynamic_content = "This is dynamic content";

    let result = build_system_param_split(static_content, dynamic_content, false);

    if let Some(ApiSystem::Blocks(blocks)) = result {
        // Should have 2 blocks: static (cached), dynamic (not cached)
        assert_eq!(blocks.len(), 2);

        // Block 0: static (cached)
        assert!(blocks[0].cache_control.is_some());

        // Block 1: dynamic (not cached)
        assert!(blocks[1].cache_control.is_none());
    } else {
        panic!("Expected Blocks variant");
    }
}

// --- Cross-turn cache correctness tests ---
// These tests verify the two-marker sliding-window strategy that allows each turn
// to READ from the previous turn's conversation cache.

fn count_message_cache_breakpoints(messages: &[ApiMessage]) -> usize {
    messages
        .iter()
        .flat_map(|m| &m.content)
        .filter(|b| {
            matches!(
                b,
                ApiContentBlock::Text {
                    cache_control: Some(_),
                    ..
                } | ApiContentBlock::ToolUse {
                    cache_control: Some(_),
                    ..
                }
            )
        })
        .count()
}

fn cached_message_indices(messages: &[ApiMessage]) -> Vec<usize> {
    messages
        .iter()
        .enumerate()
        .filter(|(_, m)| {
            m.content.iter().any(|b| {
                matches!(
                    b,
                    ApiContentBlock::Text {
                        cache_control: Some(_),
                        ..
                    } | ApiContentBlock::ToolUse {
                        cache_control: Some(_),
                        ..
                    }
                )
            })
        })
        .map(|(i, _)| i)
        .collect()
}

/// Helper to build a minimal conversation with N exchanges (user→assistant pairs).
/// Returns messages suitable for add_message_cache_breakpoint (includes a trailing user msg).
fn build_conversation(exchanges: usize) -> Vec<ApiMessage> {
    let mut messages = vec![ApiMessage {
        role: "user".to_string(),
        content: vec![ApiContentBlock::Text {
            text: "identity".to_string(),
            cache_control: None,
        }],
    }];
    for i in 0..exchanges {
        messages.push(ApiMessage {
            role: "user".to_string(),
            content: vec![ApiContentBlock::Text {
                text: format!("Question {}", i + 1),
                cache_control: None,
            }],
        });
        messages.push(ApiMessage {
            role: "assistant".to_string(),
            content: vec![ApiContentBlock::Text {
                text: format!("Answer {}", i + 1),
                cache_control: None,
            }],
        });
    }
    // Trailing user message (the current turn's input)
    messages.push(ApiMessage {
        role: "user".to_string(),
        content: vec![ApiContentBlock::Text {
            text: format!("Question {}", exchanges + 1),
            cache_control: None,
        }],
    });
    messages
}

#[test]
fn test_cache_one_exchange_single_marker() {
    // Turn 2: only one assistant reply exists → one marker (WRITE only)
    let mut messages = build_conversation(1);
    add_message_cache_breakpoint(&mut messages);

    let indices = cached_message_indices(&messages);
    assert_eq!(indices.len(), 1, "One assistant message → one cache marker");
    // The assistant message is at index 2 (identity=0, user=1, assistant=2, user=3)
    assert_eq!(indices[0], 2);
}

#[test]
fn test_cache_two_exchanges_two_markers() {
    // Turn 3: two assistant replies → two markers (READ prev + WRITE new)
    let mut messages = build_conversation(2);
    // identity=0, user=1, assistant=2, user=3, assistant=4, user=5
    add_message_cache_breakpoint(&mut messages);

    let indices = cached_message_indices(&messages);
    assert_eq!(
        indices.len(),
        2,
        "Two assistant messages → two cache markers"
    );
    assert!(
        indices.contains(&2),
        "Second-to-last assistant (READ marker) at index 2"
    );
    assert!(
        indices.contains(&4),
        "Last assistant (WRITE marker) at index 4"
    );
}

#[test]
fn test_cache_many_exchanges_still_two_markers() {
    // 10 exchanges → still only 2 markers (within the 4-breakpoint API limit)
    let mut messages = build_conversation(10);
    add_message_cache_breakpoint(&mut messages);

    let count = count_message_cache_breakpoints(&messages);
    assert_eq!(
        count, 2,
        "Should always place exactly 2 markers regardless of conversation length"
    );
}

#[test]
fn test_cache_cross_turn_read_marker_preserved() {
    // THE KEY REGRESSION TEST: simulates turn N → turn N+1 and verifies that the
    // assistant message from turn N still has cache_control in the turn N+1 request.
    // Without this, the turn N cache snapshot is written but never read.

    // Turn 2: one assistant reply
    let mut turn2 = build_conversation(1);
    // identity=0, user=1, assistant=2, user=3
    add_message_cache_breakpoint(&mut turn2);
    let turn2_cached = cached_message_indices(&turn2);
    assert_eq!(
        turn2_cached,
        vec![2],
        "Turn 2: cache marker at assistant index 2"
    );

    // The content of the assistant message from turn 2 (what gets written to cache)
    let cached_text = match &turn2[2].content[0] {
        ApiContentBlock::Text { text, .. } => text.clone(),
        _ => panic!("Expected text block"),
    };

    // Turn 3: same conversation + one more exchange (assistant[2] is now second-to-last)
    let mut turn3 = build_conversation(2);
    // identity=0, user=1, assistant=2(same as before), user=3, assistant=4(new), user=5
    add_message_cache_breakpoint(&mut turn3);
    let turn3_cached = cached_message_indices(&turn3);

    // CRITICAL: assistant at index 2 MUST still have cache_control in turn 3,
    // so Anthropic can serve a cache READ hit for the turn-2 snapshot.
    assert!(
        turn3_cached.contains(&2),
        "Turn 3 MUST keep cache_control on the turn-2 assistant message (index 2) \
             so Anthropic can serve a cache_read hit. Without this, turn-2's cache is \
             written but never read, wasting cache_creation tokens every turn."
    );
    assert!(
        turn3_cached.contains(&4),
        "Turn 3 must add cache_control on the new assistant message (index 4) to \
             write a fresh cache snapshot for turn 4 to read"
    );

    // Verify it's actually the same content (same assistant message, not a different one)
    match &turn3[2].content[0] {
        ApiContentBlock::Text {
            text,
            cache_control,
        } => {
            assert_eq!(text, &cached_text);
            assert!(cache_control.is_some(), "Must have cache_control set");
        }
        _ => panic!("Expected text block"),
    }
}

#[test]
fn test_cache_non_oauth_path_gets_breakpoints() {
    // Non-OAuth path should now also get conversation cache breakpoints
    // (previously it returned early without calling add_message_cache_breakpoint)
    let messages = vec![
        ApiMessage {
            role: "user".to_string(),
            content: vec![ApiContentBlock::Text {
                text: "Hello".to_string(),
                cache_control: None,
            }],
        },
        ApiMessage {
            role: "assistant".to_string(),
            content: vec![ApiContentBlock::Text {
                text: "Hi there!".to_string(),
                cache_control: None,
            }],
        },
        ApiMessage {
            role: "user".to_string(),
            content: vec![ApiContentBlock::Text {
                text: "Follow-up".to_string(),
                cache_control: None,
            }],
        },
    ];

    let result = format_messages_with_identity(messages, false);
    let indices = cached_message_indices(&result);
    assert_eq!(
        indices,
        vec![1],
        "Non-OAuth path should add cache breakpoint to assistant message"
    );
}

#[test]
fn test_cache_total_breakpoints_within_api_limit() {
    // Anthropic allows at most 4 cache_control parameters per request total
    // (system blocks + tool definitions + message blocks).
    // System: 1 (static block) + Tools: 1 (last tool) + Messages: up to 2 = 4 max.
    // This test verifies messages never exceed 2 breakpoints.
    for exchanges in 1..=20 {
        let mut messages = build_conversation(exchanges);
        add_message_cache_breakpoint(&mut messages);
        let count = count_message_cache_breakpoints(&messages);
        assert!(
            count <= 2,
            "Conversation with {} exchanges produced {} message breakpoints, exceeding \
                 the 2-message budget (system+tools use the other 2 of Anthropic's 4-limit)",
            exchanges,
            count
        );
    }
}

#[tokio::test]
async fn test_sanitize_tool_ids_with_dots() {
    let provider = AnthropicProvider::new();

    let messages = vec![
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Hello".to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "chatcmpl-BF2xX.tool_call.0".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "ls"}),
                thought_signature: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "chatcmpl-BF2xX.tool_call.0".to_string(),
                content: "file1.txt".to_string(),
                is_error: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
    ];

    let formatted = provider.format_messages(&messages, false);

    let sanitized_id = "chatcmpl-BF2xX_tool_call_0";
    for msg in &formatted {
        for block in &msg.content {
            match block {
                ApiContentBlock::ToolUse { id, .. } => {
                    assert_eq!(id, sanitized_id);
                }
                ApiContentBlock::ToolResult { tool_use_id, .. } => {
                    assert_eq!(tool_use_id, sanitized_id);
                }
                _ => {}
            }
        }
    }
}

#[tokio::test]
async fn test_sanitize_dangling_tool_ids_with_dots() {
    let provider = AnthropicProvider::new();

    let messages = vec![
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Hello".to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call.with.dots".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "crash"}),
                thought_signature: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
    ];

    let formatted = provider.format_messages(&messages, false);

    let sanitized_id = "call_with_dots";
    for msg in &formatted {
        for block in &msg.content {
            match block {
                ApiContentBlock::ToolUse { id, .. } => {
                    assert_eq!(id, sanitized_id);
                }
                ApiContentBlock::ToolResult { tool_use_id, .. } => {
                    assert_eq!(tool_use_id, sanitized_id);
                }
                _ => {}
            }
        }
    }
}

/// The runtime-provider identity that `set_credential_mode` writes must decode
/// back to the exact same credential mode. This guards the model picker / header
/// widget from reporting OAuth when an API key is in use (or vice versa): the
/// env key is the single source of truth those surfaces read, so an asymmetric
/// mapping here would surface an inaccurate auth method to the user.
#[test]
fn credential_mode_runtime_provider_identity_round_trips() {
    let _guard = jcode_base::storage::lock_test_env();
    let previous = std::env::var_os("JCODE_RUNTIME_PROVIDER");

    jcode_base::env::set_var("JCODE_RUNTIME_PROVIDER", "claude");
    assert_eq!(
        AnthropicCredentialMode::from_runtime_env(jcode_provider_core::DualAuthProvider::Anthropic),
        AnthropicCredentialMode::OAuth,
        "OAuth selection must surface as the OAuth runtime identity"
    );

    jcode_base::env::set_var("JCODE_RUNTIME_PROVIDER", "claude-api");
    assert_eq!(
        AnthropicCredentialMode::from_runtime_env(jcode_provider_core::DualAuthProvider::Anthropic),
        AnthropicCredentialMode::ApiKey,
        "API-key selection must surface as the API-key runtime identity"
    );

    match previous {
        Some(value) => jcode_base::env::set_var("JCODE_RUNTIME_PROVIDER", value),
        None => jcode_base::env::remove_var("JCODE_RUNTIME_PROVIDER"),
    }
}

#[tokio::test]
async fn auto_mode_falls_back_to_api_key_when_oauth_is_expired() {
    let _guard = jcode_base::storage::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let _home = EnvVarGuard::set("JCODE_HOME", temp.path());
    let _api_key = EnvVarGuard::set("ANTHROPIC_API_KEY", "test-anthropic-api-key");
    let _runtime = EnvVarGuard::set("JCODE_RUNTIME_PROVIDER", "auto");

    jcode_base::auth::claude::upsert_account(jcode_base::auth::claude::AnthropicAccount {
        label: "claude-1".to_string(),
        access: "expired-oauth-access".to_string(),
        refresh: String::new(),
        expires: 0,
        email: None,
        subscription_type: Some("max".to_string()),
        scopes: vec!["user:inference".to_string()],
    })
    .unwrap();

    let provider = AnthropicProvider::new();
    assert_eq!(
        provider.credential_mode_snapshot(),
        AnthropicCredentialMode::Auto
    );

    let (token, is_oauth) = provider.get_access_token().await.unwrap();
    assert_eq!(token, "test-anthropic-api-key");
    assert!(
        !is_oauth,
        "automatic fallback must use API-key request semantics"
    );
}

#[tokio::test]
async fn explicit_oauth_mode_does_not_silently_fall_back_to_api_key() {
    let _guard = jcode_base::storage::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let _home = EnvVarGuard::set("JCODE_HOME", temp.path());
    let _api_key = EnvVarGuard::set("ANTHROPIC_API_KEY", "test-anthropic-api-key");
    let _runtime = EnvVarGuard::set("JCODE_RUNTIME_PROVIDER", "claude");

    jcode_base::auth::claude::upsert_account(jcode_base::auth::claude::AnthropicAccount {
        label: "claude-1".to_string(),
        access: "expired-oauth-access".to_string(),
        refresh: String::new(),
        expires: 0,
        email: None,
        subscription_type: Some("max".to_string()),
        scopes: vec!["user:inference".to_string()],
    })
    .unwrap();

    let provider = AnthropicProvider::new();
    assert_eq!(
        provider.credential_mode_snapshot(),
        AnthropicCredentialMode::OAuth
    );

    let error = provider.get_access_token().await.unwrap_err().to_string();
    assert!(error.contains("expired"), "unexpected error: {error}");
}

#[test]
fn test_anthropic_fable_5_sends_reasoning_fields() {
    // `claude-fable-5` rejected reasoning fields during its preview, but the
    // released model accepts an adaptive `thinking` block and an
    // `output_config` effort (verified live 2026-07-01). The request builder
    // must send both when an effort is configured.
    let provider = AnthropicProvider::new();
    *provider.reasoning_effort.write().unwrap() = Some("high".to_string());

    let (thinking, output_config, temperature) =
        provider.build_reasoning_request_parts_inner("claude-fable-5", true, false);
    assert!(
        matches!(thinking, Some(ApiThinking::Adaptive { .. })),
        "Fable 5 should send an adaptive thinking block"
    );
    assert_eq!(
        output_config.as_ref().map(|c| c.effort.as_str()),
        Some("high"),
        "Fable 5 should send the configured output_config effort"
    );
    assert_eq!(temperature, None);

    // Fable 5 supports the real `max` API level, so `max` is sent verbatim.
    *provider.reasoning_effort.write().unwrap() = Some("max".to_string());
    let (_thinking, output_config, _temp) =
        provider.build_reasoning_request_parts_inner("claude-fable-5", true, false);
    assert_eq!(
        output_config.as_ref().map(|c| c.effort.as_str()),
        Some("max")
    );

    // The effort picker surfaces levels for Fable 5.
    assert!(AnthropicProvider::model_supports_reasoning_effort(
        "claude-fable-5"
    ));
}

#[test]
fn detects_anthropic_reasoning_unsupported_errors() {
    // The real 400 bodies returned when Fable 5 is sent reasoning fields.
    let thinking_400 = "anthropic api error (400 bad request): {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"adaptive thinking is not supported on this model\"}}";
    assert!(is_reasoning_unsupported_error(thinking_400));
    let effort_400 = "anthropic api error (400 bad request): {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"this model does not support the effort parameter.\"}}";
    assert!(is_reasoning_unsupported_error(effort_400));

    // Unrelated 400s must not trigger the reasoning self-heal path.
    assert!(!is_reasoning_unsupported_error(
        "anthropic api error (400 bad request): {\"type\":\"invalid_request_error\",\"message\":\"max_tokens too large\"}"
    ));
    // A thinking-mentioning error that is not a 400 must not match either.
    assert!(!is_reasoning_unsupported_error(
        "anthropic api error (429 too many requests): rate_limit on thinking budget"
    ));
    // Model-not-found is a different recovery path.
    assert!(!is_reasoning_unsupported_error(
        "anthropic api error (404 not found): {\"type\":\"not_found_error\",\"message\":\"model not found\"}"
    ));
}

#[test]
fn detects_anthropic_model_not_found_errors() {
    // The real 404 body returned when a model id was retired (e.g. Fable 5).
    let real = "anthropic api error (404 not found): {\"type\":\"error\",\"error\":{\"type\":\"not_found_error\",\"message\":\"claude fable 5 is not available. please use opus 4.8.\"}}";
    assert!(is_model_not_found_error(real));

    // Structural marker alone (lowercased error chain).
    assert!(is_model_not_found_error(
        "model claude-foo not found (not_found_error)"
    ));

    // Unrelated failures must not trigger the model fallback path.
    assert!(!is_model_not_found_error(
        "anthropic api error (401 unauthorized): invalid authentication credentials"
    ));
    assert!(!is_model_not_found_error(
        "anthropic api error (429 too many requests): rate_limit"
    ));
    assert!(!is_model_not_found_error(
        "anthropic api error (404 not found): resource missing"
    ));
}

#[test]
fn anthropic_fallback_prefers_best_available_and_skips_tried_and_retired() {
    // The fallback logic reads the process-global model catalog; lock and
    // reset it so fixture models hydrated by other tests cannot leak in.
    let _guard = jcode_base::storage::lock_test_env();
    jcode_base::provider::models::reset_model_catalog_services_for_tests();
    let known = jcode_base::provider::known_anthropic_model_ids();
    assert!(
        !known.is_empty(),
        "expected a non-empty Anthropic model catalog"
    );

    // With nothing tried, the fallback offers the highest-quality (flagship)
    // model, NOT merely the first catalog entry. The curated order ranks Opus
    // ahead of Haiku, so the chosen model must not be a Haiku/retired tier when
    // a stronger one exists.
    let first = anthropic_fallback_model(&[], "").expect("a fallback should exist");
    let first_key = AnthropicProvider::normalized_model_key(&first);
    assert!(
        !first_key.contains("haiku"),
        "fallback must not downgrade to Haiku when a flagship is available, got {first}"
    );
    assert!(
        !anthropic_model_is_retired(&first),
        "fallback must never pick a retired model, got {first}"
    );

    // A retired model in `tried` must never be re-offered, and the result must
    // skip retired families entirely.
    let next = anthropic_fallback_model(&["claude-mythos-1".to_string()], "")
        .expect("another fallback should exist");
    assert!(!anthropic_model_is_retired(&next));

    // Exhausting every viable known model yields None.
    let exhausted = anthropic_fallback_model(&known, "");
    assert!(
        exhausted.is_none(),
        "no fallback should remain once all known models are tried, got {exhausted:?}"
    );
}

#[test]
fn anthropic_fallback_honors_server_recommendation() {
    // The recommendation matcher scores hints against the process-global model
    // catalog; lock and reset it so fixture models hydrated by other tests
    // (e.g. claude-opus-5-preview) cannot outrank the real catalog entries.
    let _guard = jcode_base::storage::lock_test_env();
    jcode_base::provider::models::reset_model_catalog_services_for_tests();
    // The real 404 body recommends a specific replacement model. We must honor
    // it over the generic quality ranking.
    let body = "anthropic api error (404 not found): {\"type\":\"error\",\"error\":{\"type\":\"not_found_error\",\"message\":\"claude fable 5 is not available. please use opus 4.8. learn more: https://anthropic.com\"}}";
    let recommended =
        anthropic_recommended_model_from_error(body).expect("should parse a recommendation");
    assert_eq!(
        AnthropicProvider::normalized_model_key(&recommended),
        "claude-opus-4-8",
        "server recommendation 'Opus 4.8' should map to claude-opus-4-8"
    );

    // The full fallback also returns the recommended model.
    let fallback = anthropic_fallback_model(&["claude-mythos-1".to_string()], body)
        .expect("a fallback should exist");
    assert_eq!(
        AnthropicProvider::normalized_model_key(&fallback),
        "claude-opus-4-8"
    );

    // A recommendation pointing at a retired model is ignored (falls through to
    // quality ranking).
    let retired_rec = "model x not available. please use mythos 1.";
    assert!(
        anthropic_recommended_model_from_error(retired_rec).is_none()
            || !anthropic_model_is_retired(
                &anthropic_recommended_model_from_error(retired_rec).unwrap()
            )
    );

    // No recommendation phrase -> None.
    assert!(anthropic_recommended_model_from_error("429 too many requests").is_none());
}

#[test]
fn anthropic_quality_rank_orders_opus_before_haiku_and_retired_last() {
    let opus = anthropic_model_quality_rank("claude-opus-4-8");
    let sonnet = anthropic_model_quality_rank("claude-sonnet-4-6");
    let haiku = anthropic_model_quality_rank("claude-haiku-4-5");
    let retired = anthropic_model_quality_rank("claude-mythos-1");
    // Fable 5 is live again and curated as the flagship, so it ranks first.
    let fable = anthropic_model_quality_rank("claude-fable-5");
    assert!(
        fable <= opus,
        "Fable 5 should rank at or ahead of Opus ({fable} vs {opus})"
    );
    assert!(
        opus < sonnet,
        "Opus should outrank Sonnet ({opus} vs {sonnet})"
    );
    assert!(
        sonnet < haiku,
        "Sonnet should outrank Haiku ({sonnet} vs {haiku})"
    );
    assert!(
        haiku < retired,
        "retired models must sort last ({haiku} vs {retired})"
    );
    assert_eq!(retired, usize::MAX);
    // Dated live ids must rank like their canonical base.
    assert_eq!(
        anthropic_model_quality_rank("claude-haiku-4-5-20251001"),
        haiku
    );
}

#[test]
fn ping_keepalive_emits_streaming_phase_event() {
    // Issue #451: during silent reasoning phases, `ping` events can be the
    // only upstream traffic. They must surface as a StreamEvent so the client
    // stall guard sees activity instead of cancelling a healthy stream.
    let mut state = SseStreamState::default();
    let event = SseEvent {
        event_type: "ping".to_string(),
        data: r#"{"type": "ping"}"#.to_string(),
    };
    let events = process_sse_event(&event, &mut state, true);
    assert!(
        events.iter().any(|e| matches!(
            e,
            StreamEvent::ConnectionPhase {
                phase: jcode_message_types::ConnectionPhase::Streaming
            }
        )),
        "expected ping to emit a Streaming ConnectionPhase event, got {events:?}"
    );
}

#[test]
fn test_anthropic_opus_5_low_effort_reaches_the_wire() {
    // Benchmark campaigns pin `claude-opus-5` at `low` effort. Opus 5 also
    // *defaults* to `low` (jcode's default model/effort pairing), and an
    // explicit `low` must survive normalization, must NOT be silently
    // promoted, and must land in `output_config.effort` on the request.
    assert!(AnthropicProvider::model_supports_output_effort(
        "claude-opus-5"
    ));
    assert_eq!(
        AnthropicProvider::default_reasoning_effort_for_model("claude-opus-5").as_deref(),
        Some("low"),
    );
    assert_eq!(
        AnthropicProvider::normalize_reasoning_effort("low").as_deref(),
        Some("low"),
    );
    // Downward selection is never clamped upward toward the model default.
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-opus-5", "low"),
        "low",
    );
    assert_eq!(
        AnthropicProvider::store_effort_for_model("claude-opus-5", "low"),
        "low",
    );

    let provider = AnthropicProvider::new();
    *provider
        .model
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = "claude-opus-5".to_string();
    provider.set_reasoning_effort("low").unwrap();
    assert_eq!(provider.reasoning_effort().as_deref(), Some("low"));

    let (thinking, output_config, _temp) =
        provider.build_reasoning_request_parts_inner("claude-opus-5", true, false);
    assert_eq!(
        output_config
            .expect("explicit low effort should set output_config")
            .effort,
        "low",
    );
    // Opus 5 rejects `thinking.type.enabled`; it requires adaptive thinking.
    assert!(matches!(thinking, Some(ApiThinking::Adaptive { .. })));
}

/// A `content_block_start` carrying an unrecognized block type must still
/// deserialize. Before the `Unknown` catch-all the whole event failed to parse
/// and was dropped, so an unknown *tool* block produced a turn that reported
/// `stop_reason: tool_use` with no tool call for the agent to run.
#[test]
fn test_anthropic_unknown_content_block_start_does_not_drop_event() {
    for block_type in [
        "server_tool_use",
        "web_search_tool_result",
        "some_future_block",
    ] {
        let mut state = SseStreamState::default();
        let event = SseEvent {
            event_type: "content_block_start".to_string(),
            data: serde_json::json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": block_type, "id": "srvtoolu_1", "name": "web_search"}
            })
            .to_string(),
        };
        let events = process_sse_event(&event, &mut state, false);
        assert!(
            events.is_empty(),
            "{block_type}: unknown block must not synthesize stream events"
        );
        assert!(
            state.current_tool_use.is_none(),
            "{block_type}: unknown block must not start tool accumulation"
        );
        assert!(
            !state.current_thinking_block,
            "{block_type}: unknown block must not start a thinking block"
        );
    }
}

#[test]
fn next_untried_account_skips_already_attempted_labels() {
    let candidates = vec![
        "claude-1".to_string(),
        "claude-2".to_string(),
        "claude-3".to_string(),
    ];
    let tried = vec!["claude-1".to_string(), "claude-2".to_string()];

    assert_eq!(
        next_untried_account(&candidates, &tried),
        Some("claude-3"),
        "rotation must move past every account the request already used"
    );
}

#[test]
fn next_untried_account_returns_first_candidate_when_nothing_tried() {
    let candidates = vec!["claude-2".to_string(), "claude-1".to_string()];

    assert_eq!(
        next_untried_account(&candidates, &[]),
        Some("claude-2"),
        "candidates arrive best-first, so the head is the preferred account"
    );
}

#[test]
fn next_untried_account_is_none_when_every_account_is_exhausted() {
    let candidates = vec!["claude-1".to_string(), "claude-2".to_string()];
    let tried = vec!["claude-2".to_string(), "claude-1".to_string()];

    assert_eq!(
        next_untried_account(&candidates, &tried),
        None,
        "with no account left the caller must fall through to model fallback"
    );
}

#[test]
fn rate_limit_errors_are_recognized_as_usage_limits() {
    let error_str = "anthropic api error (429 too many requests): {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"this request would exceed your account's rate limit.\"}}";

    assert!(
        jcode_base::provider::error_looks_like_usage_limit(error_str),
        "the 429 body jcode logs must trigger account rotation"
    );
    assert!(
        is_retryable_error(error_str),
        "429 stays retryable so the rotated account gets an attempt"
    );
}

#[test]
fn should_rotate_account_only_on_usage_limits() {
    let rate_limit = "anthropic api error (429 too many requests)";

    assert!(
        should_rotate_account(true, rate_limit),
        "a 429 is exactly the case account rotation exists for"
    );
    assert!(
        !should_rotate_account(false, rate_limit),
        "rotation must stay off when prefer_account_failover is disabled or auth is an API key"
    );
    assert!(
        !should_rotate_account(true, "error sending request: connection reset by peer"),
        "transport faults belong to the retry loop, not to another account"
    );
    assert!(
        !should_rotate_account(true, "anthropic api error (500 internal server error)"),
        "a server fault is not an account problem"
    );
}

#[test]
fn account_override_is_restored_only_when_rotation_ran_dry() {
    let original = Some("claude-1".to_string());
    let rotated = Some("claude-2".to_string());

    assert!(
        should_restore_account(&original, &rotated, true),
        "every account limited: hand the override back rather than pinning a searched-through account"
    );
    assert!(
        !should_restore_account(&original, &rotated, false),
        "a rotation that found a working account must survive an unrelated later failure"
    );
    assert!(
        !should_restore_account(&original, &original, true),
        "nothing to undo when the request never left its starting account"
    );
    assert!(
        !should_restore_account(&None, &None, true),
        "no override was set, so none may be written"
    );
}

// ---------------------------------------------------------------------------
// End-to-end account rotation over real HTTP, against a hand-rolled mock
// server. `API_URL`/`API_URL_OAUTH` are `const`, so `set_test_api_url_override`
// (test-only, compiled out of release builds; see `api_url` in `lib.rs`) is the
// seam that lets `run_stream_with_retries` be driven exactly as production
// calls it, with a local listener standing in for api.anthropic.com. No mock
// server crate is added: the workspace has none (wiremock/httpmock/mockito are
// all absent from every `Cargo.toml` and `Cargo.lock`), and a few dozen lines
// of raw HTTP/1.1 framing is small enough not to be worth a new dependency for
// one test file.
//
// Fixture tokens below (`fixture-access-claude-*`) are not real credentials;
// nothing derived from a real account is ever read, logged, or asserted here.
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Minimal single-purpose HTTP/1.1 responder standing in for the Anthropic
/// Messages endpoint. Each accepted connection is one request/response pair:
/// headers and the declared `Content-Length` body are read (so the client's
/// `write_all` never blocks on a peer that stopped reading), the
/// `Authorization` header is captured for the test to inspect, and a
/// `respond` callback decides the status/body by 1-based call number.
struct MockAnthropicServer {
    addr: std::net::SocketAddr,
    authorizations: Arc<std::sync::Mutex<Vec<String>>>,
    handle: tokio::task::JoinHandle<()>,
}

impl MockAnthropicServer {
    async fn start(mut respond: impl FnMut(u32) -> (u16, String) + Send + 'static) -> Self {
        Self::start_with_request_inspection(move |call_no, _authorization| respond(call_no)).await
    }

    /// Like [`Self::start`], but the responder also sees the request's
    /// `Authorization` header, so a test can answer differently per *account*
    /// rather than per call number. Needed to observe which account a rotation
    /// actually landed on, which call-ordering alone cannot distinguish.
    async fn start_with_request_inspection(
        mut respond: impl FnMut(u32, &str) -> (u16, String) + Send + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let authorizations = Arc::new(std::sync::Mutex::new(Vec::new()));
        let authorizations_task = Arc::clone(&authorizations);

        let handle = tokio::spawn(async move {
            let mut call_no: u32 = 0;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                call_no += 1;
                let authorization = read_request_authorization(&mut socket).await;
                authorizations_task
                    .lock()
                    .unwrap()
                    .push(authorization.clone());

                let (status, body) = respond(call_no, &authorization);
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\ncontent-type: text/event-stream\r\ncontent-length: {len}\r\nconnection: close\r\n\r\n{body}",
                    status = status,
                    reason = canonical_reason(status),
                    len = body.len(),
                    body = body,
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });

        Self {
            addr,
            authorizations,
            handle,
        }
    }

    fn url(&self) -> String {
        format!("http://{}/v1/messages", self.addr)
    }

    fn call_count(&self) -> usize {
        self.authorizations.lock().unwrap().len()
    }

    /// Whether the bearer token on request `index` (0-based) differs from the
    /// one on request 0. Never returns or prints the token itself.
    fn authorization_changed_from_first(&self, index: usize) -> bool {
        let seen = self.authorizations.lock().unwrap();
        match (seen.first(), seen.get(index)) {
            (Some(first), Some(other)) => first != other,
            _ => false,
        }
    }

    fn authorization_contains(&self, index: usize, needle: &str) -> bool {
        self.authorizations
            .lock()
            .unwrap()
            .get(index)
            .is_some_and(|auth| auth.contains(needle))
    }

    fn stop(self) {
        self.abort();
    }

    /// Same teardown as [`Self::stop`] without consuming the server, so an
    /// owning fixture can abort it from `Drop`.
    fn abort(&self) {
        self.handle.abort();
    }
}

fn canonical_reason(status: u16) -> &'static str {
    match status {
        429 => "Too Many Requests",
        200 => "OK",
        _ => "Error",
    }
}

async fn read_request_authorization(socket: &mut tokio::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let headers_end = loop {
        let n = socket.read(&mut tmp).await.unwrap_or(0);
        if n == 0 {
            return String::new();
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
    };
    let header_text = String::from_utf8_lossy(&buf[..headers_end]).to_string();
    let content_length: usize = header_text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())
                .flatten()
        })
        .unwrap_or(0);
    let body_start = headers_end + 4;
    while buf.len() < body_start + content_length {
        let n = socket.read(&mut tmp).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    header_text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("authorization")
                .then(|| value.trim().to_string())
        })
        .unwrap_or_default()
}

fn mock_429_body() -> String {
    "{\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"this request would exceed your account's rate limit.\"}}".to_string()
}

fn mock_success_sse_body(text: &str) -> String {
    format!(
        concat!(
            "event: message_start\n",
            "data: {{\"type\":\"message_start\",\"message\":{{\"model\":\"claude-3-5-haiku-20241022\",\"usage\":{{\"input_tokens\":5}}}}}}\n\n",
            "event: content_block_start\n",
            "data: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\n",
            "event: content_block_delta\n",
            "data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{text}\"}}}}\n\n",
            "event: content_block_stop\n",
            "data: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n",
            "event: message_delta\n",
            "data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":1}}}}\n\n",
            "event: message_stop\n",
            "data: {{\"type\":\"message_stop\"}}\n\n",
        ),
        text = text,
    )
}

/// Seeds two fake Claude accounts (`claude-1` active, `claude-2` the only
/// rotation candidate) in the sandboxed `JCODE_HOME`. Caller must hold
/// `jcode_base::storage::lock_test_env()` and have already pointed
/// `JCODE_HOME` at a temp dir, exactly like the credential-mode tests above.
fn seed_two_claude_accounts() {
    let far_future = chrono::Utc::now().timestamp_millis() + 3_600_000;
    for label in ["claude-1", "claude-2"] {
        jcode_base::auth::claude::upsert_account(jcode_base::auth::claude::AnthropicAccount {
            label: label.to_string(),
            access: format!("fixture-access-{label}"),
            refresh: String::new(),
            expires: far_future,
            email: None,
            subscription_type: Some("max".to_string()),
            scopes: vec![],
        })
        .unwrap();
    }
    jcode_base::auth::claude::set_active_account("claude-1").unwrap();
}

fn e2e_test_request() -> ApiRequest {
    ApiRequest {
        model: "claude-3-5-haiku-20241022".to_string(),
        max_tokens: 16,
        system: None,
        messages: vec![],
        tools: None,
        metadata: None,
        thinking: None,
        output_config: None,
        temperature: None,
        service_tier: None,
        stream: true,
    }
}

/// Fixture for the account-rotation streaming tests: seeds two fake Claude
/// accounts (`claude-1` active, `claude-2` the only rotation candidate) in a
/// sandboxed `JCODE_HOME`, and runs a mock server that 429s on the first call
/// then answers 200 on every subsequent one.
///
/// Holds the test-env lock and temp dir for its own lifetime so callers don't
/// need to manage either. `drive_stream_to_completion` runs
/// `run_stream_with_retries` against the mock server and returns every event.
///
/// Field order matters: struct fields drop in declaration order (unlike local
/// variables, which drop in reverse), so `_guard` is declared last to release
/// the test-env lock only after the temp dir, env var, and mock server have
/// already torn down.
struct RotationFixture {
    server: MockAnthropicServer,
    _home: EnvVarGuard,
    _temp: tempfile::TempDir,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Drop for RotationFixture {
    fn drop(&mut self) {
        // The pre-fixture version of these tests ended with an explicit
        // `server.stop()`. Without this the listener task outlives every
        // rotation test and keeps its port bound for the rest of the run.
        self.server.abort();
    }
}

impl RotationFixture {
    async fn new() -> Self {
        let _guard = jcode_base::storage::lock_test_env();
        let temp = tempfile::TempDir::new().unwrap();
        let _home = EnvVarGuard::set("JCODE_HOME", temp.path());
        seed_two_claude_accounts();

        assert_eq!(
            jcode_base::provider::account_failover_candidates(
                jcode_provider_core::ActiveProvider::Claude
            ),
            vec!["claude-2".to_string()],
            "fixture must leave exactly one rotation candidate"
        );

        let server = MockAnthropicServer::start(|call_no| {
            if call_no == 1 {
                (429, mock_429_body())
            } else {
                (200, mock_success_sse_body("pong"))
            }
        })
        .await;
        set_test_api_url_override(Some(server.url()));

        Self {
            server,
            _home,
            _temp: temp,
            _guard,
        }
    }

    async fn drive_stream_to_completion(&self) -> Vec<Result<StreamEvent>> {
        let (tx, mut rx) = mpsc::channel::<Result<StreamEvent>>(100);
        let model_state = Arc::new(std::sync::RwLock::new(
            "claude-3-5-haiku-20241022".to_string(),
        ));

        run_stream_with_retries(
            Client::new(),
            "fixture-access-claude-1".to_string(),
            true,
            e2e_test_request(),
            tx,
            Arc::new(RwLock::new(None)),
            "claude-3-5-haiku-20241022".to_string(),
            "test-session".to_string(),
            model_state,
            DirectTransportConfig::from_env(),
        )
        .await;

        set_test_api_url_override(None);

        let mut events = Vec::new();
        while let Some(item) = rx.recv().await {
            events.push(item);
        }
        events
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_stream_with_retries_rotates_account_on_streaming_429_and_replays_successfully() {
    let fixture = RotationFixture::new().await;
    let events = fixture.drive_stream_to_completion().await;

    for event in &events {
        assert!(
            event.is_ok(),
            "stream must succeed once the retry replays on a working account"
        );
    }

    let switched = events.iter().any(|event| {
        matches!(
            event,
            Ok(StreamEvent::StatusDetail { detail })
                if detail.contains("claude-1") && detail.contains("claude-2")
        )
    });
    assert!(
        switched,
        "the account switch must be surfaced to the consumer via StatusDetail"
    );

    let text_deltas: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            Ok(StreamEvent::TextDelta(text)) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        text_deltas,
        vec!["pong"],
        "the replay must stream the successful response exactly once, not duplicated"
    );

    assert_eq!(
        fixture.server.call_count(),
        2,
        "one 429 on the original account, one successful replay on the rotated account"
    );
    assert!(
        fixture.server.authorization_changed_from_first(1),
        "the replay must carry a different bearer token than the first attempt"
    );
    assert!(
        fixture
            .server
            .authorization_contains(0, "fixture-access-claude-1")
    );
    assert!(
        fixture
            .server
            .authorization_contains(1, "fixture-access-claude-2")
    );

    assert_eq!(
        jcode_base::auth::claude::active_account_label().as_deref(),
        Some("claude-2"),
        "a rotation that lands on a working account must stick, not get restored"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rotation_publishes_an_account_rotated_bus_event() {
    let fixture = RotationFixture::new().await;
    let mut bus_rx = jcode_base::bus::Bus::global().subscribe();

    fixture.drive_stream_to_completion().await;

    let mut rotations = Vec::new();
    while let Ok(event) = bus_rx.try_recv() {
        if let jcode_base::bus::BusEvent::AnthropicAccountRotated(rotated) = event {
            rotations.push((
                rotated.previous_label.clone(),
                rotated.label.clone(),
                rotated.restored,
            ));
        }
    }

    assert_eq!(
        rotations,
        vec![(Some("claude-1".to_string()), "claude-2".to_string(), false)],
        "a rotation the client cannot otherwise observe must be announced exactly once, and \
         landing on a working account is not the give-up path"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_stream_with_retries_bounds_rotation_attempts_and_restores_original_account() {
    let _guard = jcode_base::storage::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let _home = EnvVarGuard::set("JCODE_HOME", temp.path());
    seed_two_claude_accounts();
    // Force the quota verdict rather than depending on ambient cache state.
    //
    // The per-account usage cache is process-global and keyed by label, so any
    // other test that seeds or probes `claude-1`/`claude-2` decides this test's
    // quota-versus-burst branch for it. Seeding the cache here is not enough
    // either, because a detached post-rotation refresh from another test can
    // overwrite it mid-run. The thread-local override is the seam built for
    // exactly this (see `with_quota_exhausted_override`), and `false` states the
    // premise of this test: nothing is out of quota, so every 429 is a burst.
    jcode_base::provider::clear_all_account_cooldowns();

    let candidates = jcode_base::provider::account_failover_candidates(
        jcode_provider_core::ActiveProvider::Claude,
    );
    assert_eq!(
        candidates.len(),
        1,
        "fixture must leave exactly one rotation candidate"
    );

    let server = MockAnthropicServer::start(|_call_no| (429, mock_429_body())).await;
    set_test_api_url_override(Some(server.url()));

    let (tx, mut rx) = mpsc::channel::<Result<StreamEvent>>(100);
    let model_state = Arc::new(std::sync::RwLock::new(
        "claude-3-5-haiku-20241022".to_string(),
    ));

    // The override is thread-local and the runtime is multi-threaded, so it is
    // set inside the same future that runs the loop.
    jcode_base::provider::with_quota_exhausted_override(Some(false), || {
        futures::executor::block_on(run_stream_with_retries(
            Client::new(),
            "fixture-access-claude-1".to_string(),
            true,
            e2e_test_request(),
            tx,
            Arc::new(RwLock::new(None)),
            "claude-3-5-haiku-20241022".to_string(),
            "test-session".to_string(),
            model_state,
            DirectTransportConfig::from_env(),
        ));
    });

    set_test_api_url_override(None);

    let mut events = Vec::new();
    while let Some(item) = rx.recv().await {
        events.push(item);
    }

    let error_count = events.iter().filter(|event| event.is_err()).count();
    assert_eq!(
        error_count, 1,
        "exactly one terminal error once every account and retry is exhausted"
    );

    // attempt_budget starts at MAX_RETRIES and gains exactly one extra attempt
    // per successful rotation, plus one per rate-limit wait once rotation is
    // exhausted (see docs/adr/0015: a burst limit needs a seconds-scale wait,
    // not the ~200ms transport backoff).
    //
    // The number itself matters less than the property: every branch that grows
    // the budget is bounded by its own counter, so an always-429 world stops
    // instead of spinning forever.
    let expected_calls = (MAX_RETRIES + 1 + (MAX_RATE_LIMIT_RETRIES - 1)) as usize;
    assert_eq!(
        server.call_count(),
        expected_calls,
        "the rotation+retry loop must terminate at the sized attempt budget instead of looping forever"
    );
    assert!(
        server.call_count() < 20,
        "sanity: the budget must stay small enough that a user is not left waiting through an unbounded retry storm"
    );

    assert_eq!(
        jcode_base::auth::claude::active_account_label().as_deref(),
        Some("claude-1"),
        "every account limited: the original account override must be restored"
    );

    server.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn switch_to_next_claude_account_terminates_once_candidates_are_exhausted() {
    let _guard = jcode_base::storage::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let _home = EnvVarGuard::set("JCODE_HOME", temp.path());
    seed_two_claude_accounts();

    let credentials: Arc<RwLock<Option<CachedCredentials>>> = Arc::new(RwLock::new(None));
    let mut tried = vec!["claude-1".to_string()];

    let first = switch_to_next_claude_account(&credentials, &mut tried).await;
    assert_eq!(
        first.as_ref().map(|switch| switch.label.as_str()),
        Some("claude-2"),
        "the only untried candidate must be picked"
    );

    let second = switch_to_next_claude_account(&credentials, &mut tried).await;
    assert!(
        second.is_none(),
        "no candidates left: must return None instead of looping"
    );

    assert_eq!(
        jcode_base::auth::claude::active_account_label().as_deref(),
        Some("claude-2"),
        "a None return must leave the override exactly as the prior successful switch set it"
    );
}

#[test]
fn restore_claude_account_skips_write_when_another_session_changed_the_override() {
    let _guard = jcode_base::storage::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let _home = EnvVarGuard::set("JCODE_HOME", temp.path());
    seed_two_claude_accounts();
    let far_future = chrono::Utc::now().timestamp_millis() + 3_600_000;
    jcode_base::auth::claude::upsert_account(jcode_base::auth::claude::AnthropicAccount {
        label: "claude-3".to_string(),
        access: "fixture-access-claude-3".to_string(),
        refresh: String::new(),
        expires: far_future,
        email: None,
        subscription_type: Some("max".to_string()),
        scopes: vec![],
    })
    .unwrap();
    // Simulate: this request rotated claude-1 -> claude-2, then before the
    // dead-end restore ran, something else (another concurrent request, or a
    // user-issued account switch) pointed the process-wide override at a
    // third account.
    jcode_base::auth::claude::set_active_account_override(Some("claude-3".to_string()));

    restore_claude_account(
        &Some("claude-1".to_string()),
        &Some("claude-2".to_string()),
        true,
    );

    assert_eq!(
        jcode_base::auth::claude::active_account_label().as_deref(),
        Some("claude-3"),
        "a live override that no longer matches ours must not be clobbered"
    );
}

#[test]
fn restore_claude_account_writes_back_when_override_still_matches() {
    let _guard = jcode_base::storage::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let _home = EnvVarGuard::set("JCODE_HOME", temp.path());
    seed_two_claude_accounts();
    jcode_base::auth::claude::set_active_account_override(Some("claude-2".to_string()));

    restore_claude_account(
        &Some("claude-1".to_string()),
        &Some("claude-2".to_string()),
        true,
    );

    assert_eq!(
        jcode_base::auth::claude::active_account_label().as_deref(),
        Some("claude-1"),
        "override still holds our label: restoring the original is safe"
    );
}

#[test]
fn overloaded_errors_are_classified_as_capacity_not_just_transient() {
    // The exact SSE error event Anthropic sends when it has no capacity.
    let sse = "retryable stream error: {\"type\":\"error\",\"error\":{\"details\":null,\"type\":\"overloaded_error\",\"message\":\"overloaded\"},\"request_id\":\"req_011cdjhkfn5srzakavdw2xj6\"}";

    assert!(
        is_overloaded_error(sse),
        "an overloaded_error must take the capacity backoff, not the ~200ms transport backoff"
    );
    assert!(
        is_retryable_error(sse),
        "it must stay retryable so the request is not surfaced as a hard failure"
    );
    assert!(
        is_overloaded_error("anthropic api error (529 ): overloaded"),
        "HTTP 529 is the same capacity signal on the initial response"
    );
    assert!(
        !should_rotate_account(true, sse),
        "an overload is a server capacity problem, not an account quota problem"
    );
}

#[test]
fn a_request_id_containing_529_is_not_mistaken_for_a_capacity_error() {
    // A bare "529" substring occurs inside request ids and token counts, so the
    // capacity match must be anchored on the status text.
    assert!(
        !is_overloaded_error(
            "anthropic api error (400 bad request): {\"request_id\":\"req_011cdj529hkfn\"}"
        ),
        "a request id that happens to contain 529 must not trigger capacity backoff"
    );
}

/// A rate limit that survived account rotation must get a rate-limit-shaped
/// wait, not the ~200ms transport backoff.
///
/// Anthropic enforces a short-window burst limit on top of the 5-hour and
/// weekly quotas, so "every account is rate limited" is usually "every account
/// is busy for a few seconds". The real report that motivated this had 83% of
/// the weekly quota and 97% of the 5-hour window still unused, and the turn
/// still died with a "switch to a smaller model" prompt.
#[test]
fn rate_limits_wait_on_a_rate_limit_clock_not_the_transport_clock() {
    let sse = "anthropic api error (429 too many requests): {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"this request would exceed your account's rate limit. please try again later.\"},\"request_id\":\"req_011cdu3zr7sfucuaqxhrvkdd\"}";

    assert!(
        jcode_base::provider::error_looks_like_usage_limit(sse),
        "the real 429 payload must be recognized as a usage limit"
    );
    assert!(
        !is_overloaded_error(sse),
        "a 429 is not a capacity rejection; it must not borrow that budget"
    );

    // The budget must be bigger than the transport budget, and the clock much
    // slower. With MAX_RETRIES=2 at 200ms, the old path gave up in well under a
    // second, which cannot outlast a burst window.
    assert!(
        MAX_RATE_LIMIT_RETRIES > MAX_RETRIES,
        "a rate limit needs more attempts than a transport fault ({} vs {})",
        MAX_RATE_LIMIT_RETRIES,
        MAX_RETRIES
    );
    assert!(
        RATE_LIMIT_RETRY_BASE_DELAY_MS >= 5000,
        "waiting {}ms is far too eager for a burst limit",
        RATE_LIMIT_RETRY_BASE_DELAY_MS
    );

    // Total wall time the retries can cover, which is what decides whether a
    // burst is outlasted.
    let total_ms: u64 = (1..MAX_RATE_LIMIT_RETRIES)
        .map(|attempt| {
            jcode_provider_core::attempt_tracker::retry_backoff_delay(
                attempt,
                RATE_LIMIT_RETRY_BASE_DELAY_MS,
            )
            .as_millis() as u64
        })
        .sum();
    let old_total_ms: u64 = (1..MAX_RETRIES)
        .map(|attempt| {
            jcode_provider_core::attempt_tracker::retry_backoff_delay(attempt, RETRY_BASE_DELAY_MS)
                .as_millis() as u64
        })
        .sum();

    assert!(
        total_ms >= 30_000,
        "rate-limit retries only cover {total_ms}ms; a burst window can outlast that"
    );
    assert!(
        total_ms > old_total_ms * 10,
        "new budget ({total_ms}ms) must dwarf the transport budget ({old_total_ms}ms) it replaced"
    );
}

/// The outcome that actually matters: a burst limit that clears while jcode is
/// waiting must let the turn finish, not kill it.
///
/// Anthropic's short-window limit clears in seconds. Before this, a 429 that
/// outlived account rotation fell through to the transport budget (2 attempts,
/// ~200ms apart), so the turn died with a "switch to a smaller model" prompt
/// while the user still had most of their weekly quota. Here every account 429s
/// for the first few calls and then recovers, which is exactly that world.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_burst_limit_that_clears_lets_the_turn_finish() {
    let _guard = jcode_base::storage::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let _home = EnvVarGuard::set("JCODE_HOME", temp.path());
    seed_two_claude_accounts();
    // Force the quota verdict rather than depending on ambient cache state.
    //
    // The per-account usage cache is process-global and keyed by label, so any
    // other test that seeds or probes `claude-1`/`claude-2` decides this test's
    // quota-versus-burst branch for it. Seeding the cache here is not enough
    // either, because a detached post-rotation refresh from another test can
    // overwrite it mid-run. The thread-local override is the seam built for
    // exactly this (see `with_quota_exhausted_override`), and `false` states the
    // premise of this test: nothing is out of quota, so every 429 is a burst.
    jcode_base::provider::clear_all_account_cooldowns();

    // 429 for the first four calls (initial + rotation + two waits), then the
    // burst window passes and the request succeeds.
    let server = MockAnthropicServer::start(|call_no| {
        if call_no <= 4 {
            (429, mock_429_body())
        } else {
            (200, mock_success_sse_body("recovered"))
        }
    })
    .await;
    set_test_api_url_override(Some(server.url()));

    let (tx, mut rx) = mpsc::channel::<Result<StreamEvent>>(100);
    let model_state = Arc::new(std::sync::RwLock::new(
        "claude-3-5-haiku-20241022".to_string(),
    ));

    // The override is thread-local and the runtime is multi-threaded, so it is
    // set inside the same future that runs the loop.
    jcode_base::provider::with_quota_exhausted_override(Some(false), || {
        futures::executor::block_on(run_stream_with_retries(
            Client::new(),
            "fixture-access-claude-1".to_string(),
            true,
            e2e_test_request(),
            tx,
            Arc::new(RwLock::new(None)),
            "claude-3-5-haiku-20241022".to_string(),
            "test-session".to_string(),
            model_state,
            DirectTransportConfig::from_env(),
        ));
    });

    set_test_api_url_override(None);

    let mut events = Vec::new();
    while let Some(item) = rx.recv().await {
        events.push(item);
    }

    let errors: Vec<String> = events
        .iter()
        .filter_map(|event| event.as_ref().err().map(|e| e.to_string()))
        .collect();
    assert!(
        errors.is_empty(),
        "the turn must survive a burst limit that clears; got errors: {errors:?}"
    );

    let text: String = events
        .iter()
        .filter_map(|event| match event {
            Ok(StreamEvent::TextDelta(delta)) => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        text, "recovered",
        "the retried attempt's output must reach the consumer"
    );
    assert!(
        server.call_count() >= 5,
        "expected the loop to keep trying past the burst, saw {} calls",
        server.call_count()
    );
}

/// A spent quota must fail immediately instead of waiting out a burst window.
///
/// Waiting ~75s is right when requests are merely bouncing, and wrong when the
/// quota is gone: the reset can be hours away, so every second spent waiting is
/// a second the user stares at a spinner before being told to switch. The usage
/// probe is what separates the two, so this drives the same always-429 server
/// under both verdicts and compares the request counts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_spent_quota_fails_fast_instead_of_waiting_out_a_burst() {
    async fn drive_with_verdict(quota_exhausted: bool) -> usize {
        let _guard = jcode_base::storage::lock_test_env();
        let temp = tempfile::TempDir::new().unwrap();
        let _home = EnvVarGuard::set("JCODE_HOME", temp.path());
        seed_two_claude_accounts();

        let server = MockAnthropicServer::start(|_call_no| (429, mock_429_body())).await;
        set_test_api_url_override(Some(server.url()));

        let (tx, mut rx) = mpsc::channel::<Result<StreamEvent>>(100);
        let model_state = Arc::new(std::sync::RwLock::new(
            "claude-3-5-haiku-20241022".to_string(),
        ));

        // The override is thread-local and the runtime is multi-threaded, so it
        // is set inside the same future that runs the loop.
        jcode_base::provider::with_quota_exhausted_override(Some(quota_exhausted), || {
            futures::executor::block_on(run_stream_with_retries(
                Client::new(),
                "fixture-access-claude-1".to_string(),
                true,
                e2e_test_request(),
                tx,
                Arc::new(RwLock::new(None)),
                "claude-3-5-haiku-20241022".to_string(),
                "test-session".to_string(),
                model_state,
                DirectTransportConfig::from_env(),
            ));
        });

        set_test_api_url_override(None);
        while rx.recv().await.is_some() {}
        server.call_count()
    }

    let spent_calls = drive_with_verdict(true).await;
    let burst_calls = drive_with_verdict(false).await;

    // Rotation still gets its one try per account: proving an account is
    // limited costs one request, never two.
    assert_eq!(
        spent_calls,
        (MAX_RETRIES + 1) as usize,
        "a spent quota must stop after rotation, with no rate-limit waits"
    );
    assert!(
        burst_calls > spent_calls,
        "a burst limit must still be waited out ({burst_calls} vs {spent_calls} calls)"
    );
}

/// A spent 5-hour window must make the turn fail *fast*, on the exact numbers
/// from the 2026-08-12 incident.
///
/// This drives the real retry loop with the real usage probe (no
/// `with_quota_exhausted_override`), against a server that answers 429 for every
/// account, and seeds the per-account usage cache with what the user actually
/// had: `claude-2` at 5h=100% / 7d=49% and `claude-1` at 5h=100% / 7d=24%. Both
/// accounts are therefore quota-blocked for hours, so the correct behaviour is
/// to stop after proving it once per account, with no waits.
///
/// Measured as a *request count* rather than as elapsed time, so it stays honest
/// under the `clamp_retry_delay` test clamp: waiting costs extra requests, and
/// `MAX_RATE_LIMIT_RETRIES` waits would push it well past the rotation budget.
///
/// Negative control: restoring `exhausted` to `five_hour && seven_day` makes
/// every account look like it has headroom, the quota fast-fail stops firing,
/// and the count jumps from 3 to 7. Verified by hand on 2026-08-12; that is the
/// four-minute hang the user reported, in test form.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_spent_five_hour_window_fails_fast_instead_of_waiting_minutes() {
    let _guard = jcode_base::storage::lock_test_env();
    let _cooldown_guard = jcode_base::provider::account_failover::lock_account_cooldown_tests();
    let temp = tempfile::TempDir::new().unwrap();
    let _home = EnvVarGuard::set("JCODE_HOME", temp.path());
    seed_two_claude_accounts();
    jcode_base::provider::clear_all_account_cooldowns();
    jcode_base::usage::clear_anthropic_account_usage_for_tests();

    // Seeded as usage snapshots, deliberately *not* via the 429 path: that path
    // also sets a rotation cooldown, and a cooldown alone is enough to steer
    // rotation, which would hide whether the `exhausted` predicate is right.
    let reset = (chrono::Utc::now() + chrono::Duration::hours(3)).to_rfc3339();
    jcode_base::usage::seed_anthropic_account_usage_for_tests(
        "claude-2",
        1.0,
        0.49,
        Some(reset.clone()),
    );
    jcode_base::usage::seed_anthropic_account_usage_for_tests("claude-1", 1.0, 0.24, Some(reset));

    let server = MockAnthropicServer::start(|_call_no| (429, mock_429_body())).await;
    set_test_api_url_override(Some(server.url()));

    let (tx, mut rx) = mpsc::channel::<Result<StreamEvent>>(100);
    let model_state = Arc::new(std::sync::RwLock::new(
        "claude-3-5-haiku-20241022".to_string(),
    ));

    run_stream_with_retries(
        Client::new(),
        "fixture-access-claude-1".to_string(),
        true,
        e2e_test_request(),
        tx,
        Arc::new(RwLock::new(None)),
        "claude-3-5-haiku-20241022".to_string(),
        "test-session".to_string(),
        model_state,
        DirectTransportConfig::from_env(),
    )
    .await;

    set_test_api_url_override(None);

    let mut details = Vec::new();
    while let Some(event) = rx.recv().await {
        if let Ok(StreamEvent::StatusDetail { detail }) = event {
            details.push(detail);
        }
    }

    assert_eq!(
        server.call_count(),
        (MAX_RETRIES + 1) as usize,
        "a quota wall must cost one request per account and no waits; details: {details:?}"
    );
    assert!(
        !details
            .iter()
            .any(|detail| detail.contains("Every account rate limited; waiting")),
        "a quota wall must not be waited out as if it were a burst: {details:?}"
    );

    // And the user must be told when the quota returns, instead of being left
    // with a bare 429 and an offer to downgrade the model.
    assert!(
        details
            .iter()
            .any(|detail| detail.contains("out of quota") && detail.contains("earliest reset")),
        "the failure must name the earliest reset time: {details:?}"
    );

    // The usage cache is process-global, so a seeded verdict must not outlive
    // this test and steer an unrelated one.
    jcode_base::usage::clear_anthropic_account_usage_for_tests();
    jcode_base::provider::clear_all_account_cooldowns();
}

/// A rotation must refresh the new account's usage snapshot, in the background.
///
/// Guards the switch that keeps this refresh from leaking across tests: a
/// `cfg(test)` flag that silently disabled a real feature would be worse than
/// the flakiness it fixes, so one test turns it on and observes it firing.
///
/// Observed through the cache rather than through the network: the refresh
/// *replaces* whatever entry the rotated-to account had, so a planted sentinel
/// disappearing is proof the refresh ran. The probe cannot succeed here (the
/// fixture tokens are not real credentials and the usage endpoint is not mocked),
/// which is the point: even a failing refresh must invalidate the stale verdict
/// rather than leave it in place.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rotation_refreshes_the_new_accounts_usage_in_the_background() {
    let _guard = jcode_base::storage::lock_test_env();
    let _cooldown_guard = jcode_base::provider::account_failover::lock_account_cooldown_tests();
    let temp = tempfile::TempDir::new().unwrap();
    let _home = EnvVarGuard::set("JCODE_HOME", temp.path());
    seed_two_claude_accounts();
    jcode_base::usage::clear_anthropic_account_usage_for_tests();
    jcode_base::provider::clear_all_account_cooldowns();

    // A stale "has headroom" verdict for the account the rotation will land on.
    jcode_base::usage::seed_anthropic_account_usage_for_tests("claude-2", 0.10, 0.10, None);
    assert!(
        jcode_base::usage::cached_anthropic_account_usage_any_age("claude-2").is_some(),
        "precondition: claude-2 has a cached verdict"
    );

    set_post_rotation_usage_refresh_for_tests(true);
    let credentials: Arc<RwLock<Option<CachedCredentials>>> = Arc::new(RwLock::new(None));
    let mut tried = vec!["claude-1".to_string()];
    let switched = switch_to_next_claude_account(&credentials, &mut tried).await;
    assert_eq!(
        switched.as_ref().map(|switch| switch.label.as_str()),
        Some("claude-2"),
        "precondition: the rotation must land on claude-2"
    );

    // The refresh is detached, so poll rather than assume it has been scheduled.
    //
    // The check is "the seeded verdict is gone", not "the entry is gone": a failed
    // probe caches an error entry of its own, so the entry can exist again while
    // still no longer asserting the stale 10% headroom. An error entry reports
    // `exhausted == false`, i.e. "unknown", which is the correct reading of a
    // probe that could not be reached.
    let mut refreshed = false;
    for _ in 0..100 {
        match jcode_base::usage::cached_anthropic_account_usage_any_age("claude-2") {
            None => {
                refreshed = true;
                break;
            }
            Some(usage) if usage.last_error.is_some() => {
                refreshed = true;
                break;
            }
            Some(_) => {}
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    set_post_rotation_usage_refresh_for_tests(false);

    assert!(
        refreshed,
        "a rotation must not leave the new account's stale usage verdict in place"
    );

    jcode_base::usage::clear_anthropic_account_usage_for_tests();
    jcode_base::provider::clear_all_account_cooldowns();
}

/// The cross-turn property the user actually reported: after a rotation away
/// from a quota-blocked account, the *next* turn must not go back to it.
///
/// Every other test here drives a single turn, and a single turn cannot show
/// this bug. With two accounts, `tried_accounts` already excludes the spent one
/// within a turn, so even the old code rotates away exactly once and looks fine.
/// The failure is *between* turns: `tried_accounts` is per-request, the cooldown
/// expired after 60s, and the cached verdict still claimed headroom, so the next
/// turn started on the blocked account again. That is the
/// `claude-2 -> claude-1 -> claude-2` flapping in the report, and it is why a
/// single live turn is not evidence that this is fixed.
///
/// Both accounts answer 429 here, so neither can "win" by serving; what is
/// measured is purely *which account each request targets*.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_later_turn_does_not_return_to_the_quota_blocked_account() {
    let _guard = jcode_base::storage::lock_test_env();
    let _cooldown_guard = jcode_base::provider::account_failover::lock_account_cooldown_tests();
    let temp = tempfile::TempDir::new().unwrap();
    let _home = EnvVarGuard::set("JCODE_HOME", temp.path());
    seed_two_claude_accounts();
    jcode_base::usage::clear_anthropic_account_usage_for_tests();
    jcode_base::provider::clear_all_account_cooldowns();

    // claude-2 is quota-blocked for hours; claude-1 has headroom. The session
    // starts on the blocked account, as the user's did.
    jcode_base::usage::seed_anthropic_account_usage_for_tests(
        "claude-2",
        1.0,
        0.49,
        Some((chrono::Utc::now() + chrono::Duration::hours(3)).to_rfc3339()),
    );
    jcode_base::usage::seed_anthropic_account_usage_for_tests("claude-1", 0.41, 0.24, None);
    jcode_base::auth::claude::set_active_account("claude-2").unwrap();

    let server = MockAnthropicServer::start(|_call_no| (429, mock_429_body())).await;
    set_test_api_url_override(Some(server.url()));

    // Two turns, each its own request with its own fresh `tried_accounts`.
    // `first_call_of_turn` records where each turn began, because the property
    // under test is which account a turn *starts* on.
    let mut first_call_of_turn = Vec::new();
    for _ in 0..2 {
        first_call_of_turn.push(server.call_count());

        let (tx, mut rx) = mpsc::channel::<Result<StreamEvent>>(100);
        let model_state = Arc::new(std::sync::RwLock::new(
            "claude-3-5-haiku-20241022".to_string(),
        ));
        let token = jcode_base::auth::claude::active_account_label()
            .map(|label| format!("fixture-access-{label}"))
            .unwrap_or_else(|| "fixture-access-claude-2".to_string());

        run_stream_with_retries(
            Client::new(),
            token,
            true,
            e2e_test_request(),
            tx,
            Arc::new(RwLock::new(None)),
            "claude-3-5-haiku-20241022".to_string(),
            "test-session".to_string(),
            model_state,
        )
        .await;

        while rx.recv().await.is_some() {}
    }

    set_test_api_url_override(None);

    // Turn 1 starts on the blocked account by construction. Turn 2 must not:
    // that is the whole complaint, and it is what the old code got wrong, since
    // `tried_accounts` is per-request, the 60s cooldown had expired, and the
    // cached verdict still claimed headroom.
    let second_turn_start = first_call_of_turn[1];
    assert!(
        !server.authorization_contains(second_turn_start, "fixture-access-claude-2"),
        "the second turn must not start on the quota-blocked account"
    );
    assert!(
        server.authorization_contains(second_turn_start, "fixture-access-claude-1"),
        "the second turn must start on the account with headroom"
    );

    // The session override must also have been left pointing at the healthy
    // account, rather than rolled back onto the quota wall by the dead-end
    // restore. This is the state the *third* turn would inherit.
    assert_eq!(
        jcode_base::auth::claude::active_account_label().as_deref(),
        Some("claude-1"),
        "a dead end must not restore the session onto an account whose quota is spent"
    );

    // Note what is deliberately *not* asserted: that the blocked account is
    // never contacted again at all. Once the healthy account also 429s, the
    // blocked one is the only candidate left, and ADR-0015 keeps a limited
    // account as a last resort rather than turning a recoverable state into a
    // hard failure. Trying it once per turn is correct; *starting* there is not.

    jcode_base::usage::clear_anthropic_account_usage_for_tests();
    jcode_base::provider::clear_all_account_cooldowns();
}

/// The cross-session policy from ADR-0018, as a decision table.
///
/// Previously untested and resting on code reading alone, on the grounds that
/// it "needs two concurrent daemons". That was the wrong frame: concurrency
/// reaches this decision only as *state another session left behind*, namely
/// which account is active and which ones this request has already tried. Those
/// are the two inputs, so the policy is testable directly.
#[test]
fn a_rotation_claims_the_shared_override_only_when_better_informed() {
    let tried = |labels: &[&str]| -> Vec<String> { labels.iter().map(|l| l.to_string()).collect() };

    // Single-session case, and the overwhelmingly common one: the request is on
    // the account it started on, has proven that account refuses it, and so
    // knows more than the shared pointer does.
    assert!(
        should_claim_shared_override(Some("claude-1"), &tried(&["claude-1"])),
        "a request that proved the active account refuses it must move the pointer"
    );

    // The flapping case. Another session has settled on claude-1 while this
    // request was working through claude-2. This request has NOT tried claude-1,
    // so it knows nothing about it and must not drag that session off it.
    assert!(
        !should_claim_shared_override(Some("claude-1"), &tried(&["claude-2"])),
        "a request must not move a session onto an account it has not tried"
    );

    // Once this request has also proven claude-1 refuses it, it is again the
    // better-informed party and may claim the pointer.
    assert!(
        should_claim_shared_override(Some("claude-1"), &tried(&["claude-2", "claude-1"])),
        "having proven every account refuses it, the request may claim the pointer"
    );

    // No override set means the primary account with nothing learned about it;
    // claiming preserves the pre-ADR-0018 behaviour rather than silently
    // changing it.
    assert!(
        should_claim_shared_override(None, &tried(&["claude-1"])),
        "an unset override must still be claimable"
    );
}

/// The same policy exercised through the real rotation, against the real
/// process-global override, rather than through the pure helper.
///
/// This is the part a decision table cannot prove: that `switch_to_next_claude_account`
/// actually consults the policy and actually leaves the shared override alone
/// when told to. Simulating the other session is a one-line write to the same
/// global the other session would have written, which is precisely what it
/// leaves behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rotation_leaves_another_sessions_account_alone() {
    let _guard = jcode_base::storage::lock_test_env();
    let _cooldown_guard = jcode_base::provider::account_failover::lock_account_cooldown_tests();
    let temp = tempfile::TempDir::new().unwrap();
    let _home = EnvVarGuard::set("JCODE_HOME", temp.path());
    seed_two_claude_accounts();
    // A third account, because with only two this path is unreachable: the
    // candidate list excludes whatever account is currently active, so when
    // another session holds claude-1 and this request has already tried
    // claude-2, nothing is left to rotate to and the question never arises.
    // Documented in ADR-0018 as a real limitation of the contained fix.
    jcode_base::auth::claude::upsert_account(jcode_base::auth::claude::AnthropicAccount {
        label: "claude-3".to_string(),
        access: "fixture-access-claude-3".to_string(),
        refresh: String::new(),
        expires: chrono::Utc::now().timestamp_millis() + 3_600_000,
        email: None,
        subscription_type: Some("max".to_string()),
        scopes: vec![],
    })
    .unwrap();
    jcode_base::usage::clear_anthropic_account_usage_for_tests();
    jcode_base::provider::clear_all_account_cooldowns();

    // Another session on this daemon has settled on claude-1.
    jcode_base::auth::claude::set_active_account_override(Some("claude-1".to_string()));

    // Our request has been working through claude-2 and has not tried claude-1.
    let credentials: Arc<RwLock<Option<CachedCredentials>>> = Arc::new(RwLock::new(None));
    let mut tried = vec!["claude-2".to_string()];

    let switched = switch_to_next_claude_account(&credentials, &mut tried).await;
    let switched = switched.expect("a candidate must be available");

    assert!(
        !switched.claimed_shared_override,
        "the rotation must report that it did not claim the shared override"
    );
    assert_eq!(
        jcode_base::auth::claude::active_account_label().as_deref(),
        Some("claude-1"),
        "the other session's account must be exactly as it left it"
    );

    jcode_base::provider::clear_all_account_cooldowns();
}
