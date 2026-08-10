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

**Then accept the recovered protocol only if it survives a repaint.** Kitty's Unicode-placeholder path writes real characters into real cells, so when the multiplexer repaints, the placeholders are reissued and the image comes back. The iTerm2 and Sixel paths write an out-of-band escape at the cursor with no cell content behind it: the pixels live in the terminal, the multiplexer knows nothing about them, and every repaint (status line tick, scroll, pane focus) leaves diagrams flickering, duplicating, and smearing.

So inside a multiplexer, `Kitty` is accepted and out-of-band protocols are not. `JCODE_MULTIPLEXER_IMAGES=1` opts back in for users who want the images and accept the artifacts.

This is the corrected form of the decision. The first version accepted any recovered protocol, which fixed WezTerm-behind-tmux detection and immediately produced unusable flickering diagrams; readable mermaid source is strictly better than an image that will not hold still. See "Correction" below.

Both `init_picker` and `fast_picker` go through one entry point, `infer_protocol_for_current_env`, so the logging and the actual picker cannot disagree.

Two further guards are deliberate:

- The survivor path only runs when `detect_multiplexer_from_env() != Multiplexer::None`. Outside a multiplexer the generic signals are authoritative, and a stale inherited `WEZTERM_*` var (e.g. a shell launched from WezTerm but displayed elsewhere) must not override them.
- Empty values are not evidence. `env_is_set` rejects empty strings, because an unset var can arrive as `""`.

## Correction

The first version of this ADR claimed the fix delivered working diagrams in tmux under WezTerm, on the evidence that the reported protocol changed from `Halfblocks` to `Iterm2`.

That evidence was real but insufficient: it proved detection changed, not that anything was *legible*. In the live session, diagrams then flickered and smeared badly enough that the feature was worse than the text fallback it replaced, and a stuck `↻ rendering mermaid diagram...` placeholder was visible as well.

The reasoning error is worth keeping: "the protocol is now detected" was treated as equivalent to "the user can now read the diagram". A protocol string in a debug endpoint is one step in the chain, and the only check that mattered was the one not run, which was looking at the screen. A capability check is not an outcome check.


## Consequences

Ghostty users inside tmux/screen/zellij get real diagrams with no configuration and no startup cost: this is a few `getenv` calls, not a probe.

WezTerm users inside a multiplexer keep the readable mermaid source they had before, which is the honest outcome: WezTerm has no repaint-stable path in ratatui-image, so there is nothing to switch them to. They can opt into images with `JCODE_MULTIPLEXER_IMAGES=1`.

The costs:

- The repaint-stability table is per-protocol and hand-maintained. If ratatui-image gains a placeholder-based WezTerm path, `protocol_is_multiplexer_stable` must be updated or WezTerm stays on text forever.
- It is a heuristic, not a capability query. A marker can be inherited by a process whose *current* terminal is different (ssh into a box from WezTerm, then attach tmux under a terminal with no graphics support). Then we claim a protocol the terminal does not speak, and images fail to draw instead of degrading to readable source text. The stdio probe remains the correct answer for that case and is still available via `JCODE_MERMAID_PICKER_PROBE=1`.
- It needs one branch per terminal. Terminals without a distinctive env marker are not covered and still need the probe.
- tmux additionally needs `set -g allow-passthrough on` for the image escapes to reach the outer terminal. It was already `on` in the environment where this was diagnosed, so it is not part of this decision, but it is a second necessary condition.

## Alternatives rejected

**Make the stdio probe the default inside a multiplexer.** Authoritative, and it fixes every terminal at once. Rejected because it puts a potential two-second block on TUI startup, the exact reason the fast path exists.

**Ask the user to set `JCODE_MERMAID_PICKER_PROBE=1`.** That is the status quo. It requires knowing the feature silently degraded, and why. Nothing in the output points there.

**Trust `WEZTERM_*` unconditionally, without the multiplexer guard.** Simpler, but it lets a stale marker override a live, correct generic signal. The generic signals are more trustworthy when they exist.

**Accept any recovered protocol inside a multiplexer.** This was the first implementation. It makes detection correct and the display unusable, because tmux repaints over out-of-band pixels it cannot track. Rejected on live observation: the diagrams flickered worse than the text fallback they replaced.

**Ship the out-of-band protocol and try to suppress the flicker.** Would need jcode to know every repaint the multiplexer performs, which it cannot observe. The pixels are owned by the outer terminal and invisible to both tmux and jcode.

## Revisit when

- ratatui-image gains a repaint-stable (cell-backed) path for WezTerm. Then `protocol_is_multiplexer_stable` should accept it and WezTerm users get diagrams by default.
- ratatui-image or the picker gains a cheap non-blocking capability query. Then default to the probe and delete these heuristics.
- A user reports images failing to draw (rather than falling back to text) inside a multiplexer. That is the inherited-stale-marker case, and the guard needs tightening, e.g. verifying the marker against the current tty.
- Ghostty-behind-tmux turns out to flicker too. That would falsify the cell-backed-is-stable premise this decision rests on, and the correct response is to require the opt-in for every protocol inside a multiplexer.

## Evidence

Live end-to-end verification, real tmux session under the real WezTerm host, protocol read over the debug socket with `client:mermaid:stats`:

| Binary / env | Reported protocol | Meaning |
|---|---|---|
| `~/.jcode/builds/current/jcode` (before any change) | `Halfblocks` | text fallback, the original bug |
| first implementation | `Iterm2` | images, but flickering and unusable |
| current, default | `Halfblocks` | readable source, deliberately |
| current, `JCODE_MULTIPLEXER_IMAGES=1` | `Iterm2` | opt-in honored |

Renders themselves were confirmed to succeed independently of display, via `client:mermaid:ui-bench` in the same tmux session: `deferred_worker_renders: 2`, `render_errors` absent, `last_error: null`, PNG dimensions present. So the flicker was a display-path problem, not a render failure, and the stuck `↻ rendering mermaid diagram...` placeholder was a symptom of the display path, not of a hung renderer.

Tests, each in its own binary because the picker is a process-global `OnceLock` and the first test to initialize it decides the outcome for the whole process:

- `tests/tmux_masked_terminal_protocol.rs` - Ghostty behind tmux gets a native protocol and no text fallback.
- `tests/tmux_masked_terminal_out_of_band.rs` - WezTerm behind tmux keeps the text fallback, and an out-of-band protocol does not count as usable.
- `tests/tmux_masked_terminal_opt_in.rs` - `JCODE_MULTIPLEXER_IMAGES=1` restores images for that same case, so the documented escape hatch is verified rather than assumed.
- `multiplexer_survivor_markers_recover_the_outer_terminal` unit-tests the marker table, including the empty-string case.

Negative control on the detection half: reverting `infer_protocol_for_current_env` to return `None` on the survivor path made the tmux test fail (`test result: FAILED. 0 passed; 1 failed`), then pass again once restored. The test is wired to the behavior, not to the environment it runs in.
