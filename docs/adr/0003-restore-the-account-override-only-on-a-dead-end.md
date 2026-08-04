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
