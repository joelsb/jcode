# ADR-0025: Ask the terminal for its image protocol in every window where nothing else is reading

Status: Accepted
Date: 2026-08-24

Extends ADR-0014, which recovered the outer terminal's image protocol from env markers that survive a multiplexer. This one covers the case those markers actively get wrong, and the reload path where the answer was silently lost.

## Context

Mermaid diagrams rendered as blurry half-block smears (or as raw source) under WezTerm + Herdr, on and off, across roughly seven debugging sessions. Three separate faults stacked on top of each other, and each one produced a *clean, plausible, wrong* answer, which is why it survived so long.

### Fault 1: an env marker naming a protocol the multiplexer cannot forward

Herdr, unlike tmux, does not rewrite the outer terminal's identity. The failing session had:

```
TERM_PROGRAM=WezTerm
WEZTERM_EXECUTABLE=/Applications/WezTerm.app/Contents/MacOS/wezterm-gui
HERDR_ENV=1
```

`TERM_PROGRAM=WezTerm` is intact, so generic env detection confidently returned the iTerm2 protocol, which WezTerm genuinely supports. But iTerm2 graphics are an out-of-band escape payload, and Herdr does not forward them. jcode reserved image cells and drew nothing: a blank box.

ADR-0014's premise was that the surviving markers tell the truth about the terminal. They do. What they do not describe is what the *multiplexer between us and it* will pass through. A protocol is only usable inside a multiplexer if it survives a repaint (Kitty unicode placeholders do, iTerm2 and sixel do not).

### Fault 2: an upstream blacklist whose premise does not hold under a multiplexer

`ratatui-image`'s `from_query_stdio` refuses to even ask about Kitty when `WEZTERM_EXECUTABLE` is set, because WezTerm implements Kitty's unicode-placeholder part incorrectly. The blacklist is unconditional and not reachable through `QueryStdioOptions`, so every WezTerm user probes as `Halfblocks`.

That premise is right when WezTerm draws the image. It is wrong when Herdr sits in between with `experimental.kitty_graphics = true`: Herdr answers the Kitty query and composites the image itself, so WezTerm's placeholder handling never comes into it. Believing the blacklist there costs a real image protocol and yields two pixels per cell.

### Fault 3 (the expensive one): the answer could never be obtained after a reload

With faults 1 and 2 fixed, diagrams rendered correctly in a fresh session and then broke again after `selfdev build-reload`, which looked exactly like a regression in the fix. It was not. The log named it:

```
fresh session:  Mermaid picker stdio probe detected protocol: Kitty
after reload:   Probe reported Iterm2, which does not survive herdr; falling back to halfblocks
                Direct Kitty query got no answer; keeping the upstream halfblocks decision
```

The capability query writes an escape sequence to the tty and reads the reply. The guard refused to ask whenever the terminal was in raw mode, on the reasoning that the TUI's event reader would win the race and type `_Gi=31;OK\` into the user's composer (which had happened live). A reload/rebuild/update `exec`s a new binary onto a terminal the previous process *deliberately* left in raw mode with the alternate screen active. So the reloaded process refused to ask, held no answer, and fell back to halfblocks.

Passing the previous process's answer through an env var does not fix this on its own, and this was the wrong turn worth recording: **every process in a reload chain is in that same position**, so no process ever obtains an answer to hand on, and the chain stays blind forever. The handoff only works if some process can still ask.

The mistaken premise was equating "raw mode" with "unsafe". Raw mode was only ever a *proxy*. The real hazard is a **competing reader**. In the exec-handoff window the terminal is raw because the previous process left it that way, and jcode has not yet attached its event reader, so the raw flag carries no information about a competing reader.

## Decision

