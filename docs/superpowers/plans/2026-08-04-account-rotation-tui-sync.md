# Account Rotation TUI Sync Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When the daemon rotates Anthropic OAuth accounts on a rate limit, the TUI must show the new account's usage window and keep the model's real context limit (1M) instead of silently rendering the old account's reset time and a 200k window.

**Architecture:** The TUI is a client process; the agent runs in a separate daemon process. Rotation (`switch_to_next_claude_account` in `jcode-provider-anthropic-runtime`) sets a process-local account override that the client cannot observe, and the client renders usage from its own `crate::usage::get_sync()` cache. This plan publishes a rotation event on the existing global `Bus` in the daemon, forwards it to connected clients as a new `ServerEvent` over the wire protocol, and has the client mirror the account override, invalidate its usage cache, and recompute the context limit from the model rather than from the local provider default.

**Tech Stack:** Rust workspace (edition 2024), tokio, serde/serde_json line-delimited JSON wire protocol, ratatui TUI, `cargo test` / `cargo clippy` / `cargo fmt`.

## Global Constraints

- Rust workspace at `/Users/joelsbastos/MYNE/Projects/jcode`, branch `mine`. Build with `source "$HOME/.cargo/env"` first.
- Never log, print, or assert on token material (access tokens, refresh tokens, API keys). Assert on account labels and inequality only.
- `cargo check --workspace --all-targets` has 7 PRE-EXISTING errors in `src/cli/commands_tests.rs` and `src/cli/commands/menubar.rs` (`ConfidenceState`, `GateObservation`). `cargo test -p jcode-base` has 6 PRE-EXISTING failures (`spawn_detached_creates_new_session`, three `initial_session_context_*`, `streaming_guard_creates_visible_macos_sleep_assertion`, `reload_global_excludes_project_local_skills`). Do not attempt to fix either set; do not be confused by them.
- Do NOT restart, rebuild, or repoint the shared daemon (`~/.jcode/builds/shared-server/*`, `jcode self-dev --build`, `scripts/install_release.sh`). A live user session depends on it. Use your own `--socket` path for any runtime check.
- Do NOT modify anything under `~/.jcode/`, especially `~/.jcode/auth.json` (real credentials for accounts `claude-1`, `claude-2`). Tests use `jcode_base::storage::lock_test_env()` and a `tempfile::TempDir` `JCODE_HOME`, following the existing pattern in `crates/jcode-provider-anthropic-runtime/src/anthropic_tests.rs`.
- Run `cargo fmt` on every file you touch before committing.
- Commit after each task with the message given in the task's final step.

## Background: why the current behavior is wrong

Two independent defects, both visible in one screenshot (usage widget showing `resets 1h 43m` for the *previous* account while `Context 216k/200k` is red):

1. **Client never learns about rotation.** `crates/jcode-provider-anthropic-runtime/src/lib.rs` calls `auth::claude::set_active_account_override` inside the daemon. `crates/jcode-tui/src/tui/app/tui_state.rs:428` renders the widget from `crate::usage::get_sync()` evaluated in the *client* process, which resolves the account from the client's own state.
2. **Usage cache is account-blind.** `UsageData` (`crates/jcode-base/src/usage/model.rs:50-67`) has no account field and lives in one global TTL cache. `fetch_usage` (`crates/jcode-base/src/usage/accessors.rs:15-34`) reads whichever account is active *at fetch time*. After any switch, `get()`/`get_sync()` keep serving the previous account's numbers until the TTL expires, because `is_stale()` is time-based. This also affects the existing manual `/account switch`.
3. **Context limit clobbered.** Eight call sites assign `app.context_limit = app.provider.context_window() as u64`, which is the *local* provider's default window and drops the 1M variant. The correct helper is `update_context_limit_for_model` (`crates/jcode-tui/src/tui/app/model_context.rs:630`), which consults `crate::provider::context_limit_for_model_with_provider` for remote sessions.

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `crates/jcode-base/src/usage/accessors.rs` | Anthropic/OpenAI usage cache access | Add `invalidate_anthropic_usage()` that forces the next read to refetch |
| `crates/jcode-base/src/usage/model.rs` | `UsageData` shape and staleness | Add `mark_stale()` used by the invalidator |
| `crates/jcode-base/src/bus.rs` | Process-global event bus | Add `BusEvent::AnthropicAccountRotated(AnthropicAccountRotated)` + struct + `publish_anthropic_account_rotated` |
| `crates/jcode-provider-anthropic-runtime/src/lib.rs` | Streaming retry/rotation loop | Publish the bus event on a successful rotation and on restore |
| `crates/jcode-protocol/src/wire.rs` | Client/server wire events | Add `ServerEvent::AnthropicAccountRotated { label }` |
| `crates/jcode-app-core/src/server/client_lifecycle.rs` | Per-connection bus→client forwarding | Forward the bus event as the new `ServerEvent`, invalidate daemon-side usage |
| `crates/jcode-tui/src/tui/app/remote/server_events.rs` | Client reaction to server events | Mirror override, invalidate caches, message, recompute context limit |
| `crates/jcode-tui/src/tui/app/auth_account_commands.rs`, `auth.rs`, `remote.rs` | Manual account switch paths | Replace `context_window()` clobber with `update_context_limit_for_model` |

