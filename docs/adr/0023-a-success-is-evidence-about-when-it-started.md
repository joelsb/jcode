# ADR-0023: A success is evidence about when it started, not about now

- Status: Accepted
- Date: 2026-08-21
- Supersedes nothing. Fixes the residual flapping left by [ADR-0018](0018-claim-the-shared-account-override-only-when-better-informed.md) and [ADR-0020](0020-serialise-account-rotation.md).

## Decision

Two changes to Anthropic account rotation:

1. A successful request clears an account's rate-limit cooldown **only when the cooldown was recorded before that request started** (`clear_account_rate_limit_if_newer`). A cooldown noted while the request was in flight is newer evidence and survives.
2. When every account in the pool is cooling, the rate-limit wait sleeps until the **soonest cooldown actually expires** (`time_until_any_account_is_eligible`), plus a per-request spread, instead of a fixed backoff ladder.

## What forced it

The `claude-1 → claude-2 → claude-1 → claude-2` switch ladder kept happening on a binary carrying every earlier fix in this series, including the rotation mutex from ADR-0020.

ADR-0020 serialised the *decision*. It did not protect the *state the decision reads*. From the 2026-08-21 log, with 6 sessions live on one daemon:

```
10:34:15.245  Same-provider account failover: ... on account 'claude-2'
10:34:15.253  Adopting the account a concurrent session already rotated to: 'claude-2'
10:37:17.046  Same-provider account failover: ... on account 'claude-1'   <- back again
10:38:06.060  Same-provider account failover: ... on account 'claude-2'
10:40:24.999  Same-provider account failover: ... on account 'claude-1'   <- and back
```

The mutex worked: at 10:34:15 five sessions produced one move plus one adoption, exactly as ADR-0020 intends. Three minutes later the pool moved back onto `claude-1`, which had refused a request 3 minutes earlier and had a cooldown recorded for it.

The cooldown was gone, and a success deleted it. The success path ran:

```rust
Ok(()) => {
    jcode_base::provider::clear_account_rate_limit(ActiveProvider::Claude, &label);
```

unconditionally, and `clear_account_rate_limit` removes both the cooldown *and* the cached per-account usage verdict, i.e. precisely the two pieces of state ADR-0016 and ADR-0017 exist to keep correct.

The ordering that breaks it is the daemon's normal state, not an edge case:

1. T0: a request is accepted on `claude-1` and starts streaming. A turn with tool calls runs for minutes.
2. T1: the pool tips over. Sibling sessions get 429 on `claude-1`, record the cooldown, and correctly rotate to `claude-2`.
3. T2: the T0 stream finishes successfully and erases the T1 cooldown.
4. The next request sees a clean `claude-1`, rotates onto it, gets 429, records the cooldown - and the next long success wipes it again.

The success was true. It was true *about T0*. Treating it as a statement about T2 is the whole bug, and no amount of locking step 3 helps, because the writer is not racing the decision - it is destroying the decision's inputs from a request that learned nothing about the present.

The second change addresses what happens once the first one holds. With every account genuinely cooling, the old wait made 5 attempts ~15s apart. Each attempt is a real HTTP request that earns a real 429 and, through `note_account_rate_limited_for`, **re-extends the cooldown it just re-proved**. Per session. The 10:38 window shows exactly this: `attempt 1/5 ... 2/5 ... 3/5 ... 4/5`, 15s apart, interleaved across sessions, against a pool whose earliest reset the process had already computed and printed (`earliest reset 1h 22m (claude-1)`).

## Consequences

- Cooldowns now survive concurrent success traffic, so the candidate list stays truthful and the pool stops oscillating.
- `RateLimitNote` carries `noted_at` alongside `until`. Both are needed: `until` is when the account recovers, `noted_at` is how a concurrent success knows whether it is entitled to speak.
- An account that recovers early stays excluded until its cooldown expires or a request that *started after* the refusal succeeds. That is the intended trade: the previous behaviour recovered faster and was wrong most of the time.
- The pool wait is capped at 90s (`MAX_POOL_RECOVERY_WAIT`). Beyond that the loop stops waiting so the user is told about the reset and can downgrade or switch provider, rather than sitting through a quota window in silence. This keeps ADR-0015's rule that a recoverable limit must not become a hard failure, while refusing to turn an unrecoverable one into a silent stall.
- The spread (`pool_recovery_spread`, up to 2s) is load-bearing, not cosmetic. Every session derives the *same* deadline from the *same* shared cooldown, so without it they wake in the same millisecond and fire a synchronised burst at an account that can serve maybe one of them: a thundering herd that re-limits the pool instantly. The salt is per-request and allocated once, so a request keeps its slot across its own retries.

## Rejected alternatives

- **Clear the cooldown only when nothing else is in flight.** Requires a request registry and still gets it wrong when the in-flight set is empty at exactly the wrong moment. The timestamp comparison is the actual question being asked, so ask it directly.
- **Never clear on success; let cooldowns expire.** Simpler, but an account that recovers early then stays excluded for the full window, and a 6-hour clamp (ADR-0016) makes that expensive. The guarded clear keeps early recovery for the case where it is sound.
- **Make cooldown clearing idempotent per account epoch (a counter bumped on each 429).** Equivalent power to the timestamp, more machinery, and the counter cannot answer "was this refusal newer than my request" without also storing a time.
- **Per-session account identity** ([ADR-0009](0009-defer-per-session-account-identity.md)) would remove the shared state that makes all of this necessary. Still the right long-term fix, still an architectural change well beyond this defect. This ADR makes the shared state honest; it does not un-share it.

## What would make us revisit

- Per-session account attribution landing, which removes the shared cooldown map's role in cross-session decisions.
- Anthropic exposing a reliable "account is limited until X" signal on the 429 itself, which would make the local cooldown a cache of an authoritative value rather than an inference.
- Evidence of the opposite failure: accounts staying excluded after they demonstrably recovered. The cap and the guarded clear are the two knobs.

## Evidence

- `crates/jcode-provider-anthropic-runtime/src/anthropic_tests.rs`:
  - `a_success_does_not_erase_a_newer_rate_limit_from_a_concurrent_session` - pins both halves of the rule (a stale success does not clear, a newer one does).
  - `the_pool_wait_is_sized_to_the_cooldown_not_a_blind_backoff` - pins soonest-first and the "rotate, do not wait, while a healthy account exists" negative control.
  - `concurrent_waiters_do_not_wake_in_lockstep` - pins the spread.
- Three planted defects, each caught by the tests above and by no other test in the suite:
  1. success clears unconditionally (the original bug) -> `a success that began before the refusal must not report clearing it`.
  2. soonest deadline replaced by latest -> `got 10799.9s` where ~60s is correct.
  3. non-cooling accounts skipped instead of aborting the wait -> `one limited account does not make the pool unavailable`.
- Field data: `~/.jcode/logs/jcode-2026-08-21.log`, 10:34-10:47, 6 concurrent sessions, 2 accounts, 319 lines mentioning 429 and switch lines with no accompanying "restored" line - the signature that distinguishes this from the single-session flapping fixed earlier.
