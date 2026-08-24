# ADR-0024: A cached OAuth token belongs to an account, not just to a clock

- Status: accepted
- Date: 2026-08-24
- Supersedes: nothing
- Related: ADR-0007 (announce rotation over the bus), ADR-0009 (one daemon serves every session), ADR-0018 (claim the shared override only when better informed), ADR-0020 (serialise rotation)

## Context

Every mechanism for moving sessions between Anthropic accounts was in place and correct, and rotation still did nothing outside the session that triggered it. The user's report:

> the changing accounts is not working yet, maybe the accounts are not being changed across all sessions and the only session where the account is actually changed is the one that first triggers this

and, from a session that had rotated on paper:

```
⚡ Rate limited; switched Anthropic account: claude-2 → claude-1
× Anthropic API error (429 Too Many Requests): "This request would exceed your account's rate limit"
Switched to Anthropic account claude-1.
× Anthropic API error (429 Too Many Requests): ...
```

The header said `claude-1`. The 429s were still coming from `claude-2`.

The active-account override is process-wide (`RUNTIME_ACTIVE_OVERRIDES` in `account_store.rs`), a rotation rewrites it for every session, and it *was* being rewritten. But each session's `AnthropicProvider` holds its own `Arc<RwLock<Option<CachedCredentials>>>`, and `get_oauth_access_token` returned that cached token on **one** condition:

```rust
if creds.expires_at > now + 300_000 {
    return Ok((creds.access_token.clone(), true));
}
```

`CachedCredentials` recorded no account. So a session that had minted a `claude-2` token an hour ago kept returning it for the next five hours, never reaching `load_credentials()`, which is the only code that reads the override. The pointer moved; the session never looked at it again.

This fails silently by construction. The stale token is a *valid* token, so nothing downstream rejects it as an auth error and nothing retries on the auth path. The request simply lands on the account that just refused it, spends real quota to rediscover that, and the only visible symptom is a 429 in a session whose own header claims a different account. A brand-new session picked up the new account correctly, which is exactly what made it look like a broadcast problem: a fresh provider has an empty cache.

There is only one daemon (verified: PID 2372 holding `jcode-daemon.lock`, the sole `serve` process), so the multiple-servers hypothesis was not the cause.

## Decision

A cached token carries the account it was minted for, and is reusable only when both facts still hold.

- `CachedCredentials` gains `account_label: Option<String>`, set at every write site: the proactive refresh, the still-usable load, `credentials_for_claude_account` (both branches), and `force_refresh_oauth_token`.
- `cached_token_is_usable(cached, active_label, now_ms)` is the single rule, extracted as a pure function so the policy is testable without a live daemon or a network. It refuses an expired-or-expiring token, a token for a different account, and an *unlabelled* token.
- Unlabelled is refused, not trusted. A cache carried across a reload predates the field, and one extra refresh is cheaper than another silent 429 loop.
- `get_oauth_access_token` re-reads `active_account_label()` per request rather than caching it, because the whole point is that another session may have moved it since the last turn.

## Consequences

- A rotation in any session now takes effect in every session on its next request, without a restart and without waiting for token expiry.
- One extra `load_credentials()` (a local file read, no network) on the first request after a rotation. The refresh only happens if the new account's token is itself expiring.
- A token for account X can no longer be spent against account Y's quota, which was consuming quota purely to relearn a limit the process already knew.
- The label is `Option` rather than required, so nothing has to migrate a live cache.

## Rejected alternatives

- **Clear every provider's token cache from the rotation path.** The daemon has no registry of live `AnthropicProvider` instances, and building one couples the rotation policy to provider lifetime. Checking the pointer at the point of use needs no registry and cannot go stale.
- **Subscribe each provider to `BusEvent::AnthropicAccountRotated` and invalidate on receipt.** Correct-looking but strictly weaker: it fixes only the path that publishes. The override is also moved by `/account switch`, by startup auto-selection, and by the ADR-0012 dead-end restore, and a dropped or filtered event silently restores the old bug. Reading the pointer per request has no such gap.
- **Shorten the token cache lifetime.** Trades one silent wrong answer for a smaller window of the same silent wrong answer, and adds refresh traffic to every turn.

## Revisit when

Per-session account pinning arrives. If a session may deliberately stay on an account the shared pointer has left, `active_label` becomes a per-session value and this comparison must read that instead of the process-wide override.

## Evidence

- `a_cached_token_for_another_account_is_not_reused` - a valid `claude-2` token is reused for `claude-2`, refused for `claude-1`. This test fails on the pre-fix code.
- `an_unlabelled_cached_token_is_refreshed_once` - a pre-field cache entry is not trusted.
- `an_expiring_token_is_refused_even_for_the_active_account` - the expiry rule still applies, so the fix did not trade one condition for the other.
- One daemon confirmed at diagnosis time: `lsof` on `jcode-daemon.lock` names PID 2372, the only `jcode ... serve` process.