---

### Task 1: Force a usage refetch after an account change

**Files:**
- Modify: `crates/jcode-base/src/usage/model.rs` (add `mark_stale` to `impl UsageData`, near `is_stale` at line 71)
- Modify: `crates/jcode-base/src/usage/accessors.rs` (add `invalidate_anthropic_usage`, near `get` at line 69)
- Test: `crates/jcode-base/src/usage/model.rs` (unit test in the file's existing `#[cfg(test)] mod tests`; if the file has none, add one at the end)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `jcode_base::usage::invalidate_anthropic_usage()` (sync, no return value) and `UsageData::mark_stale(&mut self)`.

- [ ] **Step 1: Write the failing test**

Append to `crates/jcode-base/src/usage/model.rs`:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p jcode-base mark_stale_forces_the_next_read_to_refetch`
Expected: FAIL — `no method named 'mark_stale' found for struct 'UsageData'`

- [ ] **Step 3: Write minimal implementation**

In `crates/jcode-base/src/usage/model.rs`, inside `impl UsageData` immediately after `is_stale`:

```rust
    /// Drop the freshness stamp so the next `get`/`get_sync` refetches.
    ///
    /// `UsageData` carries no account identity and the cache is global, so a
    /// snapshot taken before an account switch is not stale by time but is
    /// wrong by account. Callers that change the active account must call this.
    pub fn mark_stale(&mut self) {
        self.fetched_at = None;
    }
```

In `crates/jcode-base/src/usage/accessors.rs`, after `pub async fn get()`:

```rust
/// Invalidate the cached Anthropic usage snapshot after an account change.
///
/// Safe to call from sync contexts: when a Tokio runtime is available the
/// write happens inline, otherwise the cache is left untouched rather than
/// panicking (the next TTL expiry then repairs it).
pub fn invalidate_anthropic_usage() {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    handle.spawn(async {
        let usage = get_usage().await;
        usage.write().await.mark_stale();
    });
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `source "$HOME/.cargo/env" && cargo test -p jcode-base mark_stale_forces_the_next_read_to_refetch`
Expected: PASS

- [ ] **Step 5: Verify the crate still builds and lints**

Run: `source "$HOME/.cargo/env" && cargo clippy -p jcode-base --all-targets 2>&1 | grep -E "^(warning|error).*usage" | head`
Expected: no output (no new warnings referencing the usage module)

- [ ] **Step 6: Commit**

```bash
source "$HOME/.cargo/env" && cargo fmt -p jcode-base
git add crates/jcode-base/src/usage/model.rs crates/jcode-base/src/usage/accessors.rs
git commit -m "feat(usage): let callers invalidate the account-blind usage cache"
```

---

### Task 2: Publish a rotation event on the global bus

**Files:**
- Modify: `crates/jcode-base/src/bus.rs` (add struct near `UiActivity` at line 209; add enum variant to `BusEvent` at line 394; add publish helper near `publish_models_updated` at line 542)
- Test: `crates/jcode-base/src/bus.rs` (append a `#[cfg(test)] mod` at the end of the file)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces:
  - `jcode_base::bus::AnthropicAccountRotated { pub label: String, pub previous_label: Option<String> }` (derives `Clone, Debug`)
  - `BusEvent::AnthropicAccountRotated(AnthropicAccountRotated)`
  - `Bus::publish_anthropic_account_rotated(&self, label: impl Into<String>, previous_label: Option<String>)`

- [ ] **Step 1: Write the failing test**

Append to `crates/jcode-base/src/bus.rs`:

```rust
#[cfg(test)]
mod account_rotation_bus_tests {
    use super::{Bus, BusEvent};

    #[test]
    fn publishing_a_rotation_reaches_subscribers_with_both_labels() {
        let bus = Bus::new();
        let mut rx = bus.subscribe();

        bus.publish_anthropic_account_rotated("claude-2", Some("claude-1".to_string()));

        match rx.try_recv() {
            Ok(BusEvent::AnthropicAccountRotated(rotated)) => {
                assert_eq!(rotated.label, "claude-2");
                assert_eq!(rotated.previous_label.as_deref(), Some("claude-1"));
            }
            other => panic!("expected an AnthropicAccountRotated event, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p jcode-base publishing_a_rotation_reaches_subscribers_with_both_labels`
Expected: FAIL — `no method named 'publish_anthropic_account_rotated'` / `no variant named 'AnthropicAccountRotated'`

- [ ] **Step 3: Write minimal implementation**

In `crates/jcode-base/src/bus.rs`, after the `UiActivity` struct block (around line 214):

```rust
/// The daemon moved this process onto a different Anthropic OAuth account
/// because the previous one was rate limited.
#[derive(Clone, Debug)]
pub struct AnthropicAccountRotated {
    /// Account label now serving requests.
    pub label: String,
    /// Account the request was on before, when known.
    pub previous_label: Option<String>,
}
```

Add to `enum BusEvent` (after the `UiActivity(UiActivity)` variant):

```rust
    /// Anthropic OAuth account rotated mid-request after a rate limit
    AnthropicAccountRotated(AnthropicAccountRotated),
```

Add to `impl Bus`, next to `publish_models_updated`:

```rust
    pub fn publish_anthropic_account_rotated(
        &self,
        label: impl Into<String>,
        previous_label: Option<String>,
    ) {
        self.publish(BusEvent::AnthropicAccountRotated(AnthropicAccountRotated {
            label: label.into(),
            previous_label,
        }));
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `source "$HOME/.cargo/env" && cargo test -p jcode-base publishing_a_rotation_reaches_subscribers_with_both_labels`
Expected: PASS

- [ ] **Step 5: Check no exhaustive match broke**

Run: `source "$HOME/.cargo/env" && cargo check -p jcode-base -p jcode-app-core -p jcode-tui 2>&1 | grep -E "^error" -A 6 | head -20`
Expected: no output. If a `match` on `BusEvent` is non-exhaustive, add an arm that ignores the new variant (`Ok(BusEvent::AnthropicAccountRotated(_)) => {}`) in that file and note it; Task 5 and Task 6 replace the real handlers.

- [ ] **Step 6: Commit**

```bash
source "$HOME/.cargo/env" && cargo fmt -p jcode-base
git add crates/jcode-base/src/bus.rs
git commit -m "feat(bus): add an Anthropic account rotation event"
```

---

### Task 3: Publish the event from the rotation path

**Files:**
- Modify: `crates/jcode-provider-anthropic-runtime/src/lib.rs` (rotation branch inside `run_stream_with_retries`, and `restore_claude_account`)
- Test: `crates/jcode-provider-anthropic-runtime/src/anthropic_tests.rs` (append; reuse the existing mock-server harness used by `run_stream_with_retries_rotates_account_on_streaming_429_and_replays_successfully`)

**Interfaces:**
- Consumes: `jcode_base::bus::Bus::global().publish_anthropic_account_rotated(label, previous_label)` and `BusEvent::AnthropicAccountRotated` from Task 2.
- Produces: no new public API; the runtime now emits the bus event whenever the active account changes.

- [ ] **Step 1: Write the failing test**

Append to `crates/jcode-provider-anthropic-runtime/src/anthropic_tests.rs`. Read the existing rotation test first and copy its fixture setup verbatim (temp `JCODE_HOME`, `lock_test_env()`, two fixture accounts, mock server returning 429 then a success SSE body, `set_test_api_url_override`) — this test differs only in that it subscribes to the bus before driving the stream:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn rotation_publishes_an_account_rotated_bus_event() {
    // Fixture setup identical to
    // `run_stream_with_retries_rotates_account_on_streaming_429_and_replays_successfully`:
    // temp JCODE_HOME with fixture accounts claude-1 and claude-2, mock server
    // answering 429 then 200, TEST_API_URL_OVERRIDE pointed at it.
    let fixture = RotationFixture::new().await;
    let mut bus_rx = jcode_base::bus::Bus::global().subscribe();

    fixture.drive_stream_to_completion().await;

    let mut rotated_labels = Vec::new();
    while let Ok(event) = bus_rx.try_recv() {
        if let jcode_base::bus::BusEvent::AnthropicAccountRotated(rotated) = event {
            rotated_labels.push((rotated.previous_label.clone(), rotated.label.clone()));
        }
    }

    assert_eq!(
        rotated_labels,
        vec![(Some("claude-1".to_string()), "claude-2".to_string())],
        "a rotation the client cannot otherwise observe must be announced exactly once"
    );
}
```

If the existing test does not already expose a reusable `RotationFixture`, extract one from it in this step (same setup, two methods: `new()` and `drive_stream_to_completion()`), and update the existing test to use it so the setup is written once.

- [ ] **Step 2: Run test to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p jcode-provider-anthropic-runtime rotation_publishes_an_account_rotated_bus_event`
Expected: FAIL — assertion shows an empty `rotated_labels` vec

- [ ] **Step 3: Write minimal implementation**

In `crates/jcode-provider-anthropic-runtime/src/lib.rs`, in the rotation branch of `run_stream_with_retries`, immediately after `current_account = Some(switched_label.clone());`:

```rust
                            // The client renders usage from its own process and
                            // cannot see this process's account override, so a
                            // rotation is invisible to it unless announced.
                            jcode_base::bus::Bus::global().publish_anthropic_account_rotated(
                                switched_label.clone(),
                                Some(previous_label.clone()),
                            );
```

In `restore_claude_account`, after the successful `set_active_account_override(original.clone())` call:

```rust
    jcode_base::bus::Bus::global().publish_anthropic_account_rotated(
        original.clone().unwrap_or_else(auth::claude::primary_account_label),
        ours.clone(),
    );
```

Note: `previous_label` is already computed above the rotation log line; do not recompute it.

- [ ] **Step 4: Run test to verify it passes**

Run: `source "$HOME/.cargo/env" && cargo test -p jcode-provider-anthropic-runtime rotation_publishes_an_account_rotated_bus_event`
Expected: PASS

- [ ] **Step 5: Run the whole crate's tests**

Run: `source "$HOME/.cargo/env" && cargo test -p jcode-provider-anthropic-runtime`
Expected: all tests pass (62 before this task, 63 after; 1 ignored)

- [ ] **Step 6: Commit**

```bash
source "$HOME/.cargo/env" && cargo fmt -p jcode-provider-anthropic-runtime
git add crates/jcode-provider-anthropic-runtime/src/lib.rs crates/jcode-provider-anthropic-runtime/src/anthropic_tests.rs
git commit -m "feat(anthropic): announce account rotation on the bus"
```

---

### Task 4: Add the wire event

**Files:**
- Modify: `crates/jcode-protocol/src/wire.rs` (add variant next to `AvailableModelsUpdated` at line 1266)
- Test: `crates/jcode-protocol/src/wire.rs` (append to the file's existing `#[cfg(test)] mod tests`; if absent, add one at the end)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `ServerEvent::AnthropicAccountRotated { label: String, previous_label: Option<String> }`, serialized with `#[serde(rename = "anthropic_account_rotated")]`.

Compatibility note for the implementer: an older client that does not know this variant logs and skips the line (`crates/jcode-tui/src/tui/backend.rs:1204-1232`), counting it against `MAX_STRAY_REMOTE_PROTOCOL_LINES = 32` (`backend.rs:262`) before disconnecting. Rotation events are rare (only on a rate limit), so this is acceptable — but do not add unrelated chatty events to this channel.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn anthropic_account_rotated_round_trips_over_the_wire() {
        let event = ServerEvent::AnthropicAccountRotated {
            label: "claude-2".to_string(),
            previous_label: Some("claude-1".to_string()),
        };

        let encoded = serde_json::to_string(&event).expect("event serializes");
        assert!(
            encoded.contains("\"anthropic_account_rotated\""),
            "the wire name is part of the protocol contract: {encoded}"
        );

        match serde_json::from_str::<ServerEvent>(&encoded).expect("event deserializes") {
            ServerEvent::AnthropicAccountRotated {
                label,
                previous_label,
            } => {
                assert_eq!(label, "claude-2");
                assert_eq!(previous_label.as_deref(), Some("claude-1"));
            }
            other => panic!("round-trip produced the wrong variant: {other:?}"),
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p jcode-protocol anthropic_account_rotated_round_trips_over_the_wire`
Expected: FAIL — `no variant named 'AnthropicAccountRotated' found for enum 'ServerEvent'`

- [ ] **Step 3: Write minimal implementation**

In `crates/jcode-protocol/src/wire.rs`, immediately after the `AvailableModelsUpdated` variant:

```rust
    /// The daemon rotated to a different Anthropic OAuth account mid-request
    /// after a rate limit. Clients render usage from their own process, so this
    /// is the only signal that their cached account view is now wrong.
    #[serde(rename = "anthropic_account_rotated")]
    AnthropicAccountRotated {
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        previous_label: Option<String>,
    },
```

- [ ] **Step 4: Run test to verify it passes**

Run: `source "$HOME/.cargo/env" && cargo test -p jcode-protocol anthropic_account_rotated_round_trips_over_the_wire`
Expected: PASS

- [ ] **Step 5: Find every exhaustive match that now fails**

Run: `source "$HOME/.cargo/env" && cargo check -p jcode-app-core -p jcode-tui 2>&1 | grep -E "^error" -A 8 | head -30`
Expected: non-exhaustive-match errors listing the files handled in Tasks 5 and 6. Add a `ServerEvent::AnthropicAccountRotated { .. } => {}` arm (returning whatever the surrounding match returns, e.g. `false`) in each so the workspace compiles; Tasks 5 and 6 fill them in.

- [ ] **Step 6: Commit**

```bash
source "$HOME/.cargo/env" && cargo fmt -p jcode-protocol -p jcode-app-core -p jcode-tui
git add crates/jcode-protocol/src/wire.rs crates/jcode-app-core crates/jcode-tui
git commit -m "feat(protocol): add the anthropic_account_rotated server event"
```

---

### Task 5: Forward the rotation from daemon to clients

**Files:**
- Modify: `crates/jcode-app-core/src/server/client_lifecycle.rs` (bus forwarding loop, `bus_event = bus_rx.recv()` arm starting at line ~806)
- Test: `crates/jcode-app-core/src/server/client_lifecycle.rs` (append a `#[cfg(test)] mod`, or extend the file's existing test module)

**Interfaces:**
- Consumes: `BusEvent::AnthropicAccountRotated` (Task 2), `ServerEvent::AnthropicAccountRotated` (Task 4), `jcode_base::usage::invalidate_anthropic_usage` (Task 1).
- Produces: a helper `fn account_rotated_server_event(rotated: &jcode_base::bus::AnthropicAccountRotated) -> ServerEvent` so the mapping is testable without a live connection.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod account_rotation_forwarding_tests {
    use super::*;

    #[test]
    fn bus_rotation_maps_to_the_wire_event() {
        let rotated = jcode_base::bus::AnthropicAccountRotated {
            label: "claude-2".to_string(),
            previous_label: Some("claude-1".to_string()),
        };

        match account_rotated_server_event(&rotated) {
            ServerEvent::AnthropicAccountRotated {
                label,
                previous_label,
            } => {
                assert_eq!(label, "claude-2");
                assert_eq!(previous_label.as_deref(), Some("claude-1"));
            }
            other => panic!("expected the rotation wire event, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p jcode-app-core bus_rotation_maps_to_the_wire_event`
Expected: FAIL — `cannot find function 'account_rotated_server_event'`

- [ ] **Step 3: Write minimal implementation**

In `crates/jcode-app-core/src/server/client_lifecycle.rs`, add near the other event-building helpers:

```rust
fn account_rotated_server_event(
    rotated: &jcode_base::bus::AnthropicAccountRotated,
) -> ServerEvent {
    ServerEvent::AnthropicAccountRotated {
        label: rotated.label.clone(),
        previous_label: rotated.previous_label.clone(),
    }
}
```

Replace the placeholder arm added in Task 4 (or add, if the loop had a catch-all) inside the `bus_event = bus_rx.recv(), if client_subscribed` match:

```rust
                    Ok(BusEvent::AnthropicAccountRotated(rotated)) => {
                        // The daemon's own usage cache is account-blind too, so
                        // drop it before the client asks for fresh numbers.
                        crate::usage::invalidate_anthropic_usage();
                        let _ = client_event_tx.send(account_rotated_server_event(&rotated));
                    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `source "$HOME/.cargo/env" && cargo test -p jcode-app-core bus_rotation_maps_to_the_wire_event`
Expected: PASS

- [ ] **Step 5: Confirm the crate builds**

Run: `source "$HOME/.cargo/env" && cargo check -p jcode-app-core --all-targets 2>&1 | grep -E "^error" -A 6 | head -20`
Expected: no output

- [ ] **Step 6: Commit**

```bash
source "$HOME/.cargo/env" && cargo fmt -p jcode-app-core
git add crates/jcode-app-core/src/server/client_lifecycle.rs
git commit -m "feat(server): forward account rotation to connected clients"
```

---

### Task 6: Make the TUI follow the rotation

**Files:**
- Modify: `crates/jcode-tui/src/tui/app/remote/server_events.rs` (replace the placeholder arm from Task 4 inside `handle_server_event`, signature at line 539)
- Test: `crates/jcode-tui/src/tui/app/tests/` — add the test to the same module that already covers `ServerEvent` handling for account commands (`commands_accounts_02/part_01.rs` is the closest existing example; follow whatever `App` construction helper that file uses)

**Interfaces:**
- Consumes: `ServerEvent::AnthropicAccountRotated` (Task 4), `jcode_base::usage::invalidate_anthropic_usage` (Task 1), `App::update_context_limit_for_model` (`crates/jcode-tui/src/tui/app/model_context.rs:630`).
- Produces: no new public API. After handling, `app.context_limit` equals the model's real limit and the account override in the client process matches the daemon's.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn rotation_event_keeps_the_model_context_limit_and_adopts_the_new_account() {
    let mut app = test_app(); // use this test module's existing App builder
    app.update_context_limit_for_model("claude-opus-5[1m]");
    let expected_limit = app.context_limit;
    assert!(
        expected_limit > 200_000,
        "fixture must use a 1M model or this test cannot detect the clobber"
    );

    handle_server_event(
        &mut app,
        ServerEvent::AnthropicAccountRotated {
            label: "claude-2".to_string(),
            previous_label: Some("claude-1".to_string()),
        },
        &mut test_remote_state(), // this test module's RemoteEventState stub
    );

    assert_eq!(
        app.context_limit, expected_limit,
        "a rate-limit rotation must not shrink the window to the local provider default"
    );
    assert_eq!(
        crate::auth::claude::active_account_label().as_deref(),
        Some("claude-2"),
        "the client must mirror the account the daemon actually uses"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p jcode-tui rotation_event_keeps_the_model_context_limit_and_adopts_the_new_account`
Expected: FAIL — the account label is unchanged (placeholder arm does nothing)

- [ ] **Step 3: Write minimal implementation**

Replace the placeholder arm in `handle_server_event`:

```rust
        ServerEvent::AnthropicAccountRotated {
            label,
            previous_label,
        } => {
            // Mirror the daemon's choice with an override rather than a
            // persisted switch: the rotation lasts as long as the daemon says
            // it does, and must not rewrite the user's configured account.
            crate::auth::claude::set_active_account_override(Some(label.clone()));
            crate::auth::AuthStatus::invalidate_cache();
            crate::usage::invalidate_anthropic_usage();

            let active_model = app.provider.model();
            app.update_context_limit_for_model(&active_model);

            let message = match previous_label {
                Some(previous) => format!(
                    "⚡ Rate limited; switched Anthropic account: {} → {}",
                    previous, label
                ),
                None => format!("⚡ Rate limited; switched Anthropic account to {}", label),
            };
            app.push_display_message(DisplayMessage::system(message));
            app.set_status_notice(format!("Account: switched to {}", label));
            false
        }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `source "$HOME/.cargo/env" && cargo test -p jcode-tui rotation_event_keeps_the_model_context_limit_and_adopts_the_new_account`
Expected: PASS

- [ ] **Step 5: Run the crate's test suite for regressions**

Run: `source "$HOME/.cargo/env" && cargo test -p jcode-tui 2>&1 | tail -3`
Expected: same pass/fail counts as before this task, plus one new passing test. If anything else fails, fix it before committing.

- [ ] **Step 6: Commit**

```bash
source "$HOME/.cargo/env" && cargo fmt -p jcode-tui
git add crates/jcode-tui/src/tui/app/remote/server_events.rs crates/jcode-tui/src/tui/app/tests
git commit -m "feat(tui): follow daemon account rotation in the usage widget"
```

---

### Task 7: Stop clobbering the context limit on manual account switches

**Files:**
- Modify: `crates/jcode-tui/src/tui/app/auth_account_commands.rs:470`, `:488`, `:518`, `:536`
- Modify: `crates/jcode-tui/src/tui/app/auth.rs:1039`, `:1111`
- Modify: `crates/jcode-tui/src/tui/app/remote.rs:442`, `:463`
- Test: same test module used in Task 6

**Interfaces:**
- Consumes: `App::update_context_limit_for_model`.
- Produces: no new API. All account-switch paths now agree on how the context limit is derived.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn manual_account_switch_keeps_the_model_context_limit() {
    let mut app = test_app();
    app.update_context_limit_for_model("claude-opus-5[1m]");
    let expected_limit = app.context_limit;
    assert!(
        expected_limit > 200_000,
        "fixture must use a 1M model or this test cannot detect the clobber"
    );

    app.refresh_context_limit_after_account_change();

    assert_eq!(
        app.context_limit, expected_limit,
        "switching accounts changes credentials, not the model's context window"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p jcode-tui manual_account_switch_keeps_the_model_context_limit`
Expected: FAIL — `no method named 'refresh_context_limit_after_account_change'`

- [ ] **Step 3: Write minimal implementation**

Add to `crates/jcode-tui/src/tui/app/model_context.rs`, next to `update_context_limit_for_model`:

```rust
    /// Recompute the context limit after an account change.
    ///
    /// Account switches change credentials, not the model, so the limit must
    /// come from the active model (which may be a 1M variant) rather than from
    /// the local provider's default window.
    pub(in crate::tui::app) fn refresh_context_limit_after_account_change(&mut self) {
        let active_model = self.provider.model();
        self.update_context_limit_for_model(&active_model);
    }
```

Then replace all eight occurrences of this pattern:

```rust
                app.context_limit = app.provider.context_window() as u64;
                app.context_warning_shown = false;
```

with:

```rust
                app.refresh_context_limit_after_account_change();
```

(In `auth.rs` the receiver is `self`, so use `self.refresh_context_limit_after_account_change();`.) `update_context_limit_for_model` already sets `context_warning_shown = false`, so dropping that line is correct — verify by reading `model_context.rs:630-640` before deleting.

- [ ] **Step 4: Run test to verify it passes**

Run: `source "$HOME/.cargo/env" && cargo test -p jcode-tui manual_account_switch_keeps_the_model_context_limit`
Expected: PASS

- [ ] **Step 5: Verify no call site was missed**

Run: `grep -rn "context_limit = .*provider.context_window()" crates/jcode-tui/src | grep -v tests`
Expected: no output

- [ ] **Step 6: Run the crate's test suite**

Run: `source "$HOME/.cargo/env" && cargo test -p jcode-tui 2>&1 | tail -3`
Expected: no new failures

- [ ] **Step 7: Commit**

```bash
source "$HOME/.cargo/env" && cargo fmt -p jcode-tui
git add crates/jcode-tui/src/tui/app
git commit -m "fix(tui): keep the model's context limit across account switches"
```

---

### Task 8: Verify end to end and document

**Files:**
- Modify: `docs/DAIN_ACCOUNT_FIRST_FAILOVER.md` (extend the "Part 2" section)

**Interfaces:**
- Consumes: everything above.
- Produces: documentation only.

- [ ] **Step 1: Full build and test of the touched crates**

Run:
```bash
source "$HOME/.cargo/env"
cargo test -p jcode-base -p jcode-protocol -p jcode-app-core -p jcode-tui -p jcode-provider-anthropic-runtime 2>&1 | tail -6
```
Expected: only the 6 pre-existing `jcode-base` failures listed in Global Constraints. Anything else must be fixed before continuing.

- [ ] **Step 2: Lint the touched crates**

Run: `source "$HOME/.cargo/env" && cargo clippy -p jcode-base -p jcode-protocol -p jcode-app-core -p jcode-tui -p jcode-provider-anthropic-runtime --all-targets 2>&1 | grep -E "^(warning|error)" | head -10`
Expected: no warnings attributable to files touched by this plan

- [ ] **Step 3: Prove the wire event survives an unknown-variant client**

Run:
```bash
source "$HOME/.cargo/env" && cargo test -p jcode-tui skipping_unparseable 2>&1 | tail -3
```
Expected: the existing stray-line handling tests still pass, confirming an old client skips rather than dies on this event. If no such test exists, add one that feeds `{"type":"anthropic_account_rotated","label":"claude-2"}` through the line classifier used at `backend.rs:1204` and asserts `LineOutcome::Event`.

- [ ] **Step 4: Runtime check against a private socket**

Run:
```bash
source "$HOME/.cargo/env" && cargo build --profile selfdev --bin jcode
./target/selfdev/jcode run --no-update --socket /tmp/jcode-rotsync-$$.sock 'Reply with exactly one word: pong'
```
Expected: `pong`. This proves the new event plumbing did not break a normal turn. Do NOT restart the shared daemon.

- [ ] **Step 5: Document the behavior**

Append to the "Part 2" section of `docs/DAIN_ACCOUNT_FIRST_FAILOVER.md`:

```markdown
### Client sync (August 2026)

Rotation happens in the daemon, but the TUI is a separate process that renders usage from its own `crate::usage::get_sync()` cache, so a rotation used to be invisible: the widget kept showing the exhausted account's 5-hour reset.

The runtime now publishes `BusEvent::AnthropicAccountRotated` on every account change (including the restore when every account is limited).
`client_lifecycle.rs` forwards it to each subscribed client as `ServerEvent::AnthropicAccountRotated` and drops the daemon's own usage cache.
The client mirrors the account with an override (not a persisted switch), invalidates its auth and usage caches, prints the switch, and recomputes the context limit from the active model.

`UsageData` carries no account identity and its staleness is time-based, so an account change now calls `usage::invalidate_anthropic_usage()`; without it a fresh-but-wrong snapshot is served until the TTL expires.

All account-switch paths use `refresh_context_limit_after_account_change()` instead of assigning `provider.context_window()`, which dropped 1M windows down to the local default of 200k.
```

- [ ] **Step 6: Commit**

```bash
git add docs/DAIN_ACCOUNT_FIRST_FAILOVER.md
git commit -m "docs: describe how account rotation reaches the client"
```

---

## Self-Review

**Spec coverage:** Client learns about rotation (Tasks 2, 3, 4, 5, 6); usage widget shows the new account's window (Tasks 1, 5, 6); context stays 1M (Tasks 6, 7); bidirectional rotation needs no work (candidate list already excludes the active account); restore path also announced (Task 3).

**Placeholder scan:** No TBDs. Two tasks deliberately defer to existing code the implementer must read first — the fixture in Task 3 (`RotationFixture`, extracted from the existing rotation test) and the `App` builder in Tasks 6 and 7 — because inventing new names for them would conflict with what is already in those files.

**Type consistency:** `AnthropicAccountRotated { label, previous_label }` is used identically in Tasks 2, 3, 4, 5, 6. `invalidate_anthropic_usage()` (Task 1) is called in Tasks 5 and 6. `update_context_limit_for_model(&str)` is the existing method; `refresh_context_limit_after_account_change()` (Task 7) wraps it and is used only in TUI switch paths.

**Known risk carried, not fixed:** account identity remains process-global in the daemon (`auth::account_store` keys by `"claude"`, one shared `AnthropicProvider` credential slot). With several sessions on one daemon, a rotation announced to all clients is accurate for the process but not per-session. Fixing that is per-session account attribution, an architectural change out of scope here.
