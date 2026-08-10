# ADR-0015: A rate limit that outlives account rotation waits on its own clock

Status: Accepted
Date: 2026-08-10

## Context

A user's swarm run died mid-task with a wall of 429s:

```
⚡ Rate limited; switched Anthropic account: claude-1 → claude-2
⚡ Every Anthropic account is rate limited; restored claude-1 (was trying claude-2)
× Anthropic API error (429 Too Many Requests): {"type":"rate_limit_error", ...}
↳ Claude failed. Fallback available: press Ctrl+Y to switch to claude-fable-5 ...
```

The account rotation from ADR-0001/0002 worked: it tried the other account, got 429 there too, and correctly reported that every account was limited. What happened *after* that was wrong.

The user's own usage dashboards at that moment:

| Account | 5-hour window | Weekly |
|---|---|---|
| joeld | 90% left | 17% left |
| elara | 0% left (resets 22:00) | 83% left |

So one account had 90% of its 5-hour window and the other had 83% of its week. "Every Anthropic account is rate limited" was true only in the sense that *right now* both requests bounced, which is Anthropic's short-window burst limit, a third limit that sits on top of the 5-hour and weekly quotas.

A burst limit clears in seconds. But once rotation reported exhaustion, the 429 fell through to the generic transport branch:

```rust
if is_retryable_error(&error_str) && transport_retries + 1 < MAX_RETRIES {
```

with `MAX_RETRIES = 2` and `RETRY_BASE_DELAY_MS = 200`. That is one retry, roughly 200ms later. A burst window cannot be outlasted in 200ms, so the turn failed and offered to downgrade the *model*, which does nothing about a per-account request limit and costs the user quality on work they had quota to run.

This is the same class of mistake as ADR-0013 (capacity rejections sharing the transport budget), on a different error type. It survived that ADR because the fix there matched `overloaded_error`/529 specifically, and a 429 is neither.

## Decision

Give a rate limit that account rotation could not escape its own retry budget and its own clock, exactly as ADR-0013 did for capacity:

- `MAX_RATE_LIMIT_RETRIES = 5`, counted in `rate_limit_retries`, independent of both `transport_retries` and `overloaded_retries`.
- `RATE_LIMIT_RETRY_BASE_DELAY_MS = 5000`, giving roughly 5s, 10s, 20s, 40s of jittered backoff, so the retries span ~75s rather than ~200ms.
- A server `retry-after` always wins over that backoff.
- Each wait grants its own attempt (`attempt_budget += 1`), so waiting out a shared limit cannot consume the retries reserved for network faults.
- The branch is placed after the rotation branch, so rotating to a free account is still strictly preferred over waiting.

Because every branch that grows `attempt_budget` is bounded by its own counter, the loop still terminates: the worst case for an always-429 world is `MAX_RETRIES + 1 rotation + (MAX_RATE_LIMIT_RETRIES - 1)` = 7 requests, then a terminal error.

## Consequences

A burst limit that clears within ~75s now finishes the user's turn instead of failing it. That is the common case when several agents share two accounts, which is exactly the swarm workload that triggered this.

The costs:

- A genuinely exhausted quota now fails ~75s slower than before, spent in visible `⏳ Every account rate limited; waiting Ns and retrying (n/5)` status lines rather than silently. Slower honest failure is the price of not abandoning recoverable work, and the status line keeps it from looking like a hang.
- Retrying into a limit consumes rate-limit headroom, which can marginally delay the window's recovery. Bounded by the 5-attempt cap and the seconds-scale backoff.
- Three independent retry budgets (transport, capacity, rate limit) is more state to keep straight. That is deliberate: they are three different faults with three different clocks, and sharing them is precisely what caused this bug and ADR-0013's.

## Rejected alternatives

**Reset the rotation state and cycle accounts again on each wait.** Tried first, and it was wrong: the reset multiplied with the retry loop, taking an always-429 test from 3 requests to 11, i.e. a retry storm against an already-limited account. Reverted. Rotation is for finding a *different* account; waiting is for the same one becoming free.

**Raise `MAX_RETRIES` / `RETRY_BASE_DELAY_MS` globally.** Would fix the 429 by making every transport fault slow. A dropped connection should be retried in milliseconds, not seconds.

**Treat 429 as `overloaded` and reuse the capacity budget.** Closer, but it conflates two provider states with different meanings and different recovery times, and would make ADR-0013's ~2-8s clock the ceiling for burst limits.

**Just tell the user to wait.** The status quo, in effect. It surfaces a raw provider error and a misleading "switch model" offer during work jcode has the quota to complete.

## Revisit when

- Anthropic exposes a burst-window reset in the 429 (a header or body field). Then wait exactly that long instead of backing off blindly.
- The usage probe can distinguish "burst limited" from "quota exhausted" before the request. Then skip the waits entirely when the quota really is spent, removing the slower-honest-failure cost.
- Someone reports a hang around ~75s on a rate limit. That means the status line is not reading as progress and needs to become a countdown.

## Evidence

`a_burst_limit_that_clears_lets_the_turn_finish` drives the real `run_stream_with_retries` against a mock Anthropic server that answers 429 for the first four calls and then succeeds, i.e. a burst that clears while jcode waits. It asserts the stream carries no error and the recovered text reaches the consumer.

Negative control: disabling only the new branch (`if false && ...`) makes that test fail with the user's exact error, then pass again when restored:

```
the turn must survive a burst limit that clears; got errors:
["Anthropic API error (429 Too Many Requests): {\"type\":\"error\",
  \"error\":{\"type\":\"rate_limit_error\", ...}}"]
```

`rate_limits_wait_on_a_rate_limit_clock_not_the_transport_clock` pins the policy: the real 429 payload classifies as a usage limit and not as capacity, the budget exceeds the transport budget, and the retries span >=30s, more than 10x the transport path. Its own negative control (restoring `2` / `200ms`) fails it.

`run_stream_with_retries_bounds_rotation_attempts_and_restores_original_account` was updated rather than deleted: it encoded the old bound of 3 requests and now asserts the new one, plus an explicit `< 20` sanity bound so the termination property is checked independently of the exact arithmetic.

### A defect this work introduced and then fixed

Adding seconds-scale backoff made the crate's own suite really sleep, taking it from ~5s to **259s**. Tests now clamp any retry sleep to 5ms under `cfg(test)` (`clamp_retry_delay`), keeping the branch and counters under test while removing the wall-clock cost; the suite runs in ~2s. Worth recording because the natural fix, shortening the production delays to keep tests fast, would have silently undone the entire point of this ADR.
