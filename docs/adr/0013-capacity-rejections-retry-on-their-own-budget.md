# ADR-0013: Anthropic capacity rejections retry on their own budget and clock

Status: Accepted
Date: 2026-08-05

## Context

Anthropic answers "no capacity right now" as an SSE error event with `"type":"overloaded_error"`, or as HTTP 529 on the initial response.
`is_retryable_error` already matched `overloaded`, so an overload was retryable.
It was nonetheless surfacing to the user as a hard failure:

```
× Retryable stream error: {"type":"error","error":{"details":null,"type":"overloaded_error","message":"Overloaded"},"request_id":"req_011CDjHKfn5sRZakAvDW2xJ6"}
↳ Claude failed. Fallback available: press Ctrl+Y to switch to claude-fable-5 via oauth (Anthropic) and resend.
```

Two things combined to produce that.

**The budget.** Overloads shared `transport_retries`, guarded by `transport_retries + 1 < MAX_RETRIES` with `MAX_RETRIES == 2`.
That allows exactly one retry. One overload consumed it, the second failed the turn.

**The clock.** `RETRY_BASE_DELAY_MS` is 200 for this provider, tuned for transport faults where an immediate reconnect on a fresh client usually works.
A capacity rejection is not a broken connection: the request was well-formed and the fleet was momentarily full.
Retrying ~200ms later asks the same saturated fleet the same question, so the one available retry was also the one least likely to succeed.

The user-visible result was the worst possible reaction to a transient condition: a raw provider error, plus an offer to downgrade to a *smaller model* to escape a problem that had nothing to do with the model.

## Decision

Classify capacity separately from transport, and give it its own budget and backoff.

- `is_overloaded_error` matches `overloaded_error` and HTTP 529. It is deliberately narrower than `is_retryable_error`, which keeps matching both so an overload never becomes non-retryable.
- Capacity retries count in `overloaded_retries`, bounded by `MAX_OVERLOADED_RETRIES` (4), independent of `transport_retries`. An overload burst and a flaky network are independent faults and must not consume each other's budget.
- Capacity backoff uses `OVERLOADED_RETRY_BASE_DELAY_MS` (2000), giving roughly 2s, 4s, 8s, which is the timescale on which Anthropic capacity actually frees up. A server `Retry-After` still wins when present.
- Like a rotation (ADR-0004), each capacity retry increments `attempt_budget`, so waiting out an overload does not spend an attempt the transport path may still need.
- The user sees `⏳ Anthropic is overloaded; retrying (n/4)` instead of a raw JSON error.

Ordering in the loop matters: the capacity branch sits after account rotation and before the generic transient branch.
An overload is a server-side capacity problem, not an account quota problem, so `should_rotate_account` must not fire on it - rotating to another account reaches the same saturated fleet while burning a rotation.

### The client-side budget, too

Fixing only the provider was not enough, and the reason is worth recording.
With the provider fix live, one session recovered (`⏳ Anthropic is overloaded; retrying (2/4)`) while another still failed.
The second session had exhausted a *different* budget: once the provider gives up, the TUI decides whether to resend the turn at all, and that path gave an overload the generic transport treatment - `AUTO_RETRY_MAX_ATTEMPTS` of 3 at 2/4/6s, reported as "Connection lost", then "Auto-retry limit reached after 3 attempts" and the model-downgrade offer.

So the client became the binding constraint, and the same reasoning applies at both layers:

- `App::error_is_provider_overload` classifies the rendered wire text, mirroring the provider-side `is_overloaded_error`.
- Capacity retries use `OVERLOAD_RETRY_MAX_ATTEMPTS` (10) at `OVERLOAD_RETRY_BASE_DELAY_SECS` (5s), growing linearly and capped by `RETRY_MAX_BACKOFF_SECS` (30s).
- The two layers multiply, and that is what sizes the client ceiling. Each client attempt drives a fresh provider request that itself retries `MAX_OVERLOADED_RETRIES` times, so 10 client attempts is ~40 real requests over ~4.5 minutes. An earlier draft used 50 attempts (~22 min); it was cut once the multiplication was made explicit, because the extra ceiling only multiplied redundant requests against an already-saturated fleet without buying meaningful patience.
- The cap exists because unbounded linear growth reaches multi-minute waits that are indistinguishable from a hang. A steady 30s cadence stays legible.
- The notice reads `⏳ Provider overloaded - retrying (n/10, in Ns)` and deliberately stops there. Calling a capacity rejection "Connection lost" told the user to check their network for a server-side problem, and appending the raw error JSON (as the transport notice does) buried the only actionable part - which attempt, and how long - behind a wrapped `request_id`. The error body is already rendered immediately above the notice.


## Consequences

- A transient overload now costs seconds of waiting instead of a failed turn. Worst case an unrecoverable overload takes ~14s longer to report, which is the right trade: the failure message is only useful once retrying is genuinely hopeless.
- `MAX_RETRIES` keeps meaning "attempts at the same request" and is now independent of all three causes: rotation, transport, capacity. Three mutable contributors to one budget is close to the limit of what stays legible; a fourth should prompt extracting a budget type rather than adding another counter.
- The 529 match is anchored on `(529` and `529 ` rather than the bare substring. `529` occurs inside request ids and token counts, and an unanchored match would send unrelated 4xx failures into a 14-second backoff before reporting them. Covered by `a_request_id_containing_529_is_not_mistaken_for_a_capacity_error`.

## Alternatives rejected

- **Raise `MAX_RETRIES`.** Would slow every transport fault by the same factor to fix a capacity problem, and still retry on the wrong clock.
- **Rotate accounts on overload.** Capacity is fleet-wide, not per-account. It burns a rotation and reaches the same saturated backend.
- **Offer the model fallback sooner.** The existing behavior, and the reason the bug was user-visible. Downgrading the model does not fix a capacity rejection, and silently answers a later question on a weaker model than the user chose.

## Revisit if

Anthropic starts returning a reliable `Retry-After` on 529, in which case the fixed backoff schedule becomes dead weight and the hint should drive the wait entirely.
Also revisit if telemetry shows overloads routinely surviving four retries, which would mean the ceiling is wrong rather than the clock.

## Evidence

- Reported failure: `overloaded_error` with `request_id` `req_011CDjHKfn5sRZakAvDW2xJ6`, surfaced with the Ctrl+Y model-downgrade offer.
- `crates/jcode-provider-anthropic-runtime/src/lib.rs`: `is_overloaded_error`, `MAX_OVERLOADED_RETRIES`, `OVERLOADED_RETRY_BASE_DELAY_MS`, and the capacity branch in `run_stream_with_retries`. Client side: `App::error_is_provider_overload`, `schedule_pending_remote_overload_retry`, `OVERLOAD_RETRY_MAX_ATTEMPTS`, `RETRY_MAX_BACKOFF_SECS`.
- Tests: `overloaded_errors_are_classified_as_capacity_not_just_transient`, `a_request_id_containing_529_is_not_mistaken_for_a_capacity_error`, `test_overload_retries_on_its_own_generous_budget`, `test_overload_backoff_is_capped_so_waits_stay_legible`.
- The two-layer nature of the bug was only visible live: with the provider fix deployed, one session recovered while another failed on the client budget. A single-layer fix looked correct in tests and in one session, which is the general lesson - a retry policy is only as patient as its least patient layer.
