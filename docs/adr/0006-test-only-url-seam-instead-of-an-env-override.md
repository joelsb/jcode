# ADR-0006: Drive the retry loop in tests through a `cfg(test)` URL seam

Status: Accepted
Date: 2026-08-04 (commit `162be29`)

## Context

Account rotation is a property of the retry loop, not of a pure function: it only shows up when attempt N fails with 429 and attempt N+1 goes out with different credentials.
Unit-testing the predicates (`should_rotate_account`, `next_untried_account`, `should_restore_account`) proves the policy but not the behavior.

`API_URL` and `API_URL_OAUTH` are `const`, so there was no way to point `stream_response` at a local mock server.
The workspace has no HTTP mocking crate available.

## Decision

Add the smallest possible indirection: a function `api_url(is_oauth) -> String` replacing the inline `const` selection, containing a `#[cfg(test)]` block that consults a `#[cfg(test)]` `OnceLock<Mutex<Option<String>>>` override.

Explicitly rejected: reading an environment variable. An env override compiles into release binaries, is readable by anything in the process, and turns a test affordance into a supported way to redirect a user's traffic to another host.

Tests then use a hand-rolled raw-TCP HTTP/1.1 mock server plus fixture accounts in a temp `JCODE_HOME`, rather than adding a mocking dependency for one test.

## Consequences

- The real retry/rotation loop is exercised end to end: two HTTP calls, distinct bearer tokens, no duplicated deltas, correct final account.
- In non-test builds the `cfg(test)` block does not exist and `api_url` reduces to the original selection. Verified on the shipped release binary: zero occurrences of the seam symbols, while the rotation strings are present.
- Cost: one `String` allocation per request instead of a `&'static str`, negligible next to an HTTPS round trip.
- The tests are **not offline-hermetic**, and this was accepted rather than solved. Reaching `account_failover_candidates` runs the usage probe, whose `USAGE_URL` in `crates/jcode-base/src/usage.rs` is a hardcoded const with no seam, so `cargo test` makes a real request to `api.anthropic.com/api/oauth/usage` with fixture tokens. It fails fast and the probe degrades to `list_accounts()`, so assertions hold either way. Sealing it needs a second seam in `jcode-base`; deferred as out of scope for the crate under test.
