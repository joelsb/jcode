# ADR-0017: The cache that decides rotation is the cache that must be invalidated

Status: Accepted
Date: 2026-08-12

Companion to [ADR-0016](0016-exhaustion-is-any-window-not-every-window.md). Extends [ADR-0010](0010-invalidate-the-usage-cache-with-an-epoch-guard.md), whose guard protects a different cache from the one the rotation decision reads.

## Context

While investigating the account flapping in ADR-0016, the user asked the question that turned out to be the second half of the cause:

> Maybe jcode is reading the cache, so the last cache that was set for that account. What we could do is, instead of looking into the cache to understand if that account is actually run over or not, it should fetch the API and then save it into the cache when rotating accounts.

Correct, and worse than a staleness window. There were **two** Anthropic usage caches, and rotation invalidated only the one it does not read:

| Cache | Keyed by | Read by | Invalidated on rotation? |
|---|---|---|---|
| `USAGE` (single slot) | nothing, account-blind | `get_sync()`, the usage widget | **Yes**, `invalidate_anthropic_usage_now()` with ADR-0010's epoch guard |
| `ANTHROPIC_USAGE_CACHE` (map) | `"label:claude-2"` | **`account_usage_probe_sync()`, i.e. the rotation decision** | **Never** |

Nothing in the tree called `.remove()` or `.clear()` on that map. Its only expiry was the 300s TTL, stretched to **900s** by `RATE_LIMIT_BACKOFF` whenever the cached entry carried a rate-limit error. So a "has headroom" verdict could outlive the 429 that disproved it by up to fifteen minutes, and every rotation decision in every session on the daemon was made from it. All of ADR-0010's careful epoch machinery guards the cache that only draws a widget.

Three further defects sat in the same area:

1. **The two caches used different key shapes for the same account.** The probe path keyed by *token prefix* (`anthropic_usage_cache_key(token, None)`), the `/usage` path by *label*. Two entries per account, neither able to invalidate the other, and a token refresh silently orphaned the token-keyed one.

2. **For the active account, the probe bypassed the map entirely** and used the account-blind single-slot snapshot. So the account most likely to have just been refused was the one whose refusal was least visible.

3. **Rotation never refreshed anything.** `switch_to_next_claude_account` swapped credentials and returned; nothing re-probed the account it was moving onto.

Together with ADR-0016's predicate bug this is why the user's `claude-2` kept being selected: the verdict was wrong, and being refused by that account could not correct it.

## Decision

**Invalidate and correct the cache the decision reads.**

- `invalidate_anthropic_account_usage(label)` removes an account's entry. Called on every 429 and available to account switches.
- `note_anthropic_account_rate_limited(label, resets_at)` records what a 429 teaches, asymmetrically:
  - **reset known**: write a spent 5-hour window carrying that reset. `exhausted` is the field every consumer reads, so an entry that merely carried an error string would still report headroom. `is_stale` treats a passed reset as stale, so the entry expires exactly when the account recovers.
  - **reset unknown**: only *forget* the stale verdict, never assert exhaustion. An unexplained 429 may be a burst clearing in seconds, and an entry whose `last_error` mentions a rate limit attracts the 900s backoff TTL, which would sideline a recovered account for fifteen minutes. The short rotation cooldown covers "do not immediately retry".
- `note_account_rate_limited_with_reset` in the provider layer updates the cooldown *and* the cache in one call, because the call site previously updated one of three pieces of state and the omission was invisible.
- **One key shape:** the probe path now keys by label too, and reads the per-account map for *every* account including the active one.
- **Rotation refreshes, in the background.** After a rotation lands, `spawn_account_usage_refresh` re-probes the new account so the *next* decision has fresh data.

**Deliberately not a blocking fetch before choosing an account**, which is the one place this departs from the user's proposal. A blocking probe would add a network round trip in front of every rotation, on the exact path the user already experiences as a stall, and the usage endpoint can itself be rate-limited or down. The 429 is the fast, free, authoritative signal; the fetch merely confirms it. Correspondingly, the failure path reads only cached values (`cached_anthropic_account_usage_any_age`, `earliest_account_reset`) and never fetches.

A failed refresh is *not* recorded as exhaustion. An unreachable probe means "unknown", and treating it as a limit would remove a healthy account from rotation because its usage endpoint blinked, the same vacuous-truth trap ADR-0015 fixed in `all_accounts_exhausted`.

