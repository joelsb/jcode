# ADR-0011: Broadcast rotation to every client and filter it client-side by provider

Status: Accepted
Date: 2026-08-04 (added after code review of `7a5fe6a` and `3609277`)
Plan: `docs/superpowers/plans/2026-08-04-account-rotation-tui-sync.md`

## Context

The bus-to-client forwarding loop in `client_lifecycle.rs` filters its neighbours by session:

```rust
Ok(BusEvent::BatchProgress(progress)) => {
    if progress.session_id == client_session_id { ... }
}
Ok(BusEvent::SidePanelUpdated(update)) => {
    if update.session_id == client_session_id { ... }
}
```

The rotation arm was added without a filter, so every connected client received it. Consequences for a session that is not on Anthropic at all: it prints "⚡ Rate limited; switched Anthropic account: claude-1 → claude-2", calls `set_active_account_override` in its own process, and repoints its usage widget at an account it never talks to. A user on a Codex session would be told they were rate limited by a provider they are not using.

Matching the neighbours was not available. `BusEvent::AnthropicAccountRotated` carries no session id, and there is nothing to fill one from: the rotation happens inside `run_stream_with_retries`, whose only session-shaped parameter is `oauth_session_id` - a `Uuid::new_v4()` minted per provider instance for the Anthropic API's own request metadata, unrelated to the daemon's `client_session_id`. Plumbing a real session id from the client connection down through the provider into the streaming retry loop is the per-session attribution work that [ADR-0009](0009-defer-per-session-account-identity.md) defers.

## Decision

Leave the broadcast, and filter on the client by provider.

`App::ignores_anthropic_account_events()` returns true when this session's provider/model identity is *positively* something other than Anthropic; the rotation arm returns early in that case, touching no state.

The filter fails open. When provider and model are both still unknown - the pre-History bootstrap window - the event is processed. That direction is deliberate: acting on a rotation that turns out not to be ours costs one stray notice, while dropping one that is ours leaves the usage widget pointed at an exhausted account, which is the entire defect [ADR-0007](0007-announce-rotation-to-clients-over-the-bus.md) exists to fix.

Rejected alternatives:

- **Filter server-side on the session id.** Correct, and blocked on ADR-0009 as above.
- **Have the client compare the rotated label against its own active account.** Circular: the client's notion of the active account is the process-global override this event is what updates.
- **Send only to the client whose request triggered the rotation.** The account is process-global, so every session on the daemon really did move. Telling only one of them would be a different lie.

## Consequences

- A non-Anthropic session is unaffected by another session's rotation, and an Anthropic session still gets the announcement.
- Two Anthropic sessions on one daemon both adopt the override. That is correct for a process-global account and is the ADR-0009 defect, not this one.
- The filter is a provider heuristic (`contains("anthropic") || contains("claude") || model.starts_with("claude-")`), reusing the idiom already in `remote_reasoning_effort_hint`. A future provider whose name matches neither, but which routes to Anthropic, would be filtered out incorrectly. The failure mode is a missed notice, not corrupted state.
- Because the event is broadcast, its cost scales with connected clients: each connection's arm also invalidates the daemon usage cache, so N clients means N invalidations of one shared cache. Harmless at rotation frequency, and another reason this channel must not carry chatty events.
- This is a mitigation, not a fix. When per-session account identity lands, the filter should be replaced by a server-side session check, not kept alongside it.

## Evidence

- `crates/jcode-app-core/src/server/client_lifecycle.rs` - the unfiltered arm next to the two filtered ones.
- `crates/jcode-tui/src/tui/app/tui_state.rs` - `ignores_anthropic_account_events`.
- `rotation_event_for_a_non_anthropic_session_is_ignored` asserts an OpenAI session neither adopts the override nor is told it was rate limited.
