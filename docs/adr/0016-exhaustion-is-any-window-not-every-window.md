# ADR-0016: An account is exhausted when *any* usage window is spent

Status: Accepted
Date: 2026-08-12

Supersedes the exhaustion predicate assumed by [ADR-0015](0015-rate-limits-wait-on-their-own-clock.md).

## Context

A user reported jcode flapping between two Anthropic accounts and then hanging for minutes, repeatedly, across five concurrent sessions on one daemon:

```
⚡ Rate limited; switched Anthropic account: claude-1 → claude-2
⏳ Every account rate limited; waiting 59s and retrying (1/5)
⏳ Every account rate limited; waiting 59s and retrying (2/5)
⏳ Every account rate limited; waiting 59s and retrying (3/5)
⏳ Every account rate limited; waiting 59s and retrying (4/5)
⚡ Rate limited; switched Anthropic account: claude-2 → claude-1
```

`jcode usage`, run while that was happening:

| Account | 5-hour | 7-day | jcode's `exhausted` |
|---|---|---|---|
| `claude-1` | 41% | 24% | `false`, correct |
| `claude-2` | **100%**, resets in 2h51m | 49% | `false`, **wrong** |

`claude-2` was refusing every request for the next three hours, and jcode believed it had headroom, because:

```rust
exhausted: usage.five_hour >= 0.99 && usage.seven_day >= 0.99,
```

Anthropic applies the 5-hour and weekly limits independently and refuses a request when **either** is full. A conjunction can therefore only ever be true in the rarest case, both windows full at once, and reports a hard-blocked account as usable in every other case.

Three consequences, all visible in the logs above:

1. `account_failover_candidates` filters on `!exhausted`, so the blocked account stayed a *preferred* rotation target and kept being selected.
2. `all_accounts_quota_exhausted()` stayed false, so ADR-0015's "a spent quota fails fast" path never fired, and every turn spent its full rate-limit budget waiting.
3. `switch_guidance()` requires `current_exhausted()`, so the "another account has headroom" hint never appeared, on the one occasion it was true and useful.

The OpenAI-shaped predicate had the same defect in a different spelling, `windows.all(|w| w.usage_ratio >= 0.99)`.

Two further problems compounded it, both fixed here:

- **The cooldown was a blind 60s timer.** ADR-0015 chose 60s deliberately, sized to a burst limit. Applied to a 3-hour quota wall it means jcode forgets within a minute and selects the blocked account again, which is the ping-pong.
- **A quota `retry-after` was honoured as if it were a burst.** Anthropic sends `retry-after: 60` for both, so each "burst" wait was a full minute and four of them read as a four-minute hang. For a genuine burst that is far too long; for a quota wall no wait is right at all.

## Decision

- `exhausted` is a **disjunction**: any window at or above `EXHAUSTED_RATIO` (0.99) blocks the account. Same change for the OpenAI variant, `all` becomes `any`, counting only windows the provider actually reported so a missing window is read as neither full nor free.
- A new `blocking_reset_at()` reports the reset of a *spent* window, preferring it over an unspent one's. The reset only means something paired with the window it belongs to: reporting the 5-hour reset for an account blocked weekly promises recovery days early.
- The rotation cooldown stores an **expiry instant**, not a start time, so a quota limit can be remembered until its real reset (`note_account_rate_limited_until`) while a burst keeps the short 60s fallback. Clamped: never shorter than 60s (a reset in the past or seconds away must still stop an immediate retry) and never longer than 6 hours (a wrong or stale reset must not hide an account indefinitely). A later burst-shaped note keeps the longer deadline rather than shortening it.
- A cooling-down account is **skipped while any non-cooling candidate remains**, and kept as a last resort when every candidate is cooling. ADR-0015 only sorted it last, which is not enough: with two accounts both cooling, the order among equals is arbitrary, so the spent one could still be tried first.
- A server `retry-after` on the burst path is capped at `MAX_BURST_RETRY_AFTER` (15s), and the terminal failure now names the earliest reset: `⛔ Every Anthropic account is out of quota; earliest reset 2h51m (claude-1)`, once per turn.

