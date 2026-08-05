# ADR-0014: Detect WezTerm by its own marker variables, not only terminal identity

Status: Accepted
Date: 2026-08-05

## Context

A WezTerm user running jcode inside tmux saw every inline image replaced with:

```
ⓘ Your terminal cannot render inline images; showing a text fallback.
```

WezTerm speaks the iTerm2 inline-image protocol correctly, and `infer_protocol_from_env` already had a branch for it, keyed on `TERM_PROGRAM` and `LC_TERMINAL` containing `wezterm`.
The branch simply never fired, because a multiplexer takes those variables over:

| variable | outside tmux | inside tmux |
|---|---|---|
| `TERM` | `xterm-256color` | `tmux-256color` |
| `TERM_PROGRAM` | `WezTerm` | `tmux` |
| `WEZTERM_PANE` | `0` | `0` |
| `WEZTERM_EXECUTABLE` | `.../wezterm-gui` | `.../wezterm-gui` |

So detection fell through every branch and returned `None`, which the mermaid renderer correctly reports as "no image protocol".
The capability was never missing; the terminal was simply unidentifiable through the variables being consulted.

`real_iterm2_without_opt_in` had the same blind spot, and it mattered independently: it exists to distinguish real iTerm2 (whose image output corrupts the TUI, hence disabled by default) from WezTerm (which renders the same protocol correctly).
Inside tmux it could not tell them apart either, so even a successful stdio probe reporting `Iterm2` was downgraded to halfblocks.

## Decision

Consult WezTerm's own marker variables, which tmux passes through untouched, as an additional identity signal: `WEZTERM_PANE`, `WEZTERM_EXECUTABLE`, `WEZTERM_UNIX_SOCKET`.

- `infer_protocol_from_env_with_wezterm` takes the marker explicitly, keeping the function pure and testable; `infer_protocol_from_env` stays as the no-marker wrapper so existing callers and tests are unaffected.
- A marker that is present but blank is not treated as evidence. An exported-but-empty variable says nothing about the outer terminal.
- `real_iterm2_without_opt_in` consults the same markers, so WezTerm behind tmux is never mistaken for real iTerm2 and downgraded.

Terminal-identity variables are advisory and rewritable; a terminal's own private variables are a stronger signal precisely because no multiplexer has a reason to claim them.

## Consequences

- Inline images work in WezTerm inside tmux, screen, and any other multiplexer that preserves the environment, with no user configuration.
- The markers are inherited by child processes, so a WezTerm-launched shell that later execs a different terminal emulator could in principle report WezTerm falsely. Accepted: the failure mode is a wrong protocol guess in a contrived setup, versus a guaranteed loss of image support in the common one. Users with an exotic arrangement can force the authoritative probe with `JCODE_MERMAID_PICKER_PROBE=1`.
- tmux passthrough wrapping is not affected. `Picker::from_fontsize` sets `is_tmux` from the environment, so the escape wrapping images need inside tmux was already correct - only the protocol decision was wrong.

## Alternatives rejected

- **Always run the stdio capability probe.** It is authoritative and would have found the right answer, but it can block for two seconds when a terminal does not answer, putting an optional image capability on the TUI startup critical path. That is why the fast env path exists.
- **Treat an unknown terminal inside a multiplexer as image-capable.** Guessing optimistically corrupts the display on terminals that genuinely cannot render images, which is a worse failure than a text fallback.
- **Ask the user to set `JCODE_MERMAID_PICKER_PROBE=1`.** A workaround, not a fix. The environment already contained enough information to get this right.

## Revisit if

tmux gains a documented way to expose the outer terminal's identity, or ratatui-image's probe becomes cheap enough to run unconditionally at startup. Either would make the marker heuristic unnecessary.

## Evidence

- Reported live in WezTerm + tmux: `TERM=tmux-256color`, `TERM_PROGRAM=tmux`, `WEZTERM_PANE=0`, `WEZTERM_EXECUTABLE=/Applications/WezTerm.app/Contents/MacOS/wezterm-gui`.
- `crates/jcode-tui-mermaid/src/mermaid_runtime.rs`: `wezterm_env_marker`, `infer_protocol_from_env_with_wezterm`, `real_iterm2_without_opt_in`.
- Test: `infer_protocol_detects_wezterm_behind_tmux_via_its_own_markers`, which asserts both that the identity variables alone fail and that the marker succeeds.
