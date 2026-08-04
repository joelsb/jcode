# ADR-0005: Rotate after partial output, rolling the partial attempt back

Status: Accepted
Date: 2026-08-04 (commit `8b771e6`)

## Context

The first implementation gated rotation on `!saw_output`: rotate only if nothing had streamed yet, on the reasoning that replaying a request whose output the consumer already saw would duplicate it.

Review pointed out where that leaves the request.
A usage limit arriving *after* partial output skipped rotation and fell through to the generic transient-retry branch, which retries the **same exhausted account** and honors its `retry-after` - a 60s stall followed by the same failure.
That is the original bug of [ADR-0002](0002-rotate-accounts-inside-the-streaming-retry-loop.md) in a narrower form, so the fix was incomplete for that case.

The loop already had a mechanism for this exact situation. When a transport fault hits mid-stream, the transient path emits `StreamEvent::RetryRollback { attempt, max }` to tell the consumer to discard the partial attempt before the retry replays the response.

## Decision

Allow rotation regardless of `saw_output`, and when output had already streamed, emit `StreamEvent::RetryRollback` before replaying on the new account - the same contract the transient-retry path already relies on.

`should_rotate_account` consequently drops its `saw_output` parameter and gates only on "rotation enabled" plus "this looks like a usage limit".

## Consequences

- A mid-stream rate limit now reaches another account instead of stalling on the exhausted one.
- Correctness of the no-duplication guarantee is delegated to `RetryRollback`, so this decision inherits whatever that mechanism's limits are. It is not a new risk surface: mid-stream transport faults have always replayed this way.
- Mid-stream 429s are rare compared to pre-stream ones, so the practical blast radius is small either way. The deciding argument was consistency: two paths handling "replay after partial output" differently is a bug waiting to be rediscovered.
