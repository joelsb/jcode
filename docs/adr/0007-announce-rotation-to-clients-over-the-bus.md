# ADR-0007: Announce rotation to clients over the bus and mirror it with an override

Status: Accepted
Date: 2026-08-04 (commits `38d6d80`, `1d65038`, `f122474`, `a4edfb1`, `7a5fe6a`, `3609277`, `72e4ef7`)
Plan: `docs/superpowers/plans/2026-08-04-account-rotation-tui-sync.md`

## Context

Rotation works, and the user cannot tell.
After a switch to the second account, the TUI kept rendering the first account's 5-hour bar and reset time.

The cause is a process boundary.
The agent runs in the daemon; the TUI is a separate client process that renders the widget from `crate::usage::get_sync()` evaluated locally (`crates/jcode-tui/src/tui/app/tui_state.rs:449`).
Rotation sets a *process-local* override inside the daemon, which the client cannot observe and was never told about.

A second, independent defect compounds it: `UsageData` has no account field (`crates/jcode-base/src/usage/model.rs`) and lives in one global TTL cache whose `is_stale()` is time-based.
After any account change the cache is fresh but describes the wrong account - which also affects the pre-existing manual `/account switch`, not just rotation.

## Decision

Treat an account change as an event rather than a private daemon fact.

1. Publish `BusEvent::AnthropicAccountRotated { label, previous_label, restored }` on the existing process-global bus from the rotation path, including the restore-on-dead-end case. The `restored` flag distinguishes the two outcomes; see [ADR-0012](0012-announce-a-dead-end-restore-as-its-own-outcome.md).
2. Forward it per connection in `client_lifecycle.rs` as a new `ServerEvent::AnthropicAccountRotated`, and invalidate the daemon's own usage cache *before* sending - otherwise `/usage` in the daemon stays as wrong as the widget was, and the client's follow-up request can beat the invalidation.
3. Add explicit usage-cache invalidation, since a time-based TTL cannot express "wrong account". The mechanism has more to it than one function call; see [ADR-0010](0010-invalidate-the-usage-cache-with-an-epoch-guard.md).
4. On the client, mirror the account with `set_active_account_override` - **not** a persisted `set_active_account`.

Point 4 is the load-bearing choice. Persisting would have needed no protocol work at all (the client would notice on its next poll), and was rejected: a transient, daemon-scoped reaction to a rate limit must not rewrite the account the user configured, and the restore path would rewrite it again.

## Consequences

- The widget shows the account that is actually serving requests, which matters because that number is what the user uses to decide whether to keep working.
- A new wire variant changes client/server compatibility. Verified tolerable: an older client logs and skips unknown event lines (`RemoteConnection::classify_protocol_line` in `crates/jcode-tui/src/tui/backend.rs`) rather than dying, counting them against `MAX_STRAY_REMOTE_PROTOCOL_LINES = 32` (`backend.rs:263`). Rotation events are rare, so the budget is not a practical concern - but this channel must not be used for chatty events.
- The client's mirrored override is per-client. With several clients attached to one daemon, each gets the same announcement, which is correct for a process-global account and wrong for per-session accounts. See [ADR-0009](0009-defer-per-session-account-identity.md) for the deferral and [ADR-0011](0011-filter-the-rotation-broadcast-client-side.md) for how the client keeps another session's rotation out of its own state.
- The client handler must return `needs_redraw = true`. This is not optional bookkeeping: a rotation lands during a 429 retry, when no stream deltas are arriving to repaint, so returning `false` leaves the notice and the corrected context limit invisible until unrelated input. The same trap is documented in-code on the `ModelCatalog` arm of `handle_server_event`, and it was hit here anyway - the first implementation returned `false`.
- Five crates gain coupling to one event shape (`jcode-base`, provider runtime, `jcode-protocol`, `jcode-app-core`, `jcode-tui`). Accepted because the alternative - polling account state from the client - adds latency and still cannot distinguish "no change" from "changed twice".
