# ADR-0020: Serialise account rotation, and let followers adopt the leader's account

Status: Accepted
Date: 2026-08-14

Closes the gap [ADR-0018](0018-claim-the-shared-account-override-only-when-better-informed.md) left open. Builds on [ADR-0009](0009-defer-per-session-account-identity.md) (process-global override) and [ADR-0015](0015-rate-limits-wait-on-their-own-clock.md) (wait out a recoverable limit).

## Context

ADR-0018 narrowed *who may announce* a rotation. It stated plainly that it did not resolve the underlying concurrency, and it was right: the same shape came back on 2026-08-14 with five sessions open, on a binary that already carried the ADR-0018 fix and the single-session flapping fix.

The user reported a wall of switch lines and diagnosed the cause themselves:

> they all run out of tokens at the same time, and so they request and change and then continue... only one can do this and it must be locked, and the others should wait until the lock is done

The logs confirm it. Correlating each failover to the session that logged it:

```
23:54:50  -> claude-1   session_unicorn
23:54:59  -> claude-2   session_unicorn
23:55:06  -> claude-1   session_panda
23:55:11  -> claude-2   session_swan
23:55:15  -> claude-1   session_spider
```

Four distinct sessions, 25 seconds, alternating accounts.

This is a different defect from the single-session cross-turn flapping, and the two are distinguishable in the transcript: that one always pairs a switch with a **"restored"** line, and there are none here. Do not treat "switch spam" as one bug.

The mechanism is check-then-act. The cooldown that would tell session B "claude-2 is limited, do not go there" is only recorded when B's own 429 comes *back*. All five sessions read the candidate list, pick the same account, and commit before any response lands. ADR-0018's `should_claim_shared_override` gates the *write* to the shared pointer, not the *decision*, so every session still rotates and every session still spends a request.

## Decision

Two changes in `switch_to_next_claude_account`, both required:

1. **Serialise rotation** on a process-wide `tokio::sync::Mutex`. One daemon serves all sessions (ADR-0009), so this is an in-process lock, not a file lock. It is a `tokio` mutex because it is held across the credential load, which awaits.

2. **Re-read the shared state after acquiring the lock.** If a sibling already rotated to a usable, non-cooling account this request has not tried, adopt it and return `claimed_shared_override: false`. Do not rotate again.

Part 2 is not an optimisation. With the lock alone, the leader takes the only other account and each follower then finds every candidate either active or already tried, reports a dead end, and drops a serviceable turn into the ADR-0015 wait. Measured: **4 of 5 sessions dead-ended**.

## Consequences

- N concurrent sessions reacting to one shared limit perform one pool-wide move and emit one switch line, instead of N of each.
- Rotation is now a serialisation point. The critical section is one credential read on a path that only runs after a 429, so the cost is negligible against the network round trip it replaces. If rotation ever grows a blocking probe, this lock becomes a bottleneck and must be revisited.
- Followers report `claimed_shared_override: false`, so the ADR-0018 announcement rule keeps them silent. Correct: the leader already announced the move.
- A follower adopts an account it has not personally validated. Acceptable: the leader just loaded its credentials, and if it turns out to be limited the normal 429 path handles it.

## Alternatives rejected

- **Lock without the follower path.** Fixes the announcement spam but converts four rotations into four dead ends. Strictly worse for the user: silence instead of noise, but the turns still stall.
- **Per-session account identity** (the real fix ADR-0009 deferred). Correct, and still deferred. It touches credential storage, usage attribution, and the client protocol. This ADR is the contained fix for the symptom that is hurting now.
- **Backoff/jitter before rotating.** Reduces collision probability without eliminating it, and adds latency to every rotation including uncontended ones.

## Revisit when

- Per-session account identity ships, at which point this lock and ADR-0018's claim rule both become unnecessary.
- Rotation gains a blocking network call, which would make holding the lock across it unacceptable.
- Sessions on one daemon routinely exceed the account pool by a large factor, where a single leader may no longer be enough and a queue with per-account assignment would be better.

## Evidence

- `concurrent_sessions_rotate_the_shared_pool_only_once` (`crates/jcode-provider-anthropic-runtime/src/anthropic_tests.rs`): five concurrent sessions, two accounts.
- Negative controls, both run:
  - remove the lock, keep the re-check: `saw 5 of 5` claims (the reported bug).
  - keep the lock, remove the re-check: `saw 4 of 5` dead ends.
- `~/.jcode/logs/jcode-2026-08-14.log`: 111 failovers that day; the 23:54-23:55 burst correlated to four distinct sessions above.
