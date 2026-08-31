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
| [0003](0003-restore-the-account-override-only-on-a-dead-end.md) | Restore the account override only when rotation runs dry, and never blindly | Accepted, amended twice (2026-08-14: never restore onto a *cooling* account) |
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
| [0014](0014-recover-outer-terminal-image-protocol-behind-a-multiplexer.md) | Recover the outer terminal's image protocol from env markers that survive a multiplexer | Accepted, extended by 0025 |
| [0015](0015-rate-limits-wait-on-their-own-clock.md) | A rate limit that outlives account rotation waits on its own budget and clock | Accepted, exhaustion predicate superseded by 0016 |
| [0016](0016-exhaustion-is-any-window-not-every-window.md) | An account is exhausted when *any* usage window is spent, not every one | Accepted |
| [0017](0017-invalidate-the-cache-the-decision-reads.md) | Invalidate and correct the per-account usage cache the rotation decision reads | Accepted |
| [0018](0018-claim-the-shared-account-override-only-when-better-informed.md) | A rotation claims the shared account override only when better informed | Accepted |
| [0019](0019-only-turn-terminal-events-clear-resumed-turn-state.md) | Only turn-terminal events (Interrupted/Done/Error) clear the re-adopted-turn marker | Accepted |
| [0020](0020-serialise-account-rotation.md) | Serialise account rotation; followers adopt the leader's account instead of re-rotating | Accepted |
| [0021](0021-memory-decay-requires-a-verdict.md) | Memory confidence decays only on a real judge verdict, never on judge failure | Accepted |
| [0022](0022-retention-decays-from-last-use.md) | Memory retention decays from last use; expiry retires before it deletes | Accepted |
| [0023](0023-a-success-is-evidence-about-when-it-started.md) | A success clears a rate-limit cooldown only if it predates it; the pool wait uses the real deadline | Accepted |
| [0024](0024-a-cached-token-belongs-to-an-account.md) | A cached OAuth token belongs to an account, not just to a clock, so a rotation reaches every session | Accepted |
| [0025](0025-ask-the-terminal-in-every-window-where-nothing-else-is-reading.md) | Ask the terminal for its image protocol in every window where nothing else is reading; filter it by what the multiplexer forwards | Accepted |
| [0026](0026-a-fire-and-forget-child-must-still-be-reaped.md) | A fire-and-forget child must still be reaped, because a zombie costs a process-table slot | Accepted |
| [0027](0027-anthropic-fast-mode-is-speed-not-service-tier.md) | Anthropic fast mode is the `speed` field, never `service_tier` | Accepted |
| [0028](0028-claudeignore-is-a-cost-guard-not-a-permission-system.md) | `.claudeignore` is a token-cost guard, not a permission system; `bash` is deliberately exempt | Accepted |
| [0029](0029-a-zombie-is-signalable-but-not-running.md) | A zombie answers `kill(pid, 0)`, so liveness needs the process state, not signal deliverability | Accepted |
