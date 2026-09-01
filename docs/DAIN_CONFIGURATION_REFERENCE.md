# Jcode Configuration Reference (DAIN)

Our jcode setup and config notes. Official docs: https://jcode.sh/docs#configuration

## Config File

`~/.jcode/config.toml` - everything has defaults, file can start empty.

## Our Config Highlights

```toml
[provider]
default_model = "claude-sonnet-5"
default_provider = "claude"
default_effort = "high"
openai_reasoning_effort = "high"
cross_provider_failover = "automatic"      # after every funded same-provider account is exhausted, switch provider immediately
same_provider_account_failover = true       # try other accounts on same provider
prefer_account_failover = true              # DAIN patch: account before model downgrade

[providers]                                 # empty: no named profiles configured
```

### OmniRoute: removed 2026-09-01

The `[providers.omniroute]` profile and the `jc-or` / `jc-or-5` aliases are gone.
It looked like a safety net and was not one:

- Nothing listens on `localhost:20128`; the service is not running.
- A named `open-ai-compatible` profile can **never** be selected by failover.
  `ActiveProvider` in `crates/jcode-provider-core/src/selection.rs` is a fixed
  enum with no variant for named profiles, so the profile is invisible to the
  failover sequence even when the service is up. That is why an exhausted-quota
  error listed 7 providers and never OmniRoute.

The `claude()` shell wrapper still points **Claude Code** at port 20128. That is
a separate tool and was deliberately left alone. Restore point:
`~/.jcode/config.toml.bak-20260901-122712`, `~/.zshrc.bak-20260901-122712`.

## Shell Aliases (~/.zshrc)

```bash
alias jc='git pull || true ; jcode -m "claude-opus-5"'
alias jc-5='git pull || true ; jcode --socket /tmp/jcode-sonnet5.sock -m "claude-sonnet-5"'
```

No `[1m]` suffix on purpose: from opus/sonnet 4.7 up the 1M window is native and
jcode strips the suffix before the request anyway.

## Accounts

Three Claude OAuth accounts configured (`jcode login claude -a <label>`):
- `claude-1` - primary (team subscription)
- `claude-2` - joel@datainnovation.io
- `claude-3` - elara@datainnovation.io

Failover order: sorted by usage (least-used first). `same_provider_account_failover = true` cascades through them.

## Key Config Sections

| Section | What |
|---------|------|
| `[provider]` | Default model/provider, reasoning effort, failover |
| `[providers.<name>]` | Named openai-compatible profiles (none configured; not failover-eligible) |
| `[display]` | Theme, diff mode, layout |
| `[features]` | Memory, swarm, mermaid, macOS menu bar indicator (`menubar`) |
| `[keybindings]` | All keyboard shortcuts |
| `[agents]` | Swarm config, concurrency |
| `[hooks]` | Shell commands at tool/session boundaries |
| `[tools]` | Tool profiles, allow/deny lists |
| `[ambient]` | Background agent mode |
| `[safety]` | Notifications (ntfy, desktop, email, telegram) |

## Supporting Files

| File | Location |
|------|----------|
| `AGENTS.md` | Repo root - per-repo agent instructions |
| `~/AGENTS.md` | Global agent instructions |
| `~/.jcode/mcp.json` | Global MCP server config |
| `.jcode/mcp.json` | Project MCP server config |
| `~/.jcode/skills/` | Custom skills (markdown) |
| `~/.jcode/auth.json` | OAuth credentials (auto-managed) |
| `~/Library/Application Support/jcode/provider-*.env` | API keys per provider |

## Environment Variables

All config keys overridable via `JCODE_<SECTION>_<KEY>` pattern:
- `JCODE_CROSS_PROVIDER_FAILOVER=countdown|manual`
- `JCODE_SAME_PROVIDER_ACCOUNT_FAILOVER=true|false`
- `JCODE_PREFER_ACCOUNT_FAILOVER=true|false` (DAIN patch)
- `JCODE_STREAM_IDLE_TIMEOUT_SECS=180`
- `JCODE_DEFAULT_REASONING_DISPLAY=off|compact|full`

## Build From Source

```bash
git clone https://github.com/1jehuang/jcode.git
cd jcode
cargo build --release
scripts/install_release.sh          # LTO, slow, optimized
scripts/install_release.sh --fast   # no LTO, quick
```

Binary paths:
- `~/.local/bin/jcode` - launcher symlink
- `~/.jcode/builds/current/jcode` - active build
- `~/.jcode/builds/stable/jcode` - stable release
- `~/.jcode/builds/versions/<hash>/jcode` - immutable versions

## Testing Changes at Runtime

`cargo build` alone does nothing - jcode runs via a daemon. To test without disturbing active sessions:

```bash
cargo build --profile selfdev
./target/selfdev/jcode run --no-update --socket /tmp/jcode-test.sock 'test prompt'
```

Or install properly and server auto-reloads:
```bash
scripts/install_release.sh --fast
```
