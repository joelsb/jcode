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

The runtime now publishes `BusEvent::AnthropicAccountRotated` on every account change.
`client_lifecycle.rs` forwards it to each subscribed client as `ServerEvent::AnthropicAccountRotated` and drops the daemon's own usage cache.
The client mirrors the account with an override (not a persisted switch), invalidates its auth and usage caches, prints the switch, and recomputes the context limit from the active model.
Because the event changes what the widget shows while no stream deltas are arriving to repaint - a rotation happens during a 429 retry - the client handler returns `needs_redraw = true`.

The give-up path (every candidate limited, original override put back) publishes the same event with `restored: true`, via `publish_anthropic_account_restored`.
Without that flag the client wording reads as "carrying on elsewhere" when the truth is "out of options".
The field is `#[serde(default)]` and skipped when false, so an older daemon that omits it still parses and still means an ordinary rotation.

The bus event has no session id, so the daemon fans it out to *every* connected client - unlike `BatchProgress` and `SidePanelUpdated`, which filter on `client_session_id`.
The client therefore drops the event when its own session is positively on another provider (`App::ignores_anthropic_account_events`), failing open while provider identity is still unsettled.
Adding a session id to the event needs the same per-session account attribution that the section above defers.

`UsageData` carries no account identity and its staleness is time-based, so an account change invalidates it explicitly; without that a fresh-but-wrong snapshot is served until the TTL expires.
Two forms exist for this: `usage::invalidate_anthropic_usage_now()` awaits the write and is what the daemon uses, because it sends the wire event immediately afterwards and the client's `/usage` round-trip would otherwise race a deferred write; `usage::invalidate_anthropic_usage()` is the sync form for UI code with nowhere to await.
Both bump `USAGE_EPOCH`, which exists because `REFRESH_IN_FLIGHT` alone lets a fetch that started before the switch land afterwards and stamp the previous account's numbers as fresh - and once they look fresh, no refresh runs for a full `CACHE_DURATION`.
A refresh that completes under a changed epoch re-marks the cache stale instead.

The remote account-switch paths (`auth_account_commands.rs` x4, `remote.rs` x2) use `refresh_context_limit_after_account_change()` instead of assigning `provider.context_window()`, which dropped 1M windows to the local default of 200k.
Only the remote sites were ever wrong: `app.provider` is an inert stub in remote mode, whereas the local `AnthropicProvider::context_window()` is `context_window::resolve(&self.model())` and already model-aware.
The two local sites in `auth.rs` were converted for consistency, not to fix a bug.

## Part 3 - The Verdict Behind It All (August 2026)

Parts 1 and 2 got the *mechanism* right and were still defeated by a wrong *fact*.

`AccountUsageSnapshot::exhausted` required **both** the 5-hour and weekly windows to be full:

```rust
exhausted: usage.five_hour >= 0.99 && usage.seven_day >= 0.99,   // wrong
```

Anthropic blocks a request when **either** is full. An account at 5h=100% / 7d=49% is hard-blocked for hours and was reported as having headroom, so rotation kept selecting it, the "every account is spent" fast-fail never fired, and turns spent four 60s waits on a limit that could not clear. Fixed as a disjunction, with `blocking_reset_at()` reporting the reset of the window that is actually blocking. See ADR-0016.

Compounding it, and each fixed here:

- **Two usage caches existed and rotation invalidated the wrong one.** The single-slot `USAGE` cache (account-blind, feeds the widget) was invalidated with the epoch guard from Part 2. The *per-account* `ANTHROPIC_USAGE_CACHE`, which is what `account_usage_probe_sync` and therefore every rotation decision reads, had **no invalidation path at all**: only a 300s TTL, stretched to 900s by `RATE_LIMIT_BACKOFF`. A 429 could not correct the verdict that contradicted it. The two also used different key shapes for the same account (token prefix versus label), so neither could invalidate the other. Now label-keyed throughout, corrected by a 429, and refreshed in the background after a rotation. See ADR-0017.
- **The rotation cooldown was a flat 60s from Part 2**, sized for a burst limit. Applied to a 3-hour quota wall it meant jcode forgot within a minute and reselected the blocked account. It now stores an expiry sized to the real reset, clamped to [60s, 6h].
- **A cooling account was only sorted last**, which is not enough when every candidate is cooling, since the order among equals is arbitrary. It is now skipped entirely: when every account is cooling the candidate list comes back empty and the request falls through to the ADR-0015 wait, which is strictly cheaper than paying another 429 to learn what the cooldown already knew. (Superseded the earlier "kept as a last resort" rule, which caused a switch-and-restore on every turn.)
- **A quota `retry-after: 60` was honoured verbatim on the burst path**, producing the four-minute silence. Capped at 15s, and the terminal failure now names the earliest reset rather than offering a model downgrade that shares the same limit.
- **The process-global override let concurrent sessions drag each other between accounts.** A rotation now claims the shared override only when the currently-active account is one this request has already proven refuses it. This narrows ADR-0009's blast radius; it does not resolve it. See ADR-0018.

