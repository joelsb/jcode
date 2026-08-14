# ADR-0019: Only turn-terminal events clear the re-adopted-turn marker

Status: Accepted
Date: 2026-08-14

## Context

A user's TUI resubmitted the same prompt ("find the bug and make it only
swich accout once!") as message 6, 7, 8 ... up to message 336+ in one
session, with no user input in between. `KV_CACHE_USAGE` logs showed
`turn=336` on that session and 72.8M cached-read tokens burned in a single
sitting. The client log showed the same line, once per resubmit:

```
Dispatching restored startup/queued followup without active remote message id
```

repeating roughly every few seconds to every fraction of a second, ~500
times across the session.

### Root cause

`process_remote_followups` treats a specific state combination as "a
restored startup/queued follow-up that must be (re)dispatched":

```rust
let synthetic_startup_dispatch = app.is_processing
    && app.current_message_id.is_none()
    && app.remote_resume_activity.is_none()
    && (app.submit_input_on_startup
        || !app.queued_messages.is_empty()
        || !app.hidden_queued_system_messages.is_empty());
```

That combination is also produced deliberately by the busy-rejection
recovery path (`ServerEvent::Error { message: "Already processing a
message" }`, added in #391/ADR-adjacent commit d586792f0): when the server
rejects a message because a prior turn is still running, the client puts
the message back on the queue and "re-adopts" the still-running turn by
setting `app.remote_resume_activity = Some(...)` with
`current_message_id == None` (there is no id to correlate against - the
turn was never this client's own request).

`handle_server_event` had a single blanket reset that cleared
`remote_resume_activity` on almost every event the server can emit for a
turn, including every live progress signal:

```rust
if matches!(&event,
    ServerEvent::TextDelta { .. } | ServerEvent::ToolStart { .. } | ServerEvent::ToolInput { .. }
        | ServerEvent::ToolExec { .. } | ServerEvent::ToolDone { .. } | ServerEvent::TokenUsage { .. }
        | ServerEvent::ConnectionPhase { .. } | ... | ServerEvent::Done { .. } | ServerEvent::Error { .. }
) {
    app.remote_resume_activity = None;
}
```

A long-running turn streams dozens of these progress events (a tool call
alone fires `ToolStart`, `ToolInput` deltas, `ToolExec`, `ToolDone`). The
very next such event after the re-adoption cleared the marker even though
the turn was still running. On the following tick,
`process_remote_followups` then saw `is_processing && current_message_id
== None && remote_resume_activity == None`, mistook the still-running
turn for an idle restored-startup state, flipped `is_processing` back to
`false`, and redispatched the same queued prompt - which the server
promptly rejected again as busy, which re-adopted the turn again, which
the next progress event cleared again. The loop repeated once per progress
event for as long as the turn kept streaming, sending the identical prompt
hundreds of times and re-reading the entire (growing) conversation history
into the KV cache on every resend.

## Decision

Only turn-*resolving* events clear `remote_resume_activity`:
`ServerEvent::Interrupted`, `ServerEvent::Done`, and `ServerEvent::Error`.
Every other event (`TextDelta`, `ToolStart`, `ToolInput`, `ToolExec`,
`ToolDone`, `TokenUsage`, `ConnectionPhase`, etc.) is evidence the
re-adopted turn is still alive and must leave the marker alone.

This mirrors what the marker is actually for: distinguishing "no id, but a
turn is genuinely in flight" from "no id, and idle" so
`process_remote_followups`'s startup-dispatch heuristic cannot misfire.

## Consequences

- A busy-rejected prompt now stays queued for the entire lifetime of the
  turn it collided with, and only redispatches once that turn truly ends
  (Done/Error/Interrupted), instead of once per progress event.
- The cost of getting this wrong is severe and silent: no error is shown to
  the user, the transcript just fills with a system-echoed prompt over and
  over while token/cache usage climbs. Revisit this gate carefully - if a
  future terminal-ish event is added to `ServerEvent`, it needs an explicit
  decision about whether it resolves a turn (belongs in the clear-list) or
  is progress (must not).
- Test: `busy_rejection_requeue_does_not_resubmit_on_every_tool_event` in
  `crates/jcode-tui/src/tui/app/remote_tests.rs` reproduces the loop with a
  fake busy-rejected prompt and asserts zero redispatches across repeated
  `ToolStart` events for the still-running turn. It failed before this fix
  (5 redispatches in 5 ticks) and passes after.

## Evidence

- `~/.jcode/logs/jcode-2026-08-14.log`: `KV_CACHE_USAGE: turn=336 ... session_read=71665274` /
  `session_read=72804691` on `session_crab_1786527104159_ffd9e73b273ea52d`.
- 501 occurrences of `Dispatching restored startup/queued followup without
  active remote message id` in that log, each immediately preceded by
  `Recovering in-flight queued continuation into queued follow-ups after
  server busy rejection` / `Server rejected a message because a turn is
  still running; re-queued it and re-adopted the running turn`.
- `crates/jcode-tui/src/tui/app/remote/server_events.rs`, the
  `ServerEvent::Error { message: "Already processing a message", .. }` arm
  (introduced in commit d586792f0, "fix(tui): queue a user prompt the
  server rejects as busy").
