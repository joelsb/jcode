# Account-First Failover - DAIN Patch

Custom patch by DAIN (Joel) on branch `feat/account-first-failover`.
Adds `prefer_account_failover` config to jcode.

## Problem

When a model hits a 429 rate limit, jcode's TUI-layer fallback (`pick_next_fallback_route` in `fallback_pick.rs`) fires first and offers a smaller model on the same provider (Sonnet -> Fable -> Haiku).
The backend account failover (`try_same_provider_account_failover` in `multi_provider.rs`) never gets a chance because the TUI already downgraded.

With multiple OAuth accounts (joel, elara), we want: same model on next account before any model downgrade.

## Solution

Added `rate_limit: bool` flag to `FallbackPickOptions`.
When `rate_limit` is true, same-provider-different-model routes get tier 3 (below cross-provider tier 2), so the picker skips model downgrades.
This lets the backend account-failover mechanism try the same model on a different account first.

Gated by `prefer_account_failover` config option (default `true`).

## Files Changed

### Core logic
- `crates/jcode-provider-core/src/fallback_pick.rs` - Added `rate_limit` field to `FallbackPickOptions`, updated tier assignment, 3 new tests (14 total pass)

### Wiring
- `crates/jcode-base/src/provider/account_failover.rs` - Made `error_looks_like_usage_limit` public, added `prefer_account_failover_enabled()` accessor
- `crates/jcode-base/src/provider/mod.rs` - Added `pub use` re-exports for new public functions
- `crates/jcode-tui/src/tui/app/model_context.rs` - Set `rate_limit` flag in `FallbackPickOptions` construction, gated on `prefer_account_failover_enabled() && error_looks_like_usage_limit(error)`

### Config
- `crates/jcode-config-types/src/lib.rs` - Added `prefer_account_failover: bool` to `ProviderConfig` (default `true`)
- `crates/jcode-base/src/config/env_overrides.rs` - Added `JCODE_PREFER_ACCOUNT_FAILOVER` env var
- `crates/jcode-base/src/config/default_file.rs` - Added config comment
- `crates/jcode-base/src/config.rs` - Added env var to known list

## Commits

```
2b8b449 fix: remove duplicate private imports that caused unused-import warnings
2c9502d feat: add prefer_account_failover config option
6273711 feat: wire rate_limit flag into TUI fallback route picker
c0eeca7 feat: skip model downgrade on rate-limit, prefer cross-provider fallback
```

## Config

```toml
[provider]
# On rate limit, try same model on a different account before downgrading model (default: true)
prefer_account_failover = true
# Also requires:
same_provider_account_failover = true
```

Env var: `JCODE_PREFER_ACCOUNT_FAILOVER=true|false`

## Build and Install

```bash
cd ~/MYNE/Projects/jcode
git checkout feat/account-first-failover
source "$HOME/.cargo/env"
cargo build --release
bash scripts/install_release.sh --fast
```

## Architecture Notes

Two separate failover mechanisms exist:
1. **Route fallback** (TUI layer, `model_context.rs`): picks different model/provider via `pick_next_fallback_route`. User-facing (Ctrl+Y to accept).
2. **Account failover** (provider layer, `multi_provider.rs`): tries same model on different accounts via `try_same_provider_account_failover`. Automatic, no user interaction.

`ModelRoute` struct does not carry account labels - account failover is a completely separate mechanism from route-based fallback.

The tier system in `pick_next_fallback_route_with_options`:
- Tier 0: same model, different auth method (always best)
- Tier 1: same provider, different model (normal case)
- Tier 2: different provider (cross-provider hop)
- Tier 3: same provider, different model WITH rate_limit flag (our addition - pushes below cross-provider)