1. **Filter any detected protocol through the multiplexer's passthrough ability.** `usable_protocol_in_multiplexer` allows a protocol when there is no multiplexer, when the protocol survives a repaint, or when the user opted in with `JCODE_MULTIPLEXER_IMAGES=1`. Applied to the direct env signal and to the probe's answer alike, since both can name an unusable protocol.
2. **Second-chance Kitty query when the upstream probe settles on halfblocks, and only inside a multiplexer.** Bare WezTerm keeps upstream's decision, because there the blacklist is right.
3. **Gate the query on the absence of a competing reader, not on raw mode.** `safe_to_query` takes a `QueryWindow`. `BeforeRawMode` keeps the strict rule (`Some(false)` only; unknown is unsafe). `ExecHandoff` is allowed to ask despite raw mode, because it provably runs before the reader is attached.
4. **Carry a definite answer across the `exec` in `JCODE_TUI_INHERITED_KITTY_GRAPHICS`, and fall back to querying in the exec window when nothing was inherited.** Only an explicit `1`/`0` counts; absence means "never asked" and must not be inherited as a false negative.
5. **Probe when a multiplexer is present and env detection found nothing.** The startup latency is only spent by users whose alternative is the blurry fallback.

## Consequences

- Multiplexer users pay a bounded capability query (400 ms cap, normally single-digit ms) on startup and on reload. Non-multiplexer users are untouched.
- jcode now deliberately queries a raw-mode terminal in exactly one place. That is safe only as long as the exec-handoff path keeps running before the event reader is attached. Moving the call after `ratatui::init` reintroduces the escape sequence typing itself into the composer.
- The mermaid path depends on an upstream blacklist we work around rather than fix. If `ratatui-image` ever exposes the Kitty query for WezTerm, the rescue path becomes redundant and should be deleted, not kept "just in case".
- Herdr's `experimental.kitty_graphics` is a runtime setting with no exported marker, so its value is unknowable from the environment. Asking the terminal is the only way to find out, which is why this cannot be reduced back to pure env detection.

## Rejected alternatives

- **Trust `TERM_PROGRAM` under a multiplexer.** This is fault 1: it is true about the terminal and false about what reaches it.
- **Always probe.** Costs up to two seconds on the startup path for terminals that never answer, for users who already have a correct env answer.
- **Always halfblocks inside a multiplexer.** Gives up a working Kitty path that Herdr genuinely supports, and half-blocks are bad enough that mermaid prefers raw source over them.
- **Handoff env var alone, without the exec-window query.** Tried and shipped; it did not work, for the reason in fault 3. Kept only as the preferred path when an answer exists.
- **Widen the safety guard to allow any raw-mode query.** Reintroduces the live bug where the reply is typed into the user's composer. The exemption must stay scoped to the one window that is provably reader-free.

## What would make us revisit

- `ratatui-image` making the Kitty query reachable for WezTerm (delete the rescue path).
- Herdr exporting a marker for `kitty_graphics` (then env detection could answer without asking).
- Any change that attaches the event reader earlier in the exec path (breaks the `ExecHandoff` exemption's precondition).

## Evidence

- Failing log line, post-reload: `Probe reported Iterm2, which does not survive herdr; falling back to halfblocks` followed by `Direct Kitty query got no answer; keeping the upstream halfblocks decision`.
- Passing log line, post-reload, after the fix: `kitty query exec-window result` followed by `Mermaid picker stdio probe detected protocol: Kitty`, with the diagram rendering as a real image in the user's terminal.
- Tests: `kitty_probe::tests::exec_handoff_window_may_query_a_raw_terminal`, `kitty_probe::tests::query_is_refused_unless_the_terminal_is_known_to_be_cooked`, `cli::terminal::tests::kitty_graphics_answer_survives_an_exec_handoff`, `mermaid_runtime::tests::out_of_band_protocol_is_rejected_inside_a_multiplexer`.

## Debugging note for the next person

All three faults produced clean, plausible, wrong answers, and the log was the only thing that distinguished them. Start here:

```bash
grep -inE 'kitty query|Mermaid picker init|probe detected|does not survive' ~/.jcode/logs/jcode-$(date +%F).log | tail -20
```

`Mermaid picker init: mode=... env_protocol=...` gives the inputs to the decision, and the lines after it give the outcome. A `Halfblocks` result while a multiplexer is present is a wrong answer, not a capability limit.
