# ADR-0003: Restore the account override only when rotation runs dry, and never blindly

Status: Accepted
Date: 2026-08-04 (commit `8b771e6`, after code review of `d84e1dc`)

## Context

Rotation mutates a process-wide account override.
The first implementation restored it on every hard-failure exit of `run_stream_with_retries` once any rotation had happened, with a single `rotated_account: bool` guard.

Review found two problems with that.

**It punished success.** If rotation moved to `claude-2`, that account worked, and some *unrelated* later failure ended the turn (transport retries exhausted, a failed forced token refresh), the code reverted to `claude-1` - the account just proven to be rate limited. The next turn would then start on the exhausted account, rediscover the 429, and rotate again.

**It wrote blindly to shared state.** One daemon serves many sessions and the override is keyed by `"claude"` alone (`auth::account_store`). A restore therefore could overwrite an account another session had deliberately selected with `/account switch` while this request was streaming.

A third, smaller issue: when a candidate account's credentials failed to load, the override was left pointing at that unusable account before moving on.

## Decision

Three rules:

1. Restore only on a dead end - when `switch_to_next_claude_account` returned `None`, meaning every account was limited. A rotation that landed on a working account sticks, matching what `MultiProvider::try_same_provider_account_failover` already does on success. Encoded as `should_restore_account(original, ours, rotation_exhausted)`.
2. Compare and swap. Before restoring, check that the live override still holds the label this request put there. Anything else means another session changed accounts meanwhile; leave it alone and log the skip.
3. Roll back failed candidates. If a candidate's credentials cannot be loaded or refreshed, put the previous override back before trying the next one, so the override never lingers on an account that cannot serve a request.

## Consequences

- After all accounts are exhausted, the session ends up where it started rather than pinned to whichever account it probed last.
- A working rotation survives unrelated failures, which is the behavior that makes the feature worth having.
- The compare-and-swap narrows but does not close the shared-state race: another session can still change the override *between* the check and the write. Closing it properly requires per-session account identity, see [ADR-0009](0009-defer-per-session-account-identity.md).
- `should_restore_account` is a pure function precisely so the policy is unit-testable without touching global auth state.
- Because the restore is now announced to clients ([ADR-0007](0007-announce-rotation-to-clients-over-the-bus.md)), rule 1 also decides *when the user is told the rotation gave up*. It is announced as its own outcome rather than as a switch, see [ADR-0012](0012-announce-a-dead-end-restore-as-its-own-outcome.md). Rule 2 has a corollary there: when the compare-and-swap declines to write, no event is published, so a client whose account was changed by another session is not notified.

## Amendment (2026-08-12): rule 1 does not apply to a spent quota

Rule 1 rests on a premise that is only sometimes true: that on a dead end, where every account is limited, the account the request started on is as good as any other, so returning there is neutral.

That holds for a burst limit, which clears in seconds. It is false for a 5-hour quota with hours left to run. A user hit exactly this: their session started on a quota-blocked account, rotation correctly moved to the account with headroom, the healthy account then also answered 429 (a burst), rotation reported a dead end, and rule 1 handed the session **back to the blocked account**. The next turn began there and rediscovered the wall, which is the cross-turn half of the account flapping in that report.

The two cases were indistinguishable when this ADR was written. They are not any more: [ADR-0016](0016-exhaustion-is-any-window-not-every-window.md) makes exhaustion correct and carries the blocking window's reset time. So `should_restore_account` now returns `false` when the original account's quota is known-spent and not yet reset. An unknown verdict, an unreachable probe, or a merely burst-limited account all keep the original rule.

Worth noting how it was missed for so long: every test of this path drove a *single* turn, and with two accounts a single turn cannot show the bug, because `tried_accounts` already excludes the spent account within a request. The rotation looks correct right up to the moment the request ends. `a_later_turn_does_not_return_to_the_quota_blocked_account` drives two turns and asserts which account the second one *starts* on. Negative control: forcing the old `rotation_exhausted && ours != original` fails it with "the second turn must not start on the quota-blocked account".

## Amendment 2 (2026-08-14): the gate is the cooldown, not the quota

The amendment above gated the restore on the original account's *quota* being known-spent. Too narrow, and the user hit the gap the same day.

Their accounts had headroom, 0% and 34% of their 5-hour windows, so nothing was quota-spent and the gate never fired. What they were hitting was the short-window **burst** limit, and the loop ran exactly as before, one switch and one restore per turn, indefinitely:

```
switched claude-2 -> claude-1
Every Anthropic account is rate limited; restored claude-1
switched claude-1 -> claude-2
Every Anthropic account is rate limited; restored claude-1
```

The correct signal is the **rotation cooldown**, because every 429 records one, sized to the real reset when known and to a short burst window otherwise. So rule 1 now reads: never restore onto an account that is cooling down. Quota and burst are both covered, and an account that is *not* limited is still restored, which keeps this ADR's actual purpose of undoing an exploratory hop that led nowhere.

A second change was needed for the loop to stop, in the candidate list rather than here: `demote_cooling_down_accounts` used to return the cooling accounts when none were ready, so rotation kept hopping onto an equally-limited account. It now returns nothing, and the resulting dead end falls through to ADR-0015's wait. That property is pinned by `no_candidates_still_waits_out_a_burst_rather_than_failing`; without it, removing the last-resort rule would trade flapping for an instant failure on a recoverable limit, which would be worse.

Measured over three turns: 3 rotations and 21 requests before, 1 rotation and 19 requests after.

The lesson worth keeping: the first fix was correct about *what* to gate and wrong about *which signal* proved it. A gate that can only fire in the case you happened to reproduce is not a fix, it is a coincidence.
