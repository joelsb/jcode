# Account-First Failover - DAIN Patch

Custom patch by DAIN (Joel) on branch `feat/account-first-failover`.
Adds `prefer_account_failover` config to jcode.

## Problem

When a model hits a 429 rate limit, jcode's TUI-layer fallback (`pick_next_fallback_route` in `fallback_pick.rs`) fires first and offers a smaller model on the same provider (Sonnet -> Fable -> Haiku).
The backend account failover (`try_same_provider_account_failover` in `multi_provider.rs`) never gets a chance because the TUI already downgraded.

With multiple OAuth accounts (joel, elara), we want: same model on next account before any model downgrade.

## Solution

Added `rate_limit: bool` flag to `FallbackPickOptions`.
When `rate_limit` is true, same-provider-different-model routes get tier 3 (below cross-provider tier 2), so the picker skips model downgrades.
This lets the backend account-failover mechanism try the same model on a different account first.

Gated by `prefer_account_failover` config option (default `true`).

## Part 2 - In-Stream Rate Limits (August 2026)

The route-picker tiering above was necessary but not sufficient: streaming 429s never reached account failover at all.

### Why the first patch could not fire

`AnthropicProvider::complete*` returns `Ok(Box::pin(ReceiverStream::new(rx)))` before the HTTP request is sent, then runs `run_stream_with_retries` in a spawned task.
A 429 therefore arrives as an `Err` *item inside the event stream*, not as an `Err` return from `complete_on_provider`.
`MultiProvider::try_same_provider_account_failover` is only called from the `Err(err)` arm of `complete_with_failover` (`crates/jcode-base/src/provider/mod.rs`), so for streaming requests it was unreachable.

Evidence from a real session: 522 log lines mentioning `429`, zero lines mentioning `Same-provider failover`.
The retry loop also burned its whole budget on the same exhausted account, honoring the 60s `retry-after`, before surfacing the error and letting the TUI offer a model downgrade.

### What was added

Account rotation inside `run_stream_with_retries` (`crates/jcode-provider-anthropic-runtime/src/lib.rs`):

- On a usage-limit error with no output yet on the attempt (`should_rotate_account`), pick the next untried account from `account_failover_candidates(ActiveProvider::Claude)`, set the account override, load or refresh that account's token, and replay the same model.
- `retry-after` is cleared on rotation: the new account has its own quota bucket.
- Rotation gets its own attempt budget (`MAX_RETRIES + alternate account count`) and transport retries are counted separately (`transport_retries`), so rotating never consumes the retries reserved for network faults.
- Accounts whose credentials fail to load or refresh are marked tried and skipped.
- If every account is limited, the original account override is restored (`restore_claude_account`) and the error surfaces as before, so the TUI model downgrade is the last resort rather than the first.
- The switch is visible: `StreamEvent::StatusDetail` in the transcript plus a `Same-provider account failover` log line.

Scope: Anthropic HTTPS/SSE runtime (both unified and split system paths, which share `run_stream_with_retries`).
The Claude CLI runtime has its own retry loop and is not covered.

### Review fixes applied on top

- **Restore only on a dead end.** `restore_claude_account` used to fire on every hard-failure exit once a rotation had happened, so an unrelated failure *after* a successful rotation re-pinned the session onto the account already known to be limited. It now runs only when `switch_to_next_claude_account` returned `None` (`should_restore_account`).
- **No blind write to shared state.** The account override is process-wide and one daemon serves many sessions, so the restore now writes only if the override still holds the label this request put there. Anything else means another session switched accounts meanwhile (possibly a deliberate `/account switch`) and is left alone.
- **Failed candidates roll back.** If a candidate account's credentials cannot be loaded or refreshed, the override is put back before trying the next one, so it never lingers on an account that cannot serve a request.
- **Rotation cannot be starved.** The attempt budget starts at `MAX_RETRIES` and each successful rotation grants itself one extra attempt, instead of pre-sizing the budget by account count where the other one-shot retry causes (forced refresh, model fallback, reasoning strip) could consume it first.
- **Mid-stream limits rotate too.** A usage limit arriving after partial output used to skip rotation and fall into the generic transient retry, honoring the exhausted account's 60s `retry-after` - the original bug in a narrower form. Rotation now also covers that case, emitting `StreamEvent::RetryRollback` first so the consumer discards the partial attempt, the same contract the transient path already uses.

### Known limitation (not fixed here)

