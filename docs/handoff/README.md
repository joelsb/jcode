# START HERE: handoff tasks on `mine`

## Open

| # | Bug | File | Status |
|---|---|---|---|
| 3 | A dead MCP child keeps serving from a tombstone handle; `tools/list_changed` is dropped | `TASK-3-mcp-stale-handle-and-list-changed.md` | **not started**, nothing fixed |

Task 3 is unrelated to the rate-limit work below. Read its file only.

---

# The rate-limit work (both done)

Updated 2026-08-14 after both tasks were completed and installed.

## Both bugs are fixed, committed, and installed

| # | Bug | Commit | Status |
|---|---|---|---|
| 1 | Account ping-pong: switch once, then stay | `aa92245d3` | fixed, merged |
| 2 | Same prompt resubmitted 335 times (72.8M tokens) | `3fba700b1` | fixed, merged |

Installed binary: `~/.jcode/builds/versions/3fba700b1-dirty/jcode`, verified by `strings` to contain the new code, and a live turn succeeds.

Task detail files are kept for the reasoning and the reproduction steps:
`TASK-1-burst-account-pingpong.md`, `TASK-2-prompt-resubmit-loop.md`.

---

## TASK 1: account ping-pong

Two compounding causes on the dead-end path (when every account answers 429):

1. `should_restore_account` handed the session back to the account it began on. An earlier fix gated that on *quota* being spent, which never fires on a **burst** limit; the user's accounts had headroom (0% and 34% of their 5-hour windows). The gate is now the **cooldown**, which every 429 records, so quota and burst are both covered.
2. `demote_cooling_down_accounts` returned cooling accounts when none were ready. Each turn then hopped onto an equally-limited account and bought a second 429. It now returns nothing.

Returning nothing does **not** fail the turn, and that is the load-bearing part: the caller's `None` branch is the dead end, which is where ADR-0015's rate-limit wait lives. Pinned by `no_candidates_still_waits_out_a_burst_rather_than_failing`.

Measured over 3 turns: **3 rotations / 21 requests → 1 rotation / 19 requests.**

## TASK 2: the resubmit loop

A prompt the server rejected as busy re-adopted the running turn by setting `remote_resume_activity` with `current_message_id == None`. A blanket reset cleared that marker on *almost every* server event, including plain progress events (`ToolStart`, `TextDelta`, `TokenUsage`, ...). Once cleared mid-turn, `process_remote_followups` mistook the state for idle-restored-startup, flipped `is_processing` back to false, and redispatched the same prompt. The server rejected it as busy again, and the next progress event repeated it. **One resubmit per progress event, for as long as the turn kept streaming.**

## Test state

- `cargo test -p jcode-provider-anthropic-runtime --lib` → 74 pass, 0 fail
- `cargo test -p jcode-base --lib provider::` → 175 pass, 0 fail
- `cargo test -p jcode-base --lib` → 2 failures that are **pre-existing** and also fail on base commit `34635f40f`:
  - `platform::platform_tests::spawn_detached_creates_new_session`
  - `session::tests::cases::streaming_guard_creates_visible_macos_sleep_assertion`

## The one thing still outstanding, and it is yours

**`jcode server reload`.** Interactive sessions are served by the long-lived daemon, which still runs the pre-fix binary `34635f40f`. Until it is reloaded, neither fix is live for your running sessions. It was left undone deliberately: reloading hands over live sessions, which is your call, not mine.

## Warning for anyone editing these tests

Global state (`RATE_LIMITED_ACCOUNTS`, the per-account usage cache) is process-wide and leaks between tests even when `JCODE_HOME` is swapped. Use `lock_account_cooldown_tests()` plus the `clear_*` helpers.

Three tests written during this work initially **passed against the bug they were meant to catch**. If a test here passes on its first run, revert the fix and confirm it fails. Assert on the thing that distinguishes working from broken (count of rotations, count of dispatches), not on a proxy.

## ADRs

`0016` (exhaustion = any window), `0017` (invalidate the cache the decision reads), `0018` (shared override, a no-op with only 2 accounts), plus amendments to `0003` (twice: quota, then cooldown) and `0015`.
