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

[providers.omniroute]
type = "open-ai-compatible"
base_url = "http://localhost:20128/v1"
auth = "bearer"
api_key_env = "JCODE_PROVIDER_OMNIROUTE_API_KEY"
default_model = "auto/best-coding"
provider_routing = true
model_catalog = true
```

## Shell Aliases (~/.zshrc)

```bash
alias jc='git pull ; jcode -m "claude-opus-4-6[1m]"'
alias jc-5='git pull ; jcode -m "claude-sonnet-5"'
alias jc-or='git pull ; jcode --provider-profile omniroute -m "claude-opus-4-6[1m]"'
alias jc-5-or='git pull ; jcode --provider-profile omniroute -m "claude-sonnet-5"'
```

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
| `[providers.<name>]` | Named profiles (OmniRoute, etc.) |
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
