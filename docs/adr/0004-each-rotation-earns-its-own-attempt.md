# ADR-0004: Each rotation grants itself an attempt; transport retries are counted separately

Status: Accepted
Date: 2026-08-04 (commits `8b771e6`, `5ed6bf9`)

## Context

`run_stream_with_retries` has one loop serving five different retry causes: forced OAuth token refresh, model-not-found fallback, reasoning-unsupported strip, account rotation, and generic transient transport faults.
`MAX_RETRIES` is 2.

The first rotation implementation pre-sized the loop as `MAX_RETRIES + alternate_account_count`.
Review showed the arithmetic does not hold: the other one-shot causes consume attempts from the same pool, so a request that first refreshed a token, then fell back a model, then stripped reasoning could reach a rate limit with no attempts left to rotate - despite the budget having been sized "for rotation".

The reverse problem also existed. The transient-retry guard was `attempt + 1 < MAX_RETRIES`, so once rotation had consumed attempts, a genuine network fault on the new account was no longer retryable.

## Decision

Stop trying to pre-compute a budget. Instead:

- Start the budget at `MAX_RETRIES` and let each *successful* rotation increment it by one. A rotation therefore cannot be starved by earlier causes, and cannot starve them either.
- Count transport retries in their own counter, `transport_retries`, so `MAX_RETRIES` keeps its original meaning of "attempts at the same request" independent of how many times the account changed.
- Convert the `for attempt in 0..max_attempts` loop to `while attempt < attempt_budget` with the attempt consumed at the top of the body, since several branches reach `continue`.

Termination is bounded by `tried_accounts`: `next_untried_account` only returns labels not already attempted, so rotations are capped by the number of configured accounts even though the budget is mutable.

## Consequences

- Compound failures degrade sensibly: a token refresh plus a model fallback no longer eat the rotation.
- The budget is now data-dependent rather than constant, so the `Retrying {attempt, max}` event and the "Failed after N retries" message report a moving `max`. Accepted: the alternative was a fixed number that lies about how many attempts the request can actually make.
- **Regression this caused, and its fix.** Pre-incrementing `attempt` at the top of the body made `attempt == 0` never true, so the guard selecting the pooled HTTP client on the first attempt silently stopped matching and every request built a fresh unpooled transport. Found by the E2E test author, fixed in `5ed6bf9` by comparing against `attempt_index` (the zero-based value the guard was written for). Recorded here because the lesson generalizes: renaming a loop variable's meaning obliges you to re-read every comparison against it, not only the ones in the diff.
