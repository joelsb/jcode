# ADR-0001: On a rate limit, prefer another account over a smaller model

Status: Accepted, partially superseded by [ADR-0002](0002-rotate-accounts-inside-the-streaming-retry-loop.md)
Date: 2026-07 (DAIN patch, commits `c0eeca7`, `6273711`, `2c9502d`)

## Context

Two independent failover mechanisms existed, and they raced.

The TUI route picker (`pick_next_fallback_route` in `crates/jcode-provider-core/src/fallback_pick.rs`) offers the user a different model when a request fails.
The provider layer's account failover (`MultiProvider::try_same_provider_account_failover` in `crates/jcode-base/src/provider/multi_provider.rs`) retries the same model on a different OAuth account.

On a 429 the picker fired first and offered a downgrade (Opus to Fable to Haiku), so the account failover never got a turn.
With more than one Anthropic subscription available, a downgrade is the worse trade: the user paid for capacity on the second account and instead gets a weaker model on the exhausted one.

## Decision

Add a `rate_limit: bool` to `FallbackPickOptions`.
When set, same-provider-different-model routes drop to tier 3, below the cross-provider tier 2, so the picker stops volunteering a model downgrade as the first option and the account-level mechanism can act first.

Gate it on a config flag, `[provider].prefer_account_failover` (default true), which additionally requires `same_provider_account_failover`.

## Consequences

- A rate limit no longer leads with "use a weaker model".
- The picker only *reorders*; it cannot rotate accounts itself, and it still offers a downgrade when no better route exists. That gap is what ADR-0002 addresses.
- Two config flags now interact. `prefer_account_failover` alone does nothing if `same_provider_account_failover` is false, which is why `prefer_account_failover_enabled()` checks both.

## Why this was not enough

`ModelRoute` carries no account identity, so route selection is structurally incapable of expressing "same model, other account".
Reordering routes only helps if some other mechanism does the rotating - and for streaming requests, none did.
See ADR-0002.
