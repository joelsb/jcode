# ADR-0010: Invalidate the usage cache explicitly, and guard the invalidation with an epoch

Status: Accepted
Date: 2026-08-04 (commit `38d6d80`, corrected after code review)
Plan: `docs/superpowers/plans/2026-08-04-account-rotation-tui-sync.md`

## Context

[ADR-0007](0007-announce-rotation-to-clients-over-the-bus.md) establishes that an account change must invalidate the usage cache, because `UsageData` has no account field and `is_stale()` is purely time-based: after a switch the snapshot is fresh by time and wrong by account.

The first implementation was a single function that spawned a task to clear the freshness stamp:

```rust
pub fn invalidate_anthropic_usage() {
    let Ok(handle) = tokio::runtime::Handle::try_current() else { return };
    handle.spawn(async { get_usage().await.write().await.mark_stale(); });
}
```

Code review found two ways that fails to actually invalidate anything, neither visible from the call site.

**The deferred write loses a race it is on the critical path of.** `client_lifecycle.rs` called this and then immediately sent the wire event to the client. The client's reaction is to ask for usage. That request can arrive, and be served from the daemon's cache, before the spawned task has taken the write lock - so the invalidation is real but too late, and `/usage` returns exactly the stale numbers this work exists to remove. The doc comment on the function asserted the write "happens inline", which was simply untrue and would have stopped a reader from looking for this.

**A refresh already in flight silently undoes the invalidation.** `try_spawn_refresh` guards with a bare `REFRESH_IN_FLIGHT: AtomicBool`. Sequence, all of it plausible because the widget polls usage and rotation happens on a 429:

1. A refresh starts under `claude-1`; `REFRESH_IN_FLIGHT` is `true`.
2. Rotation happens. `mark_stale()` clears `fetched_at`.
3. The in-flight fetch completes and `refresh_usage` writes a whole new `UsageData` - `claude-1`'s numbers, stamped `fetched_at: Some(now)`.
4. `REFRESH_IN_FLIGHT` goes back to `false`.

The cache now holds the previous account's numbers *and* looks fresh, so `get_sync()` sees nothing to do and no refresh runs for a full `CACHE_DURATION`. This is worse than the bug we started with, because the earlier version at least self-repaired on the next TTL expiry.

## Decision

Two forms of invalidation, and a monotonic epoch that makes both correct.

1. `invalidate_anthropic_usage_now()` is `async` and awaits the write. Callers that can await use it. The daemon forwarding path must, because it sends the wire event immediately afterwards.
2. `invalidate_anthropic_usage()` stays sync for UI code with nowhere to await, and its doc comment now says the write is deferred and what that costs.
3. Both bump `USAGE_EPOCH: AtomicU64`. `try_spawn_refresh` captures the epoch before spawning; when the fetch completes under a different epoch, it re-marks the cache stale instead of letting the landed result stand as fresh.

The epoch is the load-bearing part. Point 1 alone would still lose to an in-flight fetch, and the fetch is not cancellable - by the time we know the account changed, the request is already on the wire.

## Consequences

- After any account change the next usage read refetches, and cannot be poisoned by a fetch that started under the previous account.
- `USAGE_EPOCH` looks like dead weight to anyone reading `try_spawn_refresh` in isolation: nothing in that function explains why a completed refresh would need to be thrown away. It is the whole reason the function is correct. Do not remove it without reproducing the four-step sequence above.
- The sync form still has a one-read race window by construction. It is kept because UI code genuinely cannot await, and the epoch bounds the damage to a single stale read rather than a `CACHE_DURATION` of them. If a caller cares about that read, it belongs on the async form.
- Two entry points for one concept invites picking the wrong one. The async form is the default; the sync form's doc comment says so.
- This applies to the pre-existing manual `/account switch` as much as to rotation. Neither had correct invalidation before.

## Evidence

- `crates/jcode-base/src/usage/accessors.rs` - both forms, the epoch, and the re-mark in `try_spawn_refresh`.
- `crates/jcode-base/src/usage/model.rs` - `mark_stale`, and `is_stale()` returning `true` for `fetched_at: None`.
- `mark_stale_forces_the_next_read_to_refetch` covers the stamp; the in-flight clobber is argued from the code above rather than from a test, because deterministically interleaving a spawned fetch with an invalidation needs a seam `refresh_usage` does not currently have. Worth adding if this area is touched again.
