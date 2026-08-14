# TASK 2: Same prompt resubmitted 335 times (token burn)

**Status: DONE.** Fixed by the delegated session, merged to `mine` (`3fba700b1`), built and installed.

Root cause found: a busy-rejected prompt re-adopted the running turn via
`remote_resume_activity` with `current_message_id == None`, and a blanket reset
cleared that marker on almost every server event including plain progress events.
Cleared mid-turn, `process_remote_followups` read the state as idle-restored-startup
and redispatched the same prompt - once per progress event, for as long as the turn
kept streaming. Kept below for the original evidence.
**Branch:** `mine`
**Child session:** `session_daisy_1786708230532_419069395c9813d3` (check whether it committed anything)
**Owner:** whoever picks this up next. Self-contained; read this file only.

---

## 1. The symptom

The user's TUI showed the **same prompt** submitted as message 6, 7, 8 ... **335+**, without them typing it:

```
  6> find the bug and make it only swich accout once!
  7> find the bug and make it only swich accout once!
  ...
335> find the bug and make it only swich accout once!
```

## 2. This is real money, not a display glitch

Verified in `~/.jcode/logs/jcode-2026-08-14.log`:

| Evidence | Value |
|---|---|
| `turn=` counter in `KV_CACHE_USAGE` | reached **336** |
| `session_read` on that session | **72,804,691** cached-read tokens |
| Resubmit cadence | ~1 every 8-10 seconds |
| `QUEUE_PUSH` events in the whole log | only **13** |

That last row matters: the message **queue is probably not the duplicator**. Something else is re-dispatching.

## 3. Leading suspect

**910** log lines of:

```
Dispatching restored startup/queued followup without active remote message id
```

bursting at **107 in a single minute** (12:39). Start here:

- `crates/jcode-app-core/src/server/reload_recovery.rs`
- `handle_resume_all_sessions`, `live_session_owes_continuation`, `pending_directive_for_session` in `crates/jcode-app-core/src/server/client_actions.rs`

Hypothesis to confirm or kill: reload-recovery or auto-continuation re-dispatches a followup whose "owes continuation" state is **never cleared**, so it re-fires forever.

Supporting hint: a different loop in this codebase was already guarded, and that guard's log line exists:

```
Todo completion gate exhausted after 5 attempts; stopping auto-poke to avoid an infinite continuation loop
```

So this class of bug has bitten before. **This path may simply be missing the equivalent cap.**

## 4. What to do

1. **Reproduce first.** Do not propose a fix you have not seen fail. The defining property is that it resubmits with **no user input**.
2. Find the root cause; do not stop at adding a counter cap unless you have shown the state genuinely cannot be cleared correctly.
3. Test that **fails before, passes after**. Run the negative control and paste what it printed.
4. Full suites: `cargo test -p jcode-app-core --lib`, `cargo test -p jcode-base --lib`.
   Pre-existing failures on this branch, **not yours**: `platform::platform_tests::spawn_detached_creates_new_session`, `session::tests::cases::streaming_guard_creates_visible_macos_sleep_assertion`. Verify against base `34635f40f` rather than trusting this file.
5. ADR in `docs/adr/` if the fix encodes a constraint someone could undo. House style: see `0015`-`0018`.
6. Commit on `mine`.

## 5. Trap that cost me time twice today

It is easy here to write an assertion **satisfied by the broken state**. Assert on the number of dispatches, not on a proxy like "the session eventually settles". If your test passes on the first run, revert the fix and confirm it actually fails.

## 6. Boundaries

- Do **not** touch `crates/jcode-provider-anthropic-runtime/` or `crates/jcode-base/src/provider/account_failover.rs`. TASK 1 owns those and has **uncommitted changes** there.
- Do **not** run `jcode server reload` and do **not** kill sessions or processes. The user's call.

## 7. Why this outranks TASK 1

TASK 1 is noisy and wasteful. **This one spends real tokens**: 72.8M cached-read on a single session. If you only have budget for one, do this one.
