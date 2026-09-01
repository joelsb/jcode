# Handoff: provider failover work (2026-09-01)

Session context: started from "can we disable the menu bar?", ended deep in
provider failover. Written for a fresh session with no memory of this one.

## State right now

Branch `mine`, repo `~/MYNE/Projects/tools/jcode`.

Committed this session, oldest first:

| Commit | What |
|---|---|
| `8a70a1aa3` | Treat a zombie as not running, and reap the menu bar helper (ADR-0029) |
| `8bc92964c` | Stop remote provider failover retrying the exhausted route |
| `d6ace2a2c` | Make exhausted-account failover automatic and account-first (ADR-0030) |
| `cedae41cd` | Pin weekly and 5-hour exhaustion as one verdict |
| `0aeff5b4a` | Remove three private copies of the exhaustion rule |
| `805fbdedd` | Stop the fallback offer ping-ponging between exhausted routes |

`915769162` (docs, fork divergence) is from **another agent**, not this work.

**Daemon runs `d6ace2a2c`.** So `0aeff5b4a` and `805fbdedd` are committed and
built but **not live**. Reloading needs Joel's explicit approval per
`AGENTS.md`; do not reload to prove your own work.

The working tree has uncommitted changes from another session
(`turn_execution.rs`, `server.rs`, `agentgrep.rs`, `tool/mod.rs`,
`task_label.rs`, `prompt.rs`, `mcp/client.rs`, `session_tests/`,
`fallback_pick.rs`, `provider/failover.rs`, `provider/tests.rs`). **Not mine.
Leave them alone.**

## The one unfinished thing

`cargo test -p jcode-tui --lib` (full suite) was never seen to completion. Two
attempts were killed by a server reload, not by a failure. Everything narrower
passed: the new test, all 20 `fallback` tests, and a full `jcode-base --lib`
run showing only the 3 pre-existing failures listed below.

Run this first:

```bash
cd ~/MYNE/Projects/tools/jcode && source "$HOME/.cargo/env"
cargo test --profile selfdev -p jcode-tui --lib 2>&1 | tail -6
```

Known pre-existing failures in `jcode-base --lib`, present on a clean stashed
tree, **not caused by this work**:
`every_model_login_provider_has_explicit_lifecycle_normalization`,
`spawn_detached_creates_new_session`,
`streaming_guard_creates_visible_macos_sleep_assertion`.

Also: `cargo test -p jcode-base` **hangs forever** because of an uncommitted
test from another session, `a_spent_provider_is_skipped_whichever_provider_it_is`
in `provider/tests.rs`, which makes a real network call while holding
`lock_test_env`, blocking all 20 parallel tests. Use
`-- --skip a_spent_provider_is_skipped`.

## What was actually wrong, in causal order

Joel's report: Anthropic weekly at 1% remaining, and jcode kept showing
`[jcode-provider-failover]{...}` JSON and a Ctrl+Y prompt instead of moving on.

1. **Remote client never handled the failover marker.** It recognised it as a
   boolean, printed the raw JSON, then fell into the generic retry scheduler and
   resent to the exhausted provider, which returned the same marker. Infinite
   loop. Fixed in `8bc92964c`.

2. **Preflight exhaustion skipped account rotation.** A cached/preflight verdict
   went straight to the cross-provider prompt, while a live 429 correctly tried
   other accounts first. Same fact, two behaviours. Fixed in `d6ace2a2c`, which
   also made `automatic` the default cross-provider mode.

3. **The real root cause.** `UsageData::exhausted` is a disjunction (ADR-0016),
   but three callers kept private `five_hour >= 0.99 && seven_day >= 0.99`
   copies: `provider/startup.rs`, the Anthropic runtime, and
   `app-core/debug_command_exec.rs`. With 5h at 77% used and weekly at 1% left,
   `&&` returns **false**, so `provider_precheck_unavailable_reason` was told the
   account had headroom and the ladder never started. Fixed in `0aeff5b4a`, plus
   a guard test that walks `crates/` and fails on any reintroduced copy.

4. **Ping-pong.** With both providers genuinely spent, the picker is memoryless
   and ranks "the other provider" best forever. Fixed in `805fbdedd` by tracking
   `exhausted_fallback_routes`, cleared via `reset_credential_failure_breaker`
   (already fires on turn success and auth change).

## Verification approach used

Every fix has a planted-defect check. Do not weaken this.

- Restored the pre-ADR-0016 `&&` -> window test failed.
- Reinlined a copy in `startup.rs` -> guard test failed, naming `startup.rs:617`.
- Deleted the ping-pong skip block -> new test failed on its ping-pong assertion.
- Removed the remote marker handling -> remote regression test failed.
- Zombie: planted a real `<defunct>` pid, old binary stayed wedged, new binary
  respawned.

## Open items, most useful first

1. **Finish the full `jcode-tui --lib` run.** Only genuinely unverified thing.
2. **Reload to activate `805fbdedd`.** Needs Joel's approval. Build with
   `cargo build --release && JCODE_SKIP_SERVER_RELOAD=1 bash scripts/install_release.sh --fast`,
   then ask.
3. **Omniroute can never be a failover candidate.** `ActiveProvider`
   (`crates/jcode-provider-core/src/selection.rs:5`) is a fixed 8-variant enum
   with no slot for openai-compatible profiles, so a configured profile is
   invisible to the failover sequence. Joel said this is lower priority than the
   ping-pong, but it is why the error listed 7 providers and never Omniroute.
   The service is also not running (nothing on port 20128).
4. **Inflated token estimate.** The failover message said "7.3M input tokens"
   while the context widget showed 99k. Image base64 is counted as text by the
   failover estimator. Cosmetic, affects only the warning text.
5. **Two usage caches disagree.** `is_claude_usage_exhausted` reads
   `usage::get_sync()` (account-blind single slot) while
   `all_accounts_quota_exhausted` reads the per-account cache. ADR-0017 says
   decisions must read the per-account one. Real design question, needs a live
   exhausted-plus-funded account pair to verify; not attempted.

## Config, and a mistake I made

`~/.jcode/config.toml` currently: `cross_provider_failover = "automatic"`,
`features.menubar = false`, 7 hooks, `default_model = "claude-opus-5[1m]"`.

**I broke this once.** Restoring a backup dropped Joel's `[providers.omniroute]`
profile, because the backup I chose had an empty `[providers]`. Restored from
`config.toml.bak-omniroute-20260828-132025`. Backups worth knowing:
`config.toml.bak-GUTTED-20260831-212958` (the damaged one),
`config.toml.bak-preomniroute-restore`.

The two `launch_hotkeys` entries `dicdrepo` and `cmd+\` `backoffice-back` exist
only in the gutted file and were **not** restored. Joel was asked and has not
answered.

## Account state as of 09:00 today

All three verified working with a real turn on an isolated socket:
`claude-2` (5h 5%, 7d 22%), `claude-1` (0%/0%, weekly reset overnight),
OpenAI (5h 0%, 7d 32%). Active account is `claude-2`, unchanged. Both Anthropic
tokens valid until ~16:30.

So Joel's original failure was **real, not a bug**: every route was genuinely
spent at the time.

## How to verify runtime without disturbing anything

```bash
./target/selfdev/jcode run --no-update --socket /tmp/jc-check.sock 'reply with exactly: OK'
```

Never `strings` the shared-server symlink; resolve it with `readlink -f` first.
`crate::logging::info` writes to a file, not stderr, so use `eprintln!` for
throwaway diagnostics.
