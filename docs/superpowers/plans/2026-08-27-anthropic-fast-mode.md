# Anthropic Fast Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `/fast on` in a Claude session actually request Anthropic fast mode by sending `speed: "fast"` plus the `fast-mode-2026-02-01` beta header on Claude Opus 5 and Claude Opus 4.8, instead of the unrelated `service_tier: "auto"` Priority Tier parameter it sends today.

**Architecture:** jcode currently implements Anthropic "fast mode" as `service_tier: "auto"`. That is a different Anthropic feature (Priority Tier: a capacity commitment affecting availability, not speed), and it is explicitly *not supported* on Opus 5. Real fast mode is a separate request field, `speed: "fast"`, gated behind a beta header. We keep the existing provider-trait surface (`service_tier()` / `set_service_tier()` returning `"priority"`/`None`) so `/fast`, the info widget, the account center and the client/server protocol keep working unchanged, and change only what the Anthropic runtime *emits* for that state: `speed` + beta header, never `service_tier`. The model gate widens from Opus 4.8 only to Opus 5 and Opus 4.8.

**Tech Stack:** Rust, serde/serde_json, reqwest, tokio, wiremock-style local mock server already used in `anthropic_tests.rs`.

**Spec:** Anthropic's own docs are the spec for this change:
- https://platform.claude.com/docs/en/build-with-claude/fast-mode
- https://platform.claude.com/docs/en/api/service-tiers
- https://code.claude.com/docs/en/fast-mode

## Global Constraints

Copied verbatim from the Anthropic docs; every task's requirements implicitly include these.

- Fast mode is opted into with `speed: "fast"` **and** the beta header value `fast-mode-2026-02-01`. Both are required.
- Supported models are **exactly** `claude-opus-5` and `claude-opus-4-8`.
- Claude Opus 4.7 **returns an error** for `speed: "fast"`. Claude Opus 4.6 **silently runs at standard speed** and bills standard rates. Neither may be sent fast mode.
- `service_tier` is a **different feature** (Priority Tier). Priority Tier "is supported on all available Claude models except Claude Mythos 5, Claude Mythos Preview, Claude Opus 5, and Claude Sonnet 5", and "Fast mode is not available with a Priority Tier commitment." jcode must not send both.
- Fast mode pricing is **$10 / MTok input, $50 / MTok output** for both Opus 5 and Opus 4.8, flat across the full context window including over 200k input tokens. Prompt-caching multipliers stack on top (cache read = 0.1x input).
- The response `usage` object carries a **`speed`** field whose value is `"fast"` or `"standard"`. This is the only ground truth for whether fast mode was actually granted.
- Fast mode has a dedicated rate limit; exceeding it returns **429** (or **529** for capacity). The documented client behavior is to fall back to standard speed by retrying without `speed: "fast"`.
- Switching between fast and standard speed **invalidates the prompt cache**; the two do not share cached prefixes.

## File Structure

| File | Responsibility in this change |
| --- | --- |
| `crates/jcode-provider-core/src/anthropic.rs` | Owns the single source of truth for Anthropic model capabilities and beta headers. Gains `anthropic_supports_fast_mode(model)` and `ANTHROPIC_FAST_MODE_BETA`. |
| `crates/jcode-provider-anthropic/src/lib.rs` | Owns the wire types. `ApiRequest` gains a `speed: Option<String>` field; the `service_tier` field stays for Priority Tier but is no longer set by fast mode. |
| `crates/jcode-provider-anthropic-runtime/src/lib.rs` | Owns request building, the model gate, and the HTTP headers. Fast-mode state maps to `speed`, and the beta header gains `fast-mode-2026-02-01` when fast is active. |
| `crates/jcode-provider-anthropic-runtime/src/sse_types.rs` | Owns SSE deserialization. `UsageInfo` gains `speed` so the granted speed can be observed. |
| `crates/jcode-provider-core/src/pricing.rs` | Owns cost estimation. Fast pricing keys off fast mode rather than a Priority Tier string, and covers Opus 5. |
| `crates/jcode-provider-anthropic-runtime/src/anthropic_tests.rs` | Unit tests for all of the above. |
| `docs/adr/` | ADR recording why fast mode is `speed`, not `service_tier`, so nobody "simplifies" them back together. |

