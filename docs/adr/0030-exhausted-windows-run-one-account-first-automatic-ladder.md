# 0030 - Exhausted usage windows run one account-first automatic failover ladder

**Status:** Accepted
**Date:** 2026-08-31

## Decision

A usage limit is one verdict regardless of which provider window produced it: the current account cannot accept the request.

When either Anthropic's 5-hour window or 7-day window is exhausted, jcode automatically:

1. marks the current account unavailable until the blocking window resets;
2. retries the same model on every eligible account on the same provider;
3. retries on the next configured provider only when no funded same-provider account remains.

`cross_provider_failover = "automatic"` is the default and crosses providers inside `MultiProvider::complete_with_failover`, without returning a client confirmation prompt. `countdown` and `manual` remain explicit opt-outs.

## What forced it

A real remote session reached Anthropic's weekly limit while its 5-hour window still had capacity. The daemon correctly reported:

```
OAuth usage exhausted. All Anthropic accounts appear exhausted.
```

It then returned `[jcode-provider-failover]{...}` instead of continuing to OpenAI. The remote client displayed a manual Ctrl+Y offer, and earlier versions retried the exhausted Claude route repeatedly.

A second inconsistency was earlier in the same path: remembered/preflight exhaustion entered `provider_precheck_unavailable_reason` and skipped `try_same_provider_account_failover`; only a fresh runtime 429 tried another account. The same exhausted account could therefore rotate or not rotate depending on whether the correct verdict came from the cache before the HTTP request or from the provider during it.

The provider does not distinguish these cases. A blocked 5-hour window, blocked weekly window, cached blocked verdict, and live 429 all mean the same thing: this account has no usable credits for the turn.

## Consequences

- Same-provider accounts always precede cross-provider work.
- A preflight verdict and runtime error produce identical routing behavior.
- Remote and local sessions no longer need different cross-provider mechanics in automatic mode; the daemon owns the full ladder.
- The same input can be sent to another configured provider without a countdown. Users who do not want that must set `countdown` or `manual` explicitly.
- Existing `auto` config values now mean `automatic`, not `countdown`.

## Rejected alternatives

**Special-case the weekly limit.** Rejected because it duplicates the 5-hour behavior and guarantees drift. Both are quota windows on the same account; either one blocks the request.

**Keep the three-second countdown as default.** Rejected because it stops an otherwise automatic account-first recovery, cannot operate in remote sessions, and turns an exhausted-account condition into a repeated manual action.

**Let the TUI own cross-provider failover.** Rejected because the provider layer knows which accounts were attempted and which provider candidates are configured. Moving the final step to the client loses that state and previously exposed an internal control payload.

## What would make us revisit

- A provider requires explicit user consent before cross-provider data transfer. That provider should declare the constraint rather than weakening the default ladder globally.
- Account identity becomes per-session rather than process-global; that changes concurrency mechanics, not the ordering decided here.

## Evidence

- `crates/jcode-base/src/provider/mod.rs`: remembered/preflight usage verdicts call `try_same_provider_account_failover`; automatic mode continues through the provider sequence.
- `crates/jcode-base/src/provider/tests/fallback_failover.rs`: automatic mode does not stop for a prompt; countdown and manual do.
- `crates/jcode-base/src/config_tests.rs`: default and parser behavior.
- `docs/DAIN_ACCOUNT_FIRST_FAILOVER.md`, Part 6.
- Real session screenshot on 2026-08-31: all Anthropic accounts exhausted, remote client stopped at a Ctrl+Y OpenAI offer instead of continuing.
