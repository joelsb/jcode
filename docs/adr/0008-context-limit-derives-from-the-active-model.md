# ADR-0008: Derive the context limit from the active model, never from the local provider default

Status: Accepted
Date: 2026-08-04 (commit `c6ece81`)
Plan: `docs/superpowers/plans/2026-08-04-account-rotation-tui-sync.md`

## Context

A user on a 1M-context model saw the context widget read `216k/200k` in red - the widget claiming the window was blown when it was not.

Eight sites in the TUI assigned the limit directly from the local provider:

```rust
app.context_limit = app.provider.context_window() as u64;
```

They sat in the account-switch paths - four in `auth_account_commands.rs`, two in `auth.rs`, two in `remote.rs` (line numbers as of `5797fef`, before the fix).
For a remote session the local provider's `context_window()` is a default (200k) that knows nothing about the `[1m]` model variant the daemon is actually serving, so switching accounts silently shrank the window by 5x.

The correct derivation already existed next door: `update_context_limit_for_model` (`crates/jcode-tui/src/tui/app/model_context.rs`) consults `context_limit_for_model_with_provider` for remote sessions and falls back to the provider window only when the model is unknown.

Note the trigger is an account switch, not the model - which is the tell that the code was answering the wrong question.

## Decision

Switching accounts changes credentials, not the model.
Every account-change path recomputes the limit from the active model via a single helper, `refresh_context_limit_after_account_change()`, which resolves the model as `effective_remote_provider_model()` falling back to `provider.model()`, then delegates to `update_context_limit_for_model`.
The remote-model fallback matters: on a remote session the local provider's model string is not authoritative, so asking it alone would reintroduce the same class of error this record exists to prevent.

Direct assignment from `provider.context_window()` is removed from those paths; a grep for the pattern is part of the task's verification so no site is missed.

## Consequences

- The 1M window survives account switches, including rotation-driven ones.
- The context bar drives compaction decisions, so a wrong limit was not cosmetic: it invited compacting work that did not need compacting.
- One helper is now the single place where "what changed" (credentials) is translated into "what to recompute" (model-derived limit). Future switch paths that copy the old pattern will be caught by the grep check rather than by a user noticing a red bar.
- `update_context_limit_for_model` also clears `context_warning_shown`, so the separate assignment of that flag at each site goes away. Verify that when editing, rather than assuming it.
