# DAIN fork divergence

What the `mine` branch carries that upstream `origin/master` (https://github.com/1jehuang/jcode) does not.

Generated 2026-09-01. Regenerate the numbers with the commands in the last section rather than editing them by hand.

## Position

| | |
|---|---|
| Upstream | `origin/master` at `325236119`, tagged `v0.81.4-2` |
| Our base | `bbef1f6a8`, `chore(release): prepare v0.76.0` |
| Our branch | `mine`, 129 commits ahead of that base |
| Behind upstream | 248 commits |
| Committed diff | 228 files, +23,124 / -963 |
| Uncommitted in tree | 17 files, +589 / -64, plus 2 untracked |
| Span | 2026-08-04 to 2026-08-31 |

The fork is not a patch set on current upstream. It is a patch set on v0.76.0 that has been maintained for four weeks while upstream moved five minor releases. Any rebase is a real merge exercise, not a fast-forward.

## Feature groups

Ordered by size. Every line count is `git diff --shortstat origin/master...mine -- <paths>`, so groups that share a file overlap slightly.

### 1. Anthropic account-first failover

21 files, +5,548 / -327. 40 commits. 20 of the 30 ADRs.

The largest single body of work, and the reason the fork exists. Upstream's answer to a rate limit is to downgrade the model. Ours is to try the same model on the next OAuth account first, and only then consider a different model or provider.

What it adds:

- Rotate the OAuth account inside the streaming retry loop, on an in-stream 429, not only at request setup.
- Remember a rate-limited account across requests, so a later request does not walk back into the same wall.
- Treat any spent usage window as exhaustion (five-hour *or* seven-day), pinned as one verdict rather than three private copies of the rule.
- Serialise rotation across concurrent sessions, so two sessions do not rotate past each other.
- Never restore a dead-end account override onto a quota-blocked account.
- Wait out a rate limit when waiting can help, fail fast when it cannot.
- Retry capacity rejections (`overloaded`) on their own budget and clock, ceiling 10, separate from the rate-limit path.
- Broadcast rotation on the bus (`anthropic_account_rotated`), forward it through the server, and follow it in the TUI usage widget so the client is never showing a stale account.
- Name the active account on screen in three places: header, status bar, side info widget, only when several accounts are configured.
- Reject foreign-vendor API keys sitting in OpenAI credential slots.
- `prefer_account_failover` config key.
- Per-account usage probing generalised beyond Claude, so a spent OpenAI account is recognised as spent.
- `docs/DAIN_ACCOUNT_FIRST_FAILOVER.md` describes the whole mechanism.

Hot files: `crates/jcode-provider-anthropic-runtime/src/lib.rs` (+1,140), its test file (+2,459), `crates/jcode-base/src/provider/account_failover.rs` (+480).

### 2. Anthropic fast mode

Part of group 1's file set, tracked separately because it is a distinct capability. 8 commits, ADR 0027, plan doc `docs/superpowers/plans/2026-08-27-anthropic-fast-mode.md` (+697).

- `speed` field on the Messages API request, `speed: fast`, not the Priority Tier / `service_tier` shape that looks equivalent and is not.
- `fast-mode-2026-02-01` beta header, sent only when fast mode is on.
- Capability gate so models without fast mode never send it.
- Parse and log the granted `usage.speed`, since the request is a preference and the grant is the fact.
- Inherit the saved fast-mode default, and fall back cleanly when the API refuses.
- Correct pricing for Opus 5 fast; removed invented Opus 4.7 / 4.6 fast rates.
- E2E test over real HTTP.

### 3. Documentation, ADRs and plans

41 files, +5,778. All new; upstream has none of them.

- 30 ADRs, `docs/adr/0001` through `0030`, plus a `README.md` index.
- `docs/DAIN_ACCOUNT_FIRST_FAILOVER.md`, `docs/DAIN_CONFIGURATION_REFERENCE.md`.
- `docs/handoff/` with three task briefs written to survive a session ending mid-bug.
- Three `docs/superpowers/plans/` implementation plans (account rotation TUI sync, session task label, fast mode) totalling ~2,700 lines.
- Root `CLAUDE.md` and `.claudeignore`.
- `AGENTS.md` amended with the never-reload-without-approval rule and the tools-checkout rule.

### 4. Memory agent and its health check

5 files, +1,617 / -195. 8 commits, ADRs 0021-0023.

- Self-limiting store with a unified write path, replacing several write sites.
- Never decay confidence when the judge did not run: a missing verdict is not evidence against a memory.
- Retention decays from last use, not from creation.
- `scripts/memory_health.py` (+403) checks the pipeline end to end.
- `scripts/test_memory_health.py` (+300) is a planted-defect meta-test: it injects known faults and requires the health check to catch each one. A checker that has never failed proves nothing.

### 5. Mermaid diagram rendering

12 files, +1,321 / -26. 11 commits, ADRs 0014 and 0025.

Upstream renders diagrams as text when it cannot prove the terminal supports images. Ours detects the real capability.

- Detect the outer terminal's image protocol from behind tmux, instead of reading the multiplexer's own (absent) support.
- `kitty_probe.rs` (+331) asks the terminal directly, in every window where nothing else is reading, so the answer survives a reload.
- Let the exec-handoff window ask about Kitty graphics.
- `/diagrams` opens transcript diagrams as images; `/images` recovers images a `/clear` would have dropped.
- Inline half-block diagrams restored behind the `mermaid_halfblocks` opt-in.
- Text fallback preserved for out-of-band image protocols under tmux.
- Five dedicated integration tests for the tmux-masked-terminal cases.

### 6. `.claudeignore` cost guard

9 files, +811 / -26. ADR 0028.

- `crates/jcode-tool-core/src/agent_ignore.rs` (+320), gitignore syntax, `.jcodeignore` wins over `.claudeignore`, never merged.
- Enforced by `read`, `ls` and `agentgrep` in grep/find modes. Deliberately **not** by `bash`, because this is a token-cost guard and not a permission system.
- Search and listing report what they withheld, so a short result is never mistaken for absence.
- Takes effect on the next tool call, no restart.

### 7. Session task labels

2 files, +903 / -18. 7 commits. Plan doc +1,218.

- Derive a short task label from user messages, with a stop-word list, and show it in the terminal title.
- The `todo` tool names the session in the call it already makes, so no extra model round trip.
- Persist per session in a sidecar with a staleness guard.
- Rank the task label above the todo title; refresh at turn end on both the local and remote paths.
- No emoji, wider label.

### 8. Prompt queue

~5 files, +164 test lines and the runtime changes folded into the TUI group. 10 commits.

- `/queue` toggles one-message-per-turn draining instead of dumping the whole backlog into one turn.
- Bound to Ctrl+Q, where the hotkey means exactly what the command means.
- Governs mid-turn routing and skill prompts, not just drain order.
- A user prompt the server rejects as busy is queued rather than lost, and not redispatched on every progress event.

### 9. MCP client correctness

3 files, +561 / -13. 2 commits, handoff brief TASK-3.

- Evict dead server handles instead of reusing a handle whose process is gone.
- Honour `tools/list_changed`, so a tool exposed mid-session actually becomes callable.

Both defects broke a live session; the brief in `docs/handoff/TASK-3-mcp-stale-handle-and-list-changed.md` records the symptom.

### 10. Skill frontmatter opt-out

4 files, +290 / -2.

- Upstream's serde parse of `SKILL.md` frontmatter silently drops the unknown `disable-model-invocation` key, so the flag looked honoured (it is, in Claude Code) while every opted-out skill stayed in the system prompt.
- On this machine that was 44 of 77 skills, roughly 2,500 prompt tokens per turn, plus a standing invitation for the model to self-invoke workflows marked manual-only.
- Filtered in the single render function, `build_available_skills_section` in `crates/jcode-base/src/prompt.rs`, not at the three `SkillInfo` call sites, so a new call site cannot re-expose them.
- Opted-out skills stay usable by explicit `/name`.
- Tests cover the filter, the all-opted-out case, frontmatter parsing, and the YAML folded-scalar shape the installed skills use.

### 11. Credential redaction

2 files, +233. New `crates/jcode-message-types/src/redaction.rs`.

- Redact credentials before persisting a message, rather than trusting that nothing sensitive reached the transcript.
- Record the billed account per message, so cost is attributable after the fact.

### 12. Process and daemon hygiene

Spread across `jcode-app-core` and `src/cli`. ADRs 0026, 0029.

- Reap detached children, so hooks stop eating process-table slots.
- A zombie is signalable but not running: treat it as not running, and reap the menu bar helper.
- Stop leaking `active_pid` markers and orphaning headed workers.
- Key the daemon lock to the socket rather than the runtime dir, so two sockets are two daemons.
- Revive a shared server that died without a reload handoff.
- Fire `turn_start` / `turn_end` hooks on the non-streaming turn path too (herdr agent-state support), `+101` test lines.

### 13. TUI, commands and quality of life

58 files touched in `jcode-tui` overall; test files alone are +1,782.

- `/xx` quit command, `Alt+X` quits, todo card moved to `Alt+T`; the quit chord wins over a stale config binding the same key.
- `/clear` no longer drops transcript images, `/images` recovers them.
- `features.menubar` config toggle for the macOS menu bar indicator.
- Keep the model's context limit across account switches.
- Honour launch flags in the header; reject foreign routes on a profile.
- A mid-turn `/rename` no longer fakes a rate limit and queues a message.
- Command-risk no longer gates stderr redirects and recursive reads.
- Bash tool names the command that set a nonzero exit code; the prompt teaches the model that a trailing `grep` is not a failure.
- `claude-opus-5` is the default Claude model; `idle_animation_persistent` upstreamed into config.

### 14. New config keys

Six keys upstream does not have:

`mermaid_halfblocks`, `idle_animation_persistent`, `queue_toggle`, `quit_now`, `menubar`, `prefer_account_failover`.

### 15. Uncommitted work in the tree

Not in any commit as of 2026-09-01, so absent from every `git log` based summary. 17 files, +589 / -64.

- `crates/jcode-base/src/provider/failover.rs` (+36): exhaustion checked through one per-account probe for every provider, not a Claude-only branch.
- `crates/jcode-provider-core/src/fallback_pick.rs` (+105): a rate limit must not be answered with the same model on the same provider's *other credential*, because on a subscription provider that is usually the same account behind a different door, and it silently moves the user onto metered billing. Deprioritised, never removed.
- `crates/jcode-tui/src/tui/app/model_context.rs` (+63): remember which routes reported a usage limit this turn and stop offering them, ending a Claude to OpenAI to Claude ping-pong that ran about once a second and never terminated.
- `crates/jcode-base/src/task_label.rs` (+175): larger stop-word list.
- `crates/jcode-base/src/prompt.rs`: `SHELL_PROMPT_MACOS`, teaching the model that `/bin/bash` here is 3.2.
- `crates/jcode-app-core/src/tool/agentgrep.rs`: suppression note on grep output.
- `crates/jcode-base/src/mcp/client.rs`: carry the `method` through.
- New test `remote_events_reload_04.rs` (+93).
- Untracked: `.local/prd/session-task-label-brief.md`, `crates/jcode-build-support/examples/publish_current.rs`.

## Porting this to the Pi harness

Pi is a TypeScript monorepo at `~/MYNE/Projects/tools/pi`, ten packages under `packages/`. Everything below was checked against that checkout on 2026-09-01, not inferred from feature names.

### The blocker

Pi's credential store is keyed by provider id, one credential per provider, and has no concept of an account. Our largest feature has nowhere to live until that type changes.

| File | What it says |
|---|---|
| `ai/src/auth/types.ts:51` | "App-owned credential storage, **keyed by `Provider.id`, one credential per provider**." |
| `ai/src/auth/types.ts:40` | `CredentialInfo` is `{providerId, type}`. No account identity. |
| `ai/src/auth/credential-store.ts` | `Map<string, Credential>` where the string is the provider id. Zero occurrences of "account". |
| `ai/src/utils/provider-retry.ts:105` | On a 429 it sleeps and reissues **the same request with the same credential**. There is nothing else to switch to. |

`Credential` and `CredentialStore` are exported from `@earendil-works/pi-ai`, a published package, so changing their shape is not an internal refactor. That decision belongs to Pi upstream, or it means forking pi-ai.

### Port order

Five items form one chain behind the credential change. Four depend on nothing and can be done in any order.

**Chain, in dependency order:**

1. **Multi-account credentials.** Keyed credential set per provider, account id on `CredentialInfo`, `auth.json` migration. Blocks the four below.
2. **Rotate the account on a 429.** `utils/provider-retry.ts` needs a credential-swap callback inside its retry loop.
3. **Exhaustion verdict and usage windows.** No equivalent exists anywhere in Pi. Five-hour and seven-day tracking, the any-window rule, cross-request memory.
4. **Route ranking.** No twin of our `fallback_pick.rs`. Includes the rule that a rate limit is never answered with the same model on the same provider's other credential.
5. **Rotation broadcast.** `core/event-bus.ts` can carry it; the UI to name the active account is new.

**Independent:** fast mode, the `.claudeignore` guard, mermaid as real images, task labels.

### Feature by feature

| Our feature | Pi today | Work |
|---|---|---|
| Skill `disable-model-invocation` | **Already implemented.** `visibleSkills` filters it in `agent/src/harness/system-prompt.ts:4`, with tests. | none |
| Prompt queue | Partly there: `steer` vs `followUp` queues and `queue_update` events in `core/agent-session.ts`. | small |
| Credential file hardening | `auth.json` written mode `0600` under `proper-lockfile`. | none |
| Account-first failover | Nothing. One credential per provider, retry reuses it. | the project |
| Fast mode | No `speed` or `service_tier` in `api/anthropic-messages.ts`. | one file |
| Mermaid as images | ASCII box-drawing via `grok-mermaid`. Zero hits for kitty, sixel or iterm. | full stack |
| `.claudeignore` | Nothing. Zero hits repo-wide. | module + wiring |
| Task labels | No OSC terminal-title write anywhere. | medium |
| MCP handle eviction | **Pi has no MCP at all.** Not a port, a feature. | out of scope |
| Memory health, redaction | Memory exists under `agent/harness/session/`; no redaction module. | medium |

Cheapest real win: port fast mode first. One file, self-contained, and it proves the pipeline before anyone commits to the credential refactor.

## Blast radius for a rebase

| Crate | Files modified |
|---|---|
| `jcode-tui` | 58 |
| `jcode-base` | 46 |
| `jcode-app-core` | 23 |
| `jcode-provider-core` | 4 |
| `jcode-provider-anthropic-runtime` | 3 |
| `jcode-config-types` | 3 |
| everything else | 1-2 each |

Our two heaviest files, `anthropic_tests.rs` (+2,459) and the Anthropic runtime `lib.rs` (+1,140), sit in the area upstream churns most. The isolated new files (`agent_ignore.rs`, `task_label.rs`, `kitty_probe.rs`, `redaction.rs`, the whole of `docs/`) will rebase clean.

## Regenerating this

```bash
git fetch origin
git log --oneline origin/master..mine                 # our commits
git diff --diff-filter=A --name-only origin/master...mine   # files we added
git diff --stat origin/master...mine                  # full committed diff
git status --short --untracked=all                    # uncommitted work, easily missed
git diff --shortstat origin/master...mine -- <paths>  # per-group size
```

`git log origin/master..mine` omits working-tree changes. Pair it with `git status` or the summary understates the fork.
