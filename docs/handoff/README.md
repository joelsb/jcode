# START HERE: open work on `mine`

Written 2026-08-14, before the user ended all running sessions.
Each task file is **self-contained**. Read only the one you are picking up.

| # | Task | File | Status |
|---|---|---|---|
| 1 | Account ping-pong: switch once, then stay | `TASK-1-burst-account-pingpong.md` | fix works; **2 tests to reconcile**; on branch `wip/burst-pingpong` |
| 2 | Same prompt resubmitted 335 times | `TASK-2-prompt-resubmit-loop.md` | delegated to a child session; **verify, assume unfixed** |

**Do TASK 2 first if you only do one.** It burned 72.8M tokens on a single session. TASK 1 is noisy but costs far less.

---

## Immediate state of the repo

- Branch `mine`, last commit **`82b98f34e`**.
- `mine` is **clean**. TASK 1's work is committed on branch **`wip/burst-pingpong`** (`40a63c27f`), not on `mine`, because it still fails 2 tests.
  Recover with `git checkout wip/burst-pingpong` or `git cherry-pick 40a63c27f`.
- `cargo test -p jcode-provider-anthropic-runtime --lib` → **73 pass, 0 fail**
- `cargo test -p jcode-base --lib provider::` → **2 fail**, both asserting the old rule TASK 1 deliberately changed. TASK 1's file explains what to check before touching them.

## Pre-existing failures, NOT caused by this work

Present on base commit `34635f40f` too. Verify that yourself rather than trusting this file:

```
platform::platform_tests::spawn_detached_creates_new_session
session::tests::cases::streaming_guard_creates_visible_macos_sleep_assertion
```

## Two things the user still has to decide (do not do these for them)

1. **`jcode server reload`** - their daemon is still on the pre-fix binary `34635f40f`, so none of the committed rate-limit work is live for them. Reloading hands over live sessions.
2. **Stopping the runaway session(s)** - at diagnosis time there were ~25 processes and 9 sessions on 2 accounts, producing 450 rate limits/hour with **0 successful calls**.

## What was already shipped and is committed

Commits `9dfe8d2ac` → `82b98f34e`. The **quota** side of the rate-limit problem: exhaustion is now "any window spent" not "every window", the per-account usage cache the rotation decision reads can now be invalidated, cooldowns are sized to the real reset, and a spent quota fails fast with a countdown instead of ~4 minutes of silence. See ADRs `0016`, `0017`, `0018`, plus amendments to `0003` and `0015`, and `docs/DAIN_ACCOUNT_FIRST_FAILOVER.md` Part 3.

## Honest warning about the tests in this area

I wrote **two false-passing tests** during this work, and both were caught by challenges rather than by my own checks:

- one asserted `switches <= 1`, which is satisfied by **0 switches** - and 0 switches is the broken state;
- one seeded its condition through a helper with a side effect that satisfied the assertion, so the behaviour under test was never exercised.

**If a test here passes on the first run, revert the fix and confirm it fails.** Global state (`RATE_LIMITED_ACCOUNTS`, the per-account usage cache) is process-wide and leaks between tests even when `JCODE_HOME` is swapped.
