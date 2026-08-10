# Architecture Decision Records

One file per decision, newest last.
Each record states the forces that applied, the decision taken, and what the decision costs, so a later reader can tell whether the reasoning still holds.

Format: Michael Nygard's ADR template, trimmed to the sections this codebase actually needs.

## Status values

- **Accepted** - implemented and in effect.
- **Accepted (not yet implemented)** - decided and planned, code not landed. The record names the plan that carries it.
- **Superseded by ADR-NNNN** - replaced; kept because the history explains why the replacement exists.
- **Deferred** - a known problem consciously left unfixed, with the trigger that should reopen it.

## Index

| ADR | Decision | Status |
|---|---|---|
| [0001](0001-prefer-account-failover-over-model-downgrade.md) | On a rate limit, prefer another account over a smaller model | Accepted, partially superseded by 0002 |
| [0002](0002-rotate-accounts-inside-the-streaming-retry-loop.md) | Rotate OAuth accounts inside the Anthropic streaming retry loop | Accepted |
| [0003](0003-restore-the-account-override-only-on-a-dead-end.md) | Restore the account override only when rotation runs dry, and never blindly | Accepted |
| [0004](0004-each-rotation-earns-its-own-attempt.md) | Each rotation grants itself an attempt; transport retries counted separately | Accepted |
| [0005](0005-rotate-after-partial-output-using-retry-rollback.md) | Rotate after partial output, rolling the partial attempt back | Accepted |
| [0006](0006-test-only-url-seam-instead-of-an-env-override.md) | Drive the retry loop in tests through a `cfg(test)` URL seam | Accepted |
| [0007](0007-announce-rotation-to-clients-over-the-bus.md) | Announce rotation to clients over the bus and mirror it with an override | Accepted |
| [0008](0008-context-limit-derives-from-the-active-model.md) | Derive the context limit from the active model, never from the provider default | Accepted |
| [0009](0009-defer-per-session-account-identity.md) | Leave account identity process-global for now | Deferred |
| [0010](0010-invalidate-the-usage-cache-with-an-epoch-guard.md) | Invalidate the usage cache explicitly, guarded by an epoch | Accepted |
| [0011](0011-filter-the-rotation-broadcast-client-side.md) | Broadcast rotation to every client, filter it client-side by provider | Accepted |
| [0012](0012-announce-a-dead-end-restore-as-its-own-outcome.md) | Announce a dead-end restore as its own outcome, not as a switch | Accepted |
| [0013](0013-capacity-rejections-retry-on-their-own-budget.md) | Anthropic capacity rejections retry on their own budget and clock | Accepted |
| [0014](0014-recover-outer-terminal-image-protocol-behind-a-multiplexer.md) | Recover the outer terminal's image protocol from env markers that survive a multiplexer | Accepted |
| [0015](0015-rate-limits-wait-on-their-own-clock.md) | A rate limit that outlives account rotation waits on its own budget and clock | Accepted |
