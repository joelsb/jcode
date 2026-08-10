# ADR-0014: Recover the outer terminal's image protocol from markers that survive a multiplexer

Status: Accepted
Date: 2026-08-10

## Context

Running jcode inside tmux under WezTerm rendered every mermaid diagram as raw source text.
The user's session showed pages of `style u5 fill:#1e3a5f,...` instead of diagrams, which makes the diagram feature useless for anyone who lives in tmux.

Image-protocol detection was purely generic-env based. `infer_protocol_from_env` reads `TERM`, `TERM_PROGRAM`, `LC_TERMINAL`, and `KITTY_WINDOW_ID`.
tmux replaces exactly those signals with its own identity:

```
TERM=tmux-256color
TERM_PROGRAM=tmux
TERM_PROGRAM_VERSION=3.7
```

So detection returned `None`, the picker stayed on `Halfblocks`, and `native_image_protocol_available()` was false.
Mermaid then deliberately falls back to source text, because a half-block raster is worse than the source.
That fallback is correct in itself; the input to it was wrong.

The authoritative fix already existed as an opt-in: `JCODE_MERMAID_PICKER_PROBE=1` runs a stdio capability probe that asks the *outer* terminal directly.
It is opt-in because the probe can block for two seconds when a terminal does not answer, which is unacceptable on the TUI startup path.
Nobody discovers that env var from a screen full of mermaid source.

The key observation: the terminal exports its own markers before tmux starts, and tmux passes the environment through to children.
Measured in the failing session, these survived intact:

```
WEZTERM_EXECUTABLE=/Applications/WezTerm.app/Contents/MacOS/wezterm-gui
WEZTERM_PANE=0
WEZTERM_UNIX_SOCKET=...
```

## Decision

When the generic signals yield no protocol *and* a multiplexer is detected, fall back to terminal-specific env markers that survive the multiplexer:

- `WEZTERM_EXECUTABLE` / `WEZTERM_PANE` implies `Iterm2` (the same choice the direct WezTerm branch makes, because ratatui-image's Kitty path relies on Unicode placeholders WezTerm implements incorrectly).
- `GHOSTTY_RESOURCES_DIR` / `GHOSTTY_BIN_DIR` implies `Kitty`.

Both `init_picker` and `fast_picker` go through one entry point, `infer_protocol_for_current_env`, so the logging and the actual picker cannot disagree.

Two guards are deliberate:

- The survivor path only runs when `detect_multiplexer_from_env() != Multiplexer::None`. Outside a multiplexer the generic signals are authoritative, and a stale inherited `WEZTERM_*` var (e.g. a shell launched from WezTerm but displayed elsewhere) must not override them.
- Empty values are not evidence. `env_is_set` rejects empty strings, because an unset var can arrive as `""`.

## Consequences

Diagrams render for tmux/screen/zellij users of WezTerm and Ghostty with no configuration and no startup cost: this is a few `getenv` calls, not a probe.

The costs:

- It is a heuristic, not a capability query. A marker can be inherited by a process whose *current* terminal is different (ssh into a box from WezTerm, then attach tmux under a terminal with no graphics support). Then we claim a protocol the terminal does not speak, and images fail to draw instead of degrading to readable source text. The stdio probe remains the correct answer for that case and is still available via `JCODE_MERMAID_PICKER_PROBE=1`.
- It needs one branch per terminal. Terminals without a distinctive env marker are not covered and still need the probe.
- tmux additionally needs `set -g allow-passthrough on` for the image escapes to reach the outer terminal. It was already `on` in the environment where this was diagnosed, so it is not part of this decision, but it is a second necessary condition.

## Alternatives rejected

**Make the stdio probe the default inside a multiplexer.** Authoritative, and it fixes every terminal at once. Rejected because it puts a potential two-second block on TUI startup, the exact reason the fast path exists.

**Ask the user to set `JCODE_MERMAID_PICKER_PROBE=1`.** That is the status quo. It requires knowing the feature silently degraded, and why. Nothing in the output points there.

**Trust `WEZTERM_*` unconditionally, without the multiplexer guard.** Simpler, but it lets a stale marker override a live, correct generic signal. The generic signals are more trustworthy when they exist.

## Revisit when

- ratatui-image or the picker gains a cheap non-blocking capability query. Then default to the probe and delete these heuristics.
- A user reports images failing to draw (rather than falling back to text) inside a multiplexer. That is the inherited-stale-marker case, and the guard needs tightening, e.g. verifying the marker against the current tty.

## Evidence

Live end-to-end verification, same tmux window, same WezTerm host, both binaries queried over the debug socket with `client:mermaid:stats`:

| Binary | Reported protocol |
|---|---|
| `~/.jcode/builds/current/jcode` (before) | `"protocol": "Halfblocks"` |
| `target/selfdev/jcode` (after) | `"protocol": "Iterm2"` |

Tests:

- `crates/jcode-tui-mermaid/tests/tmux_masked_terminal_protocol.rs` sets the exact masked tmux environment measured above and asserts `native_image_protocol_available()`. It lives in its own test binary because the picker is a process-global `OnceLock`, so any other test touching it first would decide the outcome.
- Negative control: reverting `infer_protocol_for_current_env` to return `None` on the survivor path makes that test fail (`test result: FAILED. 0 passed; 1 failed`), then pass again once restored. The test is wired to the behavior, not to the environment it runs in.
- `multiplexer_survivor_markers_recover_the_outer_terminal` unit-tests the marker table, including the empty-string case.
