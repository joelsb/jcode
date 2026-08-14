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
