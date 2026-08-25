# 0026 - A fire-and-forget child must still be reaped, because a zombie costs a process-table slot

**Status:** Accepted
**Date:** 2026-08-25

## Decision

Every spawn whose `Child` would otherwise be dropped goes through
`platform::spawn_detached_reaped()`, which hands the `Child` to a single
long-lived reaper thread (`platform::reap_child_in_background`) and returns only
the pid. `platform::spawn_detached()` stays available for the two callers that
genuinely keep the `Child` and wait on it themselves (`tool/bash.rs`,
`tool/open.rs`).

The reaper only ever waits on children explicitly handed to it, one owned
`Child` at a time. It must never call `waitpid(-1)`.

## What forced it

On 2026-08-25 every `fork()` on the machine started failing with `EAGAIN`. The
user's shell could not start a single command:

```
/Users/joelsbastos/.zshrc:141: fork failed: resource temporarily unavailable
```

Line 141 was `eval "$(pyenv init - --no-rehash zsh)"`, the first command
substitution in the startup file. It was not the cause, only the first thing
that needed a fork.

Measured state:

| Metric | Value |
|---|---|
| processes owned by the uid | 2677 |
| `kern.maxprocperuid` | 2666 |
| of those, `<defunct>` | 2254 |
| distinct parents of those 2254 | **1** |
| that parent | `jcode --provider auto serve`, uptime 10h18m |
| `php` processes (the initial suspicion) | 6 |
| growth rate | ~4 zombies/min |

Zombie ages tracked that one server's lifetime exactly: 237 at 10h, 735 at 9h,
514 at 8h, 287 at 7h, 312 at 6h, 180 under 1h.

The mechanism is one line of the standard library's contract:
`std::process::Child::drop` does **not** wait. A child that exits while its
parent holds no `Child` handle and never calls `wait()` stays in the process
table as a zombie until the parent dies. `dispatch_observer` in `hooks.rs`
spawned each hook process and dropped the `Child` immediately.

The user's config wires 5 hook events, of which `post_tool` and `turn_start`
fire on **every tool call**. So the leak rate is proportional to how hard the
agent is working, which is the worst possible coupling: the longer a session is
useful, the closer the machine gets to being unable to start a process at all.

## Why it stayed invisible for 10 hours

A zombie holds no memory, no CPU, no file descriptors. Only a slot. So:

- Activity Monitor and `top` look completely healthy.
- Total memory and load are normal.
- jcode itself keeps working perfectly, including the hooks - they run, they
  succeed, and they leak.
- The first symptom appears in an unrelated program (a shell, a compiler,
  anything that forks), which sends the investigation to the wrong place.

The first hypothesis in this incident was a set of leaked `php -S` dev servers
from the user's own shell aliases, which was a genuinely real and separate
defect. It accounted for **6 of 2677** processes. The leak was in this
repository the whole time.

## Consequences

- One extra OS thread per process, created lazily on the first detached spawn.
  It blocks on a channel `recv()` while idle, so an unused process pays nothing.
- Reaping is not instant: the thread polls `try_wait()` at 250ms while anything
  is pending. A slot is released within a few hundred ms of the child exiting,
  which is irrelevant against a per-uid cap in the thousands.
- `spawn_detached_reaped` returns `u32` rather than `Child`, which is
  deliberate: getting a `Child` back is exactly the shape that caused this bug.
  A caller that needs to wait must say so by using `spawn_detached`.
- Callers can no longer observe the exit status of a fire-and-forget child. That
  is already true in practice (they dropped it), and observer hooks are
  documented as fire-and-forget.

## Rejected alternatives

**`signal(SIGCHLD, SIG_IGN)` or a `waitpid(-1)` reaper loop.** One line, no
thread, reaps everything. Rejected because it reaps *everything*: it races every
other `wait()`/`try_wait()` in the process and steals exit codes from callers
that need them. `background.rs:152` reaps its own pids via
`try_reap_child_process`, `tool/bash.rs` polls `try_wait()` to decide whether a
command finished or must be promoted to a background task, and `tool/open.rs`
uses a grace-period `try_wait()` to detect an opener that died instantly. A
global reaper would silently break all three, turning a visible resource bug
into an invisible correctness bug. The reaper owning only what it is given is
the whole point of the design.

**Double-fork so the grandchild is reparented to init.** Standard daemon idiom
and it does solve the leak. Rejected because `spawn_detached` already uses
`setsid()` and callers need the direct pid: `signal_detached_process_group(pid)`
signals the whole group, and `background.rs` tracks tasks by pid. After a double
fork the pid we hold is the intermediate that already exited, so both break.

**Have each caller wait.** Defeats the purpose of a fire-and-forget hook: an
observer must not be able to block a turn.

**Raise `RLIMIT_NPROC` / `kern.maxprocperuid`.** Treats an unbounded leak as a
sizing problem. Buys hours, then fails identically.

## What would make us revisit

- A caller genuinely needing the exit status of a detached child. Then it should
  use `spawn_detached` and own the wait, not weaken the reaper.
- Evidence the 250ms poll matters. It could become a `SIGCHLD`-driven wake, but
  only while keeping the "wait on owned handles only" property.

## Evidence

- Diagnosis: 2254 zombies, single PPID, against `kern.maxprocperuid` 2666, with
  the age histogram matching the parent's 10h uptime.
- `crates/jcode-base/src/platform_tests.rs`:
  - `dropping_a_detached_child_leaks_a_zombie` - guard test, asserts the defect
    is real and the `ps`-based detector actually fires. Without it, the two
    tests below could pass because the detector is broken rather than because
    the reaper works.
  - `spawn_detached_reaped_leaves_no_zombie`
  - `reaper_handles_many_children_without_leaking` - 25 children, because a
    reaper tracking only one child would pass the single-child test and still
    leak in production.
- `crates/jcode-base/src/hooks.rs`:
  `observer_dispatch_does_not_leak_zombies` - drives the real
  `dispatch_observer` path 30 times and asserts OS-visible process state, not
  that a helper was called. The defect is invisible in code shape *and* in the
  hook's own observable behaviour.
- Meta-tested: reverting `hooks.rs` to `spawn_detached` makes that test report
  `30 observer hook dispatches leaked 30 zombie(s)`. Reverting
  `spawn_detached_reaped` to `drop(child)` turns both platform tests red.
- End to end on the real binary with the real config, hook instrumented to log
  each invocation so the run could not pass by silently not firing:
  **54 hook processes spawned across 22 tool-calling turns, 0 zombies.** The
  pre-fix server measured alongside it sat at 149 zombies in 38 minutes.

## Note on verification, because it bit twice here

Two checks returned clean, plausible, wrong answers while fixing this:

1. An early E2E run reported **0 zombies** from a server whose hooks had never
   fired: the env-var hook config was overridden by the config file. Zero was
   true and meaningless. Fixed by making the hook append to a log and asserting
   the log was non-empty *before* trusting the zombie count. A leak test that
   does not prove the spawn happened is not a test.
2. A fixed test "failed" four times deterministically because restoring a file
   with `mv file.bak file` preserved the old mtime, so cargo never rebuilt and
   kept running the planted-defect binary. `touch` after any restore, and
   distrust a result that flips between a single-test run and a suite run.