## Consequences

For the reported incident: `claude-2` is recognised as blocked, dropped from rotation, and the turn runs on `claude-1`. When both accounts really are spent the turn fails after one request per account with a reset countdown, instead of four silent minutes followed by an offer to downgrade the model, which does nothing about a per-account quota.

The costs:

- **Exhaustion is now stricter, so a wrong "100%" removes an account from rotation.** Mitigated by only excluding an account with an actually-full window, and by keeping an unreachable probe as "unknown" (ADR-0015's rule, unchanged).
- **A capped `retry-after` disobeys the server on the burst path.** Deliberate: the server cannot distinguish the two cases it is answering, and 15s already outlasts a burst window. The quota path no longer waits at all, so nothing is lost by not waiting the full minute.
- **A cooling account is invisible while an alternative exists**, so a burst on one account routes work to the other for up to a minute even if the first recovers sooner. A success clears the cooldown immediately, which bounds this in practice.

## Rejected alternatives

**Keep the conjunction and special-case the 5-hour window.** The asymmetry is not real: either window blocks requests, and a rule that names one window would break the moment a third appears (a `spark` window already exists in the payload and is currently ignored on the Anthropic path).

**Treat `exhausted` as a ratio threshold across windows, e.g. max().** Equivalent to the disjunction for the decision, but loses which window is blocking, and so loses the honest reset time.

**Drop cooling accounts entirely from the candidate list.** Turns a recoverable rate limit into a hard failure whenever every account is briefly limited. Precisely what ADR-0015 warned against, so they remain a last resort.

**Wait longer instead of failing fast.** What the user first suggested. It makes the observed symptom worse: the complaint is a multi-minute silence, and more waiting on a three-hour wall extends it without any chance of success.

## Revisit when

- Anthropic exposes which limit a 429 hit, or a burst reset, in the response. Then the burst-versus-quota split stops being inferred from the usage probe.
- The `spark` window turns out to be the real burst limiter. Including it in this predicate would make burst limits *observable* rather than inferred, and would likely simplify ADR-0015's waiting path.
- Anyone reports an account being skipped that they believe has headroom. That is the failure mode this change makes possible, and it should be traced to the probe rather than the predicate.

## Evidence

`one_spent_window_is_enough_to_exhaust_an_account` uses the incident's real numbers: 5h=100% / 7d=49% must be exhausted and report the 5-hour reset; 5h=10% / 7d=100% must be exhausted and report the *weekly* reset; 41% / 24% must not be exhausted.

`an_account_snapshot_reports_a_single_spent_window_as_exhausted` covers the field the rotation filter actually reads, since the predicate being right does not help if the snapshot drops it.

`a_spent_five_hour_window_fails_fast_instead_of_waiting_minutes` drives the real `run_stream_with_retries` against an always-429 server with both accounts quota-blocked, and asserts a request *count* (3, one per account) rather than elapsed time, so it stays honest under the `clamp_retry_delay` test clamp.

Negative control, run on 2026-08-12: restoring `&&` makes that test report **7 requests with four waits** and fails four tests across two crates. Its failure output is the user's bug verbatim:

```
left: 7, right: 3
details: ["⚡ Rate limited; switching Claude account: claude-1 → claude-2",
          "⏳ Every account rate limited; waiting 4s and retrying (1/5)",
          "⏳ Every account rate limited; waiting 9s and retrying (2/5)",
          "⏳ Every account rate limited; waiting 21s and retrying (3/5)",
          "⏳ Every account rate limited; waiting 46s and retrying (4/5)", ...]
```

`a_known_reset_outlasts_the_burst_cooldown` pins the cooldown deadline: a 3-hour reset still cools after the 60s window would have expired, a later burst note does not shorten it, and a reset in the past still yields the short cooldown rather than none.

`cooling_down_accounts_are_skipped_but_remain_a_last_resort` pins the candidate rule, including the "everything is limited" case. Negative control: restoring the `sort_by_key` ordering fails both it and the reset test.

### Two test defects this work exposed

Both were pre-existing hazards that this change made load-bearing, and both produced *clean, plausible, wrong* results:

1. **The first version of the end-to-end test passed against the very bug it was written to catch.** It seeded the limit through the 429 path, which also sets a rotation cooldown, and a cooldown alone is enough to steer rotation. The `exhausted` predicate was never exercised. Fixed by adding `seed_anthropic_account_usage_for_tests`, which sets a usage verdict and nothing else, and by asserting the fast-fail rather than the routing (with only two accounts, `tried_accounts` excludes the spent one regardless, so routing cannot discriminate).

2. **Three tests shared process-global state and failed each other roughly one run in three or four.** `RATE_LIMITED_ACCOUNTS` and the per-account usage cache are both process-wide while tests run in one process. Fixed with `lock_account_cooldown_tests()`, by having burst-sensitive tests state their premise through `with_quota_exhausted_override` instead of depending on ambient cache contents, and by defaulting the new post-rotation refresh off under `cfg(test)` (see ADR-0017) since a detached task outlived its own test and rewrote a later test's inputs.

The general lesson from both, worth more than either fix: a test that sets up its condition through a *rich* helper may be satisfied by a side effect of that helper rather than by the behaviour under test. The negative control is what caught it, and it caught it only because it was run.

### A third defect, found only by testing the reported symptom

After all of the above was committed, tested, and verified against a live account, a further test written specifically to check *the property the user described* rather than the property the code made convenient to check found a bug none of the others could see.

Every existing test drives a **single turn**. With two accounts, a single turn cannot expose it: `tried_accounts` already excludes the spent account within a request, so even the unfixed code rotates away exactly once and looks correct. The failure is *between* turns, and it was the dead-end restore from ADR-0003 handing the session back to the quota-blocked account it started on. The next turn then began there again, which is precisely the `claude-2 → claude-1 → claude-2` sequence in the report.

`a_later_turn_does_not_return_to_the_quota_blocked_account` drives two turns and asserts which account the second one *starts* on. Fix and amendment recorded in ADR-0003.

Two things this cost, both worth remembering:

- **The single live turn cited as verification proved less than claimed.** It ran one turn, succeeded, and would very likely have succeeded on the old binary too. It was reported as confirmation and was not; the negative controls were the real evidence throughout.
- **Testing the mechanism is not testing the symptom.** Every unit test here was correct and passing while the user-visible behaviour was still broken, because they all shared an assumption (one turn) that the bug lived outside of.

### Running the same lens back over the earlier fixes

The multi-turn lens was invented only after the fixes above were written, so it had never been applied to them. Run retrospectively, it exposed a second blind spot in the *opposite* direction: every test for these fixes sets up a blocked account and ends while the block is still in force, so none of them could show whether the exclusion ever **lifts**.

That is not a theoretical gap. If recovery did not work, these fixes would have traded the reported flapping for something strictly worse: an account sidelined for the rest of the process even after its window rolled over. The fixes make exhaustion stickier in three independent places (a cached verdict, a cooldown sized from a reset, and a synthetic 429 entry), and any one of them failing to expire would strand the account.

Two tests now cover it, and the negative control shows the dependency is real rather than incidental: removing the reset-passed rule from `UsageData::is_stale` fails `an_account_becomes_usable_again_once_its_window_resets` with "a verdict whose reset has passed must be stale, or the account stays excluded for the rest of the cache TTL". `a_429_evidence_entry_expires_when_its_reset_passes` covers the same property on the write path.

The general point, which is the reason this section exists at all: when a fix is *found* by a new kind of test, that test is evidence the old lens was too narrow, and the new one should be run back over everything the old one signed off. Doing so here found no further production bug, which is itself worth recording, but it did find untested behaviour whose failure mode would have been worse than the original complaint.
