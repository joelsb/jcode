# ADR-0002: Rotate OAuth accounts inside the Anthropic streaming retry loop

Status: Accepted
Date: 2026-08-04 (commit `d84e1dc`)

## Context

[ADR-0001](0001-prefer-account-failover-over-model-downgrade.md) reordered fallback routes so account failover could act first.
It never fired. A real session produced 522 log lines mentioning `429` and zero mentioning `Same-provider failover`.

The reason is where the error surfaces.
`AnthropicProvider::complete*` builds an mpsc channel, returns `Ok(Box::pin(ReceiverStream::new(rx)))` **before the HTTP request is sent**, and runs `run_stream_with_retries` in a spawned task (`crates/jcode-provider-anthropic-runtime/src/lib.rs`).
A 429 therefore arrives as an `Err` *item inside the event stream*, not as an `Err` return value.

`MultiProvider::try_same_provider_account_failover` is only reachable from the `Err(err)` arm of `complete_with_failover` (`crates/jcode-base/src/provider/mod.rs:759`).
For streaming requests that arm is dead code, so account failover was structurally unreachable - regardless of config.

Meanwhile the retry loop treated 429 as transient (`is_retryable_error` matches `429 too many requests`) and burned its whole budget on the *same* exhausted account, honoring the account's `retry-after` (typically 60s) between attempts. Only after that did the error reach the TUI, which offered a model downgrade.

## Decision

Rotate accounts where the error actually appears: inside `run_stream_with_retries`.

On a usage-limit error, pick the next untried account from `jcode_base::provider::account_failover_candidates(ActiveProvider::Claude)`, set the process-wide account override, load or refresh that account's token into the provider's credential cache, clear the pending `retry-after`, and replay the same model.

Expose `account_failover_candidates` from `jcode-base` so the runtime reuses the existing exhaustion-aware ordering (usage-probe-sorted, current account excluded) rather than inventing its own account list.

## Consequences

- A streaming rate limit now reaches a second account without user interaction and without a model downgrade. The downgrade prompt becomes the last resort instead of the first.
- `retry-after` is deliberately discarded on rotation: the new account has its own quota bucket, so waiting out the exhausted account's window would be pointless delay.
- A provider runtime now touches auth state, which is a layering compromise. The alternative was to keep auth logic out of the runtime and wrap the returned stream in `MultiProvider` (peek the first item, retry on a usage-limit error before any output). That is cleaner in principle but does not help once output has started, and it duplicates retry logic that already exists one layer down.
- Scope is the Anthropic HTTPS/SSE runtime. Both the unified and split system-prompt paths share `run_stream_with_retries`, so both are covered. The Claude CLI runtime has its own retry loop and is not.
- The rotation is announced in the transcript (`StreamEvent::StatusDetail`) and the log (`Same-provider account failover`), because a silent account switch would make usage numbers inexplicable.

## Verification

Driven end to end against a raw-TCP mock server with two sandboxed fixture accounts: first call 429, second call 200, asserting two calls, a different bearer token on the second, a single `pong` delta (no duplication), and the active label ending on the second account.
Confirmed non-vacuous by mutation: disabling `should_rotate_account` makes the test fail.