## Part 4 - The Concurrency Behind the Rest (2026-08-14)

Parts 1-3 made each session's *verdict* correct. They did not make concurrent sessions agree, and with five sessions open on one daemon the switch spam came back on a binary carrying every fix above.

- **Rotation was a check-then-act race across sessions.** Five sessions hit the same 5-hour limit within seconds; each read the candidate list, each picked the other account, each fired. The cooldown that would have warned the others off is only recorded when the first 429 comes *back*, by which point everyone has committed. Observed: four distinct sessions alternating `claude-1`/`claude-2` inside 25 seconds.
- **Rotation is now serialised** on a process-wide async mutex, and a session that acquires it re-reads the shared state first: if a sibling already moved the pool to a usable account, it adopts that account instead of rotating again. One pool-wide move, one switch line, regardless of how many sessions are open. See ADR-0020.
- **The lock alone is not the fix.** Without the adopt path the leader takes the only other account and every follower dead-ends into the rate-limit wait: measured 4 of 5. Both halves are pinned by negative controls in `concurrent_sessions_rotate_the_shared_pool_only_once`.

Diagnostic worth keeping: single-session cross-turn flapping always pairs a switch with a **"restored"** line. Multi-session stampede shows switches with no restores. Same-looking symptom, different bug, different fix.

Note for anyone touching the caches: the per-account cache is the one decisions read. Invalidating the single-slot one alone looks correct and changes nothing.

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

### Part 3 (the exhaustion verdict and the caches)
- `crates/jcode-base/src/usage/model.rs` - `UsageData::exhausted` is now a disjunction over windows; added `EXHAUSTED_RATIO` and `blocking_reset_at()`; same change for the OpenAI-shaped `all` -> `any`
- `crates/jcode-base/src/usage/cache.rs` - Per-account cache controls that did not exist before: `invalidate_anthropic_account_usage`, `note_anthropic_account_rate_limited`, `cached_anthropic_account_usage`, plus test seams
- `crates/jcode-base/src/usage/accessors.rs` - Probe reads the per-account cache for *every* account including the active one; `fetch_usage_for_account_sync` keyed by label not token; added `refresh_account_usage`
- `crates/jcode-base/src/provider/account_failover.rs` - Cooldown stores an expiry rather than a start (`note_account_rate_limited_until`); cooling accounts skipped while a healthy one exists (`demote_cooling_down_accounts`); `note_account_rate_limited_with_reset`, `clear_account_rate_limit`, `earliest_account_reset`
- `crates/jcode-provider-anthropic-runtime/src/lib.rs` - `MAX_BURST_RETRY_AFTER` caps a quota `retry-after` on the burst path; quota failures announce the earliest reset once per turn; rotation claims the shared override only when better informed (`AccountSwitch::claimed_shared_override`) and kicks a background usage refresh

## Commits

```
2b8b449 fix: remove duplicate private imports that caused unused-import warnings
2c9502d feat: add prefer_account_failover config option
6273711 feat: wire rate_limit flag into TUI fallback route picker
c0eeca7 feat: skip model downgrade on rate-limit, prefer cross-provider fallback

9dfe8d2 fix(anthropic): treat any spent usage window as an exhausted account
```

ADRs: [0016](adr/0016-exhaustion-is-any-window-not-every-window.md) (the verdict), [0017](adr/0017-invalidate-the-cache-the-decision-reads.md) (the caches), [0018](adr/0018-claim-the-shared-account-override-only-when-better-informed.md) (cross-session), plus a correction recorded in [0015](adr/0015-rate-limits-wait-on-their-own-clock.md).

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