---

### Task 1: Capability gate and beta header constant

Single source of truth for which models support fast mode, next to the existing Anthropic capability tables.

**Files:**
- Modify: `crates/jcode-provider-core/src/anthropic.rs`
- Test: `crates/jcode-provider-core/src/anthropic.rs` (inline `#[cfg(test)] mod tests`, matching the crate's existing style)

**Interfaces:**
- Consumes: `anthropic_strip_1m_suffix` (already in this module).
- Produces:
  - `pub const ANTHROPIC_FAST_MODE_BETA: &str = "fast-mode-2026-02-01";`
  - `pub fn anthropic_supports_fast_mode(model: &str) -> bool`

- [ ] **Step 1: Write the failing test**

Append to the tests module in `crates/jcode-provider-core/src/anthropic.rs`:

```rust
#[test]
fn fast_mode_is_exactly_opus_5_and_opus_4_8() {
    // Supported per https://platform.claude.com/docs/en/build-with-claude/fast-mode
    assert!(anthropic_supports_fast_mode("claude-opus-5"));
    assert!(anthropic_supports_fast_mode("claude-opus-4-8"));
    // The `[1m]` alias must not change the answer.
    assert!(anthropic_supports_fast_mode("claude-opus-5[1m]"));
    assert!(anthropic_supports_fast_mode("claude-opus-4-8[1m]"));

    // Opus 4.7 errors on speed:"fast"; Opus 4.6 silently downgrades and bills
    // standard. Both must be treated as unsupported.
    assert!(!anthropic_supports_fast_mode("claude-opus-4-7"));
    assert!(!anthropic_supports_fast_mode("claude-opus-4-6"));
    assert!(!anthropic_supports_fast_mode("claude-opus-4-5"));
    assert!(!anthropic_supports_fast_mode("claude-sonnet-5"));
    assert!(!anthropic_supports_fast_mode("claude-fable-5"));
    assert!(!anthropic_supports_fast_mode("claude-haiku-4-5"));
    assert!(!anthropic_supports_fast_mode(""));
}

#[test]
fn fast_mode_beta_header_matches_documented_value() {
    assert_eq!(ANTHROPIC_FAST_MODE_BETA, "fast-mode-2026-02-01");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p jcode-provider-core fast_mode`
Expected: FAIL to compile, `cannot find function anthropic_supports_fast_mode`.

- [ ] **Step 3: Write minimal implementation**

Add near `ANTHROPIC_OAUTH_BETA_HEADERS` at the top of `crates/jcode-provider-core/src/anthropic.rs`:

```rust
/// Beta header value required to opt into Anthropic fast mode.
/// See https://platform.claude.com/docs/en/build-with-claude/fast-mode
pub const ANTHROPIC_FAST_MODE_BETA: &str = "fast-mode-2026-02-01";
```

Add next to `anthropic_oauth_beta_headers`:

```rust
/// True when `model` supports Anthropic fast mode (`speed: "fast"`).
///
/// Fast mode is NOT the `service_tier` Priority Tier parameter; the two are
/// separate features and are mutually exclusive. Only Opus 5 and Opus 4.8
/// support fast mode. Opus 4.7 returns an error for `speed: "fast"`, and
/// Opus 4.6 silently runs at standard speed while billing standard rates, so
/// sending it there would charge the user for a request that is never fast.
pub fn anthropic_supports_fast_mode(model: &str) -> bool {
    matches!(
        anthropic_strip_1m_suffix(model).trim().to_ascii_lowercase().as_str(),
        "claude-opus-5" | "claude-opus-4-8"
    )
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p jcode-provider-core fast_mode`
Expected: PASS, 2 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-provider-core/src/anthropic.rs
git commit -m "feat(anthropic): add fast-mode capability gate and beta header constant"
```

---

### Task 2: `speed` field on the wire request

**Files:**
- Modify: `crates/jcode-provider-anthropic/src/lib.rs` (the `ApiRequest` struct, around line 575-595)
- Test: `crates/jcode-provider-anthropic-runtime/src/anthropic_tests.rs`

**Interfaces:**
- Produces: `ApiRequest.speed: Option<String>`, serialized as `speed` and omitted when `None`. Every existing `ApiRequest { .. }` literal must add `speed:`, including the test helper at `anthropic_tests.rs:2245-2262` and the two request builders in the runtime (`lib.rs` ~1413 and ~1792), which pass `None` for now.

- [ ] **Step 1: Write the failing test**

Add to `crates/jcode-provider-anthropic-runtime/src/anthropic_tests.rs`:

```rust
#[test]
fn test_api_request_speed_field_serializes_and_omits() {
    let mut request = base_test_request();
    assert!(
        serde_json::to_value(&request).unwrap().get("speed").is_none(),
        "speed must be omitted when unset"
    );

    request.speed = Some("fast".to_string());
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(value["speed"], "fast");
    assert!(
        value.get("service_tier").is_none(),
        "fast mode must not also send the Priority Tier parameter"
    );
}
```

Note: `base_test_request()` is the existing helper at `anthropic_tests.rs:2245`. If it is private/named differently in your checkout, construct the `ApiRequest` literal inline with the same fields it uses.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p jcode-provider-anthropic-runtime speed_field`
Expected: FAIL to compile, `no field speed on type ApiRequest`.

- [ ] **Step 3: Write minimal implementation**

In `crates/jcode-provider-anthropic/src/lib.rs`, inside `ApiRequest`, immediately after the `service_tier` field:

```rust
    /// Anthropic fast mode opt-in (`"fast"`). Requires the
    /// `fast-mode-2026-02-01` beta header. Distinct from `service_tier`,
    /// which selects Priority Tier; the two are mutually exclusive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<String>,
```

Then add `speed: None,` to every `ApiRequest` literal the compiler flags. Expect three: the two runtime builders and the test helper.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p jcode-provider-anthropic-runtime speed_field`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-provider-anthropic/src/lib.rs crates/jcode-provider-anthropic-runtime
git commit -m "feat(anthropic): add speed field to Messages API request"
```

---

### Task 3: Map fast-mode state onto `speed`, not `service_tier`

The provider trait keeps its `service_tier()` / `set_service_tier()` names so the TUI, info widget and client/server protocol are untouched. Only what Anthropic *emits* changes.

**Files:**
- Modify: `crates/jcode-provider-anthropic-runtime/src/lib.rs`
  - `model_supports_priority_service_tier` (line ~982) -> replaced by the shared gate
  - `current_service_tier_for_model` (line ~1001) -> becomes `fast_speed_for_model`
  - request builders at ~1413 and ~1792
  - `set_service_tier` (~1621), `available_service_tiers` (~1635), `service_tier` (~1611)
  - provider clone at ~1715
- Test: `crates/jcode-provider-anthropic-runtime/src/anthropic_tests.rs`

**Interfaces:**
- Consumes: `jcode_provider_core::anthropic_supports_fast_mode` (Task 1), `ApiRequest.speed` (Task 2).
- Produces: `fn fast_speed_for_model(&self, model: &str) -> Option<String>` returning `Some("fast")` only when fast mode is on and the model supports it.

- [ ] **Step 1: Write the failing test**

Replace the two existing fast-mode tests in `anthropic_tests.rs` (`test_anthropic_opus_48_fast_mode_service_tier_serializes_priority` and `test_anthropic_fast_mode_is_limited_to_opus_48`) with:

```rust
#[test]
fn test_fast_mode_sends_speed_fast_on_opus_5() {
    let provider = AnthropicProvider::new();
    *provider
        .model
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = "claude-opus-5".to_string();

    assert_eq!(provider.available_service_tiers(), vec!["off", "priority"]);
    assert_eq!(provider.service_tier(), None);

    provider.set_service_tier("priority").unwrap();
    assert_eq!(provider.service_tier().as_deref(), Some("priority"));
    assert_eq!(
        provider.fast_speed_for_model("claude-opus-5").as_deref(),
        Some("fast")
    );

    provider.set_service_tier("off").unwrap();
    assert_eq!(provider.fast_speed_for_model("claude-opus-5"), None);
}

#[test]
fn test_fast_mode_sends_speed_fast_on_opus_48() {
    let provider = AnthropicProvider::new();
    provider.set_model("claude-opus-4-8").unwrap();
    provider.set_service_tier("priority").unwrap();
    assert_eq!(
        provider.fast_speed_for_model("claude-opus-4-8").as_deref(),
        Some("fast")
    );
}

#[test]
fn test_fast_mode_rejected_on_unsupported_models() {
    // Opus 4.7 errors API-side, Opus 4.6 silently bills standard: refuse both.
    for model in ["claude-opus-4-7", "claude-opus-4-6", "claude-sonnet-5"] {
        let provider = AnthropicProvider::new();
        *provider
            .model
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = model.to_string();
        assert!(
            provider.available_service_tiers().is_empty(),
            "{model} must not offer fast mode"
        );
        assert!(
            provider.set_service_tier("priority").is_err(),
            "{model} must refuse fast mode"
        );
        assert_eq!(provider.fast_speed_for_model(model), None);
    }
}

#[test]
fn test_fast_mode_never_sends_service_tier() {
    // Priority Tier is a different feature and is unsupported on Opus 5;
    // requesting fast mode must never set service_tier on the wire.
    let provider = AnthropicProvider::new();
    *provider
        .model
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = "claude-opus-5".to_string();
    provider.set_service_tier("priority").unwrap();

    let mut request = base_test_request();
    request.model = "claude-opus-5".to_string();
    request.speed = provider.fast_speed_for_model("claude-opus-5");

    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(value["speed"], "fast");
    assert!(value.get("service_tier").is_none());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p jcode-provider-anthropic-runtime fast_mode`
Expected: FAIL to compile, `no method named fast_speed_for_model`.

- [ ] **Step 3: Write minimal implementation**

In `crates/jcode-provider-anthropic-runtime/src/lib.rs`, replace `model_supports_priority_service_tier` and `current_service_tier_for_model` with:

```rust
    fn model_supports_fast_mode(model: &str) -> bool {
        jcode_provider_core::anthropic_supports_fast_mode(model)
    }

    /// `Some("fast")` when fast mode is enabled and `model` supports it.
    ///
    /// Anthropic fast mode is the `speed` request field, NOT `service_tier`.
    /// `service_tier` selects Priority Tier, which is unsupported on Opus 5
    /// and mutually exclusive with fast mode, so this never yields a tier.
    pub(crate) fn fast_speed_for_model(&self, model: &str) -> Option<String> {
        let enabled = self
            .service_tier
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
            .is_some();
        (enabled && Self::model_supports_fast_mode(model)).then(|| "fast".to_string())
    }
```

Update `normalize_service_tier` so the stored value is a plain fast flag:

```rust
    fn normalize_service_tier(raw: &str) -> Result<Option<String>> {
        let value = raw.trim().to_ascii_lowercase();
        match value.as_str() {
            "" | "default" | "off" | "standard" | "standard_only" => Ok(None),
            // `/fast on` is shared with OpenAI, which spells this `priority`;
            // Anthropic's own spelling for fast mode is `speed: "fast"`.
            "priority" | "auto" | "fast" => Ok(Some("fast".to_string())),
            other => anyhow::bail!(
                "Unsupported Anthropic speed '{}'; expected fast/priority or off/standard",
                other
            ),
        }
    }
```

Update the three accessors to use the new gate:

```rust
    fn service_tier(&self) -> Option<String> {
        self.fast_speed_for_model(&self.model())
            .map(|_| "priority".to_string())
    }

    fn set_service_tier(&self, service_tier: &str) -> Result<()> {
        let normalized = Self::normalize_service_tier(service_tier)?;
        if normalized.is_some() && !Self::model_supports_fast_mode(&self.model()) {
            anyhow::bail!(
                "Anthropic fast mode is only supported on Claude Opus 5 and Claude Opus 4.8"
            );
        }
        *self
            .service_tier
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = normalized;
        Ok(())
    }

    fn available_service_tiers(&self) -> Vec<&'static str> {
        if Self::model_supports_fast_mode(&self.model()) {
            vec!["off", "priority"]
        } else {
            vec![]
        }
    }
```

In both request builders (~1413 and ~1792), replace the `service_tier` line with:

```rust
            service_tier: None,
            speed: self.fast_speed_for_model(&model),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p jcode-provider-anthropic-runtime fast_mode`
Expected: PASS, 4 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-provider-anthropic-runtime/src
git commit -m "fix(anthropic): send speed:fast for fast mode instead of Priority Tier service_tier"
```

---

### Task 4: Send the `fast-mode-2026-02-01` beta header

Without this header the API rejects `speed`, so Task 3 is not usable until this lands.

**Files:**
- Modify: `crates/jcode-provider-anthropic-runtime/src/lib.rs` (`anthropic_beta_header_with_thinking` at ~3331, and both header sites at ~2966 and ~2977)
- Test: `crates/jcode-provider-anthropic-runtime/src/anthropic_tests.rs`

**Interfaces:**
- Consumes: `ANTHROPIC_FAST_MODE_BETA` (Task 1), `ApiRequest.speed` (Task 2).
- Produces: `fn anthropic_beta_header_with_fast(base: &str, fast: bool) -> String`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn test_fast_mode_beta_header_is_appended_once() {
    let base = "prompt-caching-2024-07-31";
    assert_eq!(
        anthropic_beta_header_with_fast(base, false),
        "prompt-caching-2024-07-31"
    );
    assert_eq!(
        anthropic_beta_header_with_fast(base, true),
        "prompt-caching-2024-07-31,fast-mode-2026-02-01"
    );
    // Idempotent: never duplicate the beta token.
    let already = "prompt-caching-2024-07-31,fast-mode-2026-02-01";
    assert_eq!(anthropic_beta_header_with_fast(already, true), already);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p jcode-provider-anthropic-runtime beta_header_is_appended`
Expected: FAIL to compile, `cannot find function anthropic_beta_header_with_fast`.

- [ ] **Step 3: Write minimal implementation**

Next to `anthropic_beta_header_with_thinking` in `crates/jcode-provider-anthropic-runtime/src/lib.rs`:

```rust
/// Append the fast-mode beta token when the request opts into `speed: "fast"`.
/// Anthropic rejects `speed` without this header.
fn anthropic_beta_header_with_fast(base: &str, fast: bool) -> String {
    if fast && !base.contains(jcode_provider_core::ANTHROPIC_FAST_MODE_BETA) {
        format!(
            "{base},{}",
            jcode_provider_core::ANTHROPIC_FAST_MODE_BETA
        )
    } else {
        base.to_string()
    }
}
```

At the OAuth header site (~2966), wrap the existing value:

```rust
        let beta_header = anthropic_beta_header_with_thinking(
            oauth_beta_headers(model_name),
            request.thinking.is_some(),
        );
        let beta_header =
            anthropic_beta_header_with_fast(&beta_header, request.speed.is_some());
```

At the API-key header site (~2977), after the existing `anthropic_beta_header_with_thinking` call:

```rust
        let beta_header =
            anthropic_beta_header_with_fast(&beta_header, request.speed.is_some());
```

If `ANTHROPIC_FAST_MODE_BETA` is not re-exported at the crate root of `jcode-provider-core`, import it from `jcode_provider_core::anthropic::ANTHROPIC_FAST_MODE_BETA` instead.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p jcode-provider-anthropic-runtime beta_header`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-provider-anthropic-runtime/src
git commit -m "feat(anthropic): send fast-mode-2026-02-01 beta header with speed:fast"
```

---

### Task 5: Observe the granted speed from the response

Ground truth for "did fast mode actually happen" is `usage.speed`, not what we asked for. Without this, a silently-standard response is indistinguishable from a fast one.

**Files:**
- Modify: `crates/jcode-provider-anthropic-runtime/src/sse_types.rs` (`UsageInfo`, line ~89)
- Modify: `crates/jcode-provider-anthropic-runtime/src/lib.rs` (usage handling at ~3437-3450)
- Test: `crates/jcode-provider-anthropic-runtime/src/anthropic_tests.rs`

**Interfaces:**
- Produces: `UsageInfo.speed: Option<String>`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn test_usage_info_parses_granted_speed() {
    let usage: crate::sse_types::UsageInfo = serde_json::from_str(
        r#"{"input_tokens":8,"output_tokens":12,"speed":"fast"}"#,
    )
    .unwrap();
    assert_eq!(usage.speed.as_deref(), Some("fast"));

    let standard: crate::sse_types::UsageInfo =
        serde_json::from_str(r#"{"input_tokens":8,"speed":"standard"}"#).unwrap();
    assert_eq!(standard.speed.as_deref(), Some("standard"));

    let absent: crate::sse_types::UsageInfo =
        serde_json::from_str(r#"{"input_tokens":8}"#).unwrap();
    assert_eq!(absent.speed, None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p jcode-provider-anthropic-runtime granted_speed`
Expected: FAIL to compile, `no field speed on type UsageInfo`.

- [ ] **Step 3: Write minimal implementation**

In `sse_types.rs`, add to `UsageInfo`:

```rust
    pub(crate) speed: Option<String>,
```

In `lib.rs`, beside the existing `service_tier` logging in the `message_start` handler:

```rust
                    if let Some(speed) = usage.speed.as_deref() {
                        jcode_base::logging::info(&format!("Anthropic granted speed={speed}"));
                        if std::env::var("JCODE_LOG_SERVICE_TIER").is_ok() {
                            eprintln!("[anthropic] granted speed={speed}");
                        }
                    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p jcode-provider-anthropic-runtime granted_speed`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-provider-anthropic-runtime/src
git commit -m "feat(anthropic): parse and log the granted usage.speed"
```

---

### Task 6: Price fast mode correctly, including Opus 5

**Files:**
- Modify: `crates/jcode-provider-core/src/pricing.rs` (`anthropic_tier_is_fast` ~17, `anthropic_api_pricing_with_tier` ~42-84)
- Test: `crates/jcode-provider-core/src/pricing.rs` (existing inline tests module)

**Interfaces:**
- Consumes: `anthropic_supports_fast_mode` (Task 1).
- Produces: unchanged public signature `anthropic_api_pricing_with_tier(model, service_tier)`; callers in `crates/jcode-base/src/provider/pricing.rs:205` need no change.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn anthropic_fast_mode_pricing_covers_opus_5() {
    // $10/$50 per MTok for both supported models, cache read 0.1x input.
    for model in ["claude-opus-5", "claude-opus-4-8"] {
        let fast = anthropic_api_pricing_with_tier(model, Some("priority"))
            .expect("priced model");
        assert_eq!(fast.input_micros_per_mtok(), usd_to_micros(10.0), "{model}");
        assert_eq!(fast.output_micros_per_mtok(), usd_to_micros(50.0), "{model}");

        let standard = anthropic_api_pricing_with_tier(model, None).expect("priced model");
        assert_eq!(standard.input_micros_per_mtok(), usd_to_micros(5.0), "{model}");
        assert_eq!(standard.output_micros_per_mtok(), usd_to_micros(25.0), "{model}");
    }

    // Opus 4.7 and 4.6 never bill fast rates: 4.7 errors, 4.6 silently runs
    // standard. Both must fall back to standard pricing.
    for model in ["claude-opus-4-7", "claude-opus-4-6"] {
        let priced = anthropic_api_pricing_with_tier(model, Some("priority"))
            .expect("priced model");
        assert_eq!(priced.input_micros_per_mtok(), usd_to_micros(5.0), "{model}");
    }
}
```

Adapt the accessor names (`input_micros_per_mtok()` etc.) to whatever `RouteCheapnessEstimate` actually exposes in this checkout; read the struct before writing the assertions.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p jcode-provider-core fast_mode_pricing`
Expected: FAIL, Opus 5 returns $5/$25 and Opus 4.7 returns $30/$150.

- [ ] **Step 3: Write minimal implementation**

Replace the fast branch of `anthropic_api_pricing_with_tier`:

```rust
    if anthropic_tier_is_fast(service_tier) && crate::anthropic_supports_fast_mode(base) {
        // Fast mode: $10/$50 per MTok, flat across the full context window,
        // for Claude Opus 5 and Claude Opus 4.8 only. Cache read is 0.1x the
        // fast input rate. https://platform.claude.com/docs/en/build-with-claude/fast-mode
        return exact(10.0, 50.0, 1.0, "Anthropic API fast mode pricing");
    }
```

and extend `anthropic_tier_is_fast` to accept `"fast"` alongside `"auto"` and `"priority"`. Delete the now-dead Opus 4.7/4.6 fast branch.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p jcode-provider-core pricing`
Expected: PASS. Fix any existing pricing test that asserted the old Opus 4.7 fast rate; those assertions encoded the bug.

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-provider-core/src/pricing.rs
git commit -m "fix(anthropic): price fast mode for Opus 5, drop bogus Opus 4.7/4.6 fast rates"
```

---

### Task 7: Whole-workspace build, TUI copy, and ADR

**Files:**
- Modify: `crates/jcode-tui/src/tui/app/helpers.rs` (`fast_mode_*` messages) only if a string names Opus 4.8 specifically
- Create: `docs/adr/NNNN-anthropic-fast-mode-is-speed-not-service-tier.md` (+ index row in `docs/adr/README.md`)

- [ ] **Step 1: Build and test the whole workspace**

Run: `cargo build --profile selfdev -p jcode --bin jcode`
Then: `cargo test -p jcode-provider-core -p jcode-provider-anthropic -p jcode-provider-anthropic-runtime`
Expected: clean build, all tests pass.

- [ ] **Step 2: Grep for stale copy and fix it**

Run: `rg -n "Opus 4.8|only supported for" crates/jcode-tui/src crates/jcode-provider-anthropic-runtime/src`
Any user-facing string claiming fast mode is Opus 4.8 only must now say Opus 5 and Opus 4.8.

- [ ] **Step 3: Write the ADR**

Create `docs/adr/NNNN-anthropic-fast-mode-is-speed-not-service-tier.md` (next free number) recording: the decision (fast mode is `speed` + beta header, `service_tier` is Priority Tier and is never sent for fast mode); what forced it (jcode conflated them, so `/fast on` was a no-op on Opus 5 and would have been rejected outright had the gate allowed it); consequences (the trait method keeps the `service_tier` name for UI compatibility, which is a deliberate misnomer worth one comment); rejected alternatives (renaming the whole trait surface: larger blast radius across protocol and TUI for no user-visible gain); what would make us revisit (fast mode leaving research preview, or Anthropic unifying the two parameters); and the evidence links.

- [ ] **Step 4: Meta-test the gate**

Temporarily widen `anthropic_supports_fast_mode` to match all `claude-opus` models, run the test suites, and confirm Task 1 and Task 3 tests fail. Revert. A gate that never failed proves nothing.

- [ ] **Step 5: Commit**

```bash
git add docs/adr crates
git commit -m "docs(adr): record that Anthropic fast mode is speed, not service_tier"
```

---

### Task 8: End-to-end verification against the live API

Everything above is unit-level. This task is the only one that proves the feature works, and it is required before claiming completion.

**Files:** none (verification only)

- [ ] **Step 1: Reload the built binary onto its own socket**

```bash
cargo build --profile selfdev -p jcode --bin jcode
./target/selfdev/jcode run --no-update --socket /run/user/$(id -u)/jcode-fasttest.sock '/fast on'
```

On macOS use a writable path such as `/tmp/jcode-fasttest.sock`. A build alone proves nothing; the shared daemon still serves the old code until reloaded.

- [ ] **Step 2: Capture the outgoing request**

Run a one-shot prompt on Opus 5 with fast mode on and `JCODE_LOG_SERVICE_TIER=1` set, then confirm from the canonical-input log that the request body contains `"speed":"fast"`, contains **no** `service_tier`, and that the `anthropic-beta` header contains `fast-mode-2026-02-01`.

- [ ] **Step 3: Confirm the API granted it**

Expected in the log: `Anthropic granted speed=fast`.
If it reports `standard`, fast mode was refused. Check for a 429 (fast-mode rate limit or unprovisioned access) and for the account requirements: subscription plans need usage credits enabled, Console orgs need fast mode provisioned.

- [ ] **Step 4: Confirm the negative case**

Switch to `claude-sonnet-5` and run `/fast on`. Expected: a clear error naming Opus 5 and Opus 4.8, and no `speed` field on any request.

- [ ] **Step 5: Report**

State plainly which of steps 2, 3 and 4 were observed first-hand, and if step 3 could not be reached because of account provisioning, say so rather than implying the feature is verified.