Account identity is process-global: `auth::account_store` keys the active override by `"claude"` alone, and one `AnthropicProvider` (with a single-slot credential cache) is shared by every session on the daemon.
A rotation therefore affects concurrent sessions, and the single credential slot can hand another session the rotated account's token.
`MultiProvider::try_same_provider_account_failover` has the same property, so this is pre-existing rather than new, but the rotation window here spans a whole streaming turn.
A real fix is per-session account attribution, which is an architectural change well beyond this patch.

### Client sync (August 2026)

Rotation happens in the daemon, but the TUI is a separate process that renders usage from its own `crate::usage::get_sync()` cache, so a rotation used to be invisible: the widget kept showing the exhausted account's 5-hour reset.

The runtime now publishes `BusEvent::AnthropicAccountRotated` on every account change (including the restore when every account is limited).
`client_lifecycle.rs` forwards it to each subscribed client as `ServerEvent::AnthropicAccountRotated` and drops the daemon's own usage cache.
The client mirrors the account with an override (not a persisted switch), invalidates its auth and usage caches, prints the switch, and recomputes the context limit from the active model.

`UsageData` carries no account identity and its staleness is time-based, so an account change now calls `usage::invalidate_anthropic_usage()`; without it a fresh-but-wrong snapshot is served until the TTL expires.

All account-switch paths use `refresh_context_limit_after_account_change()` instead of assigning `provider.context_window()`, which dropped 1M windows down to the local default of 200k.

## Files Changed

### Core logic
- `crates/jcode-provider-core/src/fallback_pick.rs` - Added `rate_limit` field to `FallbackPickOptions`, updated tier assignment, 3 new tests (14 total pass)
- `crates/jcode-provider-anthropic-runtime/src/lib.rs` - Account rotation in the streaming retry loop: `should_rotate_account`, `switch_to_next_claude_account`, `credentials_for_claude_account`, `next_untried_account`, `claude_account_candidate_count`, `restore_claude_account`; separate transport-retry counter and extended attempt budget
- `crates/jcode-base/src/provider/account_failover.rs` - Added public `account_failover_candidates(provider)` so a provider runtime can rotate accounts itself

### Wiring
- `crates/jcode-base/src/provider/account_failover.rs` - Made `error_looks_like_usage_limit` public, added `prefer_account_failover_enabled()` accessor
- `crates/jcode-base/src/provider/mod.rs` - Added `pub use` re-exports for new public functions
- `crates/jcode-tui/src/tui/app/model_context.rs` - Set `rate_limit` flag in `FallbackPickOptions` construction, gated on `prefer_account_failover_enabled() && error_looks_like_usage_limit(error)`

### Config
- `crates/jcode-config-types/src/lib.rs` - Added `prefer_account_failover: bool` to `ProviderConfig` (default `true`)
- `crates/jcode-base/src/config/env_overrides.rs` - Added `JCODE_PREFER_ACCOUNT_FAILOVER` env var
- `crates/jcode-base/src/config/default_file.rs` - Added config comment
- `crates/jcode-base/src/config.rs` - Added env var to known list

## Commits

```
2b8b449 fix: remove duplicate private imports that caused unused-import warnings
2c9502d feat: add prefer_account_failover config option
6273711 feat: wire rate_limit flag into TUI fallback route picker
c0eeca7 feat: skip model downgrade on rate-limit, prefer cross-provider fallback
```

## Config

```toml
[provider]
# On rate limit, try same model on a different account before downgrading model (default: true)
prefer_account_failover = true
# Also requires:
same_provider_account_failover = true
```

Env var: `JCODE_PREFER_ACCOUNT_FAILOVER=true|false`

## Build and Install

```bash
cd ~/MYNE/Projects/jcode
git checkout feat/account-first-failover
source "$HOME/.cargo/env"
cargo build --release
bash scripts/install_release.sh --fast
```

## Architecture Notes

Two separate failover mechanisms exist:
1. **Route fallback** (TUI layer, `model_context.rs`): picks different model/provider via `pick_next_fallback_route`. User-facing (Ctrl+Y to accept).
2. **Account failover** (provider layer, `multi_provider.rs`): tries same model on different accounts via `try_same_provider_account_failover`. Automatic, no user interaction.

`ModelRoute` struct does not carry account labels - account failover is a completely separate mechanism from route-based fallback.

The tier system in `pick_next_fallback_route_with_options`:
- Tier 0: same model, different auth method (always best)
- Tier 1: same provider, different model (normal case)
- Tier 2: different provider (cross-provider hop)
- Tier 3: same provider, different model WITH rate_limit flag (our addition - pushes below cross-provider)