## Consequences

A 429 now corrects the verdict for every session on the daemon immediately, and a rotation leaves fresh data behind. The user's observation that restarting jcode "would have fixed it" is explained: both caches are in-process, so a restart emptied them. That was evidence of the cache being the fault, not a reason to restart.

The costs:

- **Rotation now issues a background HTTP request.** Bounded by one per successful rotation and off the critical path, but it is new traffic to a rate-limitable endpoint.
- **A 429 with a known reset now writes a synthetic snapshot**, not a measured one. It claims exactly one thing (a spent 5-hour window) which may attribute the block to the wrong window when the weekly quota is the real cause. The reset time still comes from the provider, and the background refresh replaces the synthetic entry with real data shortly after.
- **Two caches still exist.** This ADR makes the decision-relevant one correct; it does not merge them. The single-slot account-blind cache remains the defect ADR-0009 deferred, and a future per-session account model should absorb it.

## Rejected alternatives

**Fetch synchronously before choosing an account.** The user's first instinct, and the natural reading of "fetch instead of trusting the cache". Rejected because it puts a network round trip in front of the rotation that a rate-limited user is already waiting on, and because the endpoint can be limited or unreachable exactly when it is needed. Post-rotation refresh gets the same freshness for the next decision at no latency cost.

**Shorten the 300s/900s TTLs.** Would reduce the window without fixing anything: the problem is that a 429 could not invalidate a contradicted entry at all, and a shorter TTL still serves a wrong verdict for its duration while adding probe traffic.

**Have the 429 write a "limited" flag beside the cache.** That is what the rotation cooldown already is. Splitting the truth across two structures is how the original bug survived: the runtime updated the cooldown and left the cache contradicting it.

**Restart the daemon or the session on rotation.** Suggested during the investigation, and it would have worked by accident, since both caches are in-process. Rejected as a fix: it discards every live session's state to clear a cache entry that can simply be removed.

## Revisit when

- Per-session account identity lands (ADR-0009). The two-cache split and its account-blind slot should disappear into that work.
- The background refresh shows up as meaningful load, or gets itself rate-limited often enough to matter. Then it should be coalesced or debounced per account.
- A synthetic 429 entry is ever seen misattributing a weekly block to the 5-hour window in a way that changes a decision. `blocking_reset_at` would need the window identity carried through the 429 path.

## Evidence

`a_429_corrects_the_per_account_usage_cache` plants exactly the wrong verdict the incident was decided on (a fresh "10% used" snapshot for a label), fires a 429 with a 3-hour reset, and asserts the cache then reports the account as limited; then asserts an explicit invalidation removes the entry entirely. Negative control: dropping the `note_anthropic_account_rate_limited` call fails the middle assertion.

`a_429_without_a_reset_only_forgets_the_stale_verdict` pins the asymmetry, which is the part most likely to be "simplified" later: an unexplained 429 must clear the entry rather than claim a spent quota, because claiming one attracts the 900s TTL.

`a_rotation_refreshes_the_new_accounts_usage_in_the_background` guards the refresh, and specifically guards the `cfg(test)` switch that disables it: a flag that silently turned off a real feature would be worse than the flakiness it fixes, so one test turns it on and observes the seeded verdict being replaced. It accepts either "entry gone" or "entry now carries an error", because the probe cannot succeed against fixture tokens, and an error entry reports `exhausted == false`, i.e. "unknown", which is the correct reading.

`a_spent_five_hour_window_fails_fast_instead_of_waiting_minutes` (ADR-0016) exercises this end to end: it decides purely from the per-account cache, with no `with_quota_exhausted_override`, so it fails if the probe cannot read a label-keyed verdict.

### A defect this work introduced and then fixed

The background refresh is a detached `tokio::spawn`, and under test it outlived the test that started it, deleted usage snapshots a *later* test had seeded, and replaced them with probe errors, flipping that test's quota-versus-burst branch. It presented as `a_burst_limit_that_clears_lets_the_turn_finish` failing about one run in four while always passing under `--test-threads=1`.

Fixed twice over, because either fix alone leaves a trap: the refresh is off by default under `cfg(test)` and switched on only by the test that asserts it, and the burst-sensitive tests now state their premise through `with_quota_exhausted_override` instead of depending on ambient cache contents. Worth recording because the tempting fix, sprinkling cache clears at the top of each test, does not work at all: the writes arrive from another test's task *after* the clear.
