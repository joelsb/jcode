# ADR-0009: Leave account identity process-global for now

Status: Deferred
Date: 2026-08-04

## Context

Everything in [ADR-0002](0002-rotate-accounts-inside-the-streaming-retry-loop.md) through [ADR-0007](0007-announce-rotation-to-clients-over-the-bus.md) is built on a shared mutable fact: the active Anthropic account is process-global.

- `auth::account_store` keys the active override by the provider string `"claude"` alone, with no session dimension.
- One `AnthropicProvider` instance, holding a **single-slot** `Arc<RwLock<Option<CachedCredentials>>>`, serves every session on the daemon.

Two consequences follow, and neither is new. `MultiProvider::try_same_provider_account_failover` has had both since long before rotation existed:

1. A rotation started by session A changes the account for every session on the daemon.
2. The single credential slot means a concurrent session refreshing or reading credentials during A's rotation window transparently picks up the rotated account's token, attributing usage to an account its user did not choose - with no notice in that session.

What rotation *did* change is the window. `try_same_provider_account_failover` holds the rotated state only long enough to get response headers for one attempt; rotation inside the streaming loop holds it for an entire streaming turn, which can be minutes.

## Decision

Ship rotation with the process-global model, and record the limitation rather than paper over it.

Mitigations taken instead of a fix:
- The restore path compare-and-swaps against the live override so it cannot clobber another session's deliberate `/account switch` ([ADR-0003](0003-restore-the-account-override-only-on-a-dead-end.md)).
- The switch is announced to clients, so a user whose account changed underneath them can at least see it ([ADR-0007](0007-announce-rotation-to-clients-over-the-bus.md)).

The real fix - per-session account attribution, meaning a session-scoped account selection and a credential cache keyed by account - is an architectural change touching auth storage, the provider instance model, and the usage cache. It is out of proportion to a rate-limit failover patch.

## What would make us revisit

- More than one interactive session routinely sharing a daemon on the same provider. Single-session-per-daemon usage keeps the defect theoretical; concurrent usage makes it a billing-attribution bug.
- Any report of usage appearing on an account a user did not select.
- Work on per-session credentials for another reason (for example multi-tenant or team accounts), which would make this a rider rather than a project.

## Consequences of deferring

- Concurrent-session behavior stays incorrect in a way that is now *documented* rather than surprising.
- Any future feature that assumes "the active account is this session's account" is building on sand; it should read this record first.
- The rotation window being longer than the pre-existing one means this defect is more reachable than it was, so deferring is a bet on single-session usage that should be re-examined, not a conclusion that it does not matter.
