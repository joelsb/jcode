# 0029 - A zombie answers `kill(pid, 0)`, so liveness needs the process state, not signal deliverability

**Status:** Accepted
**Date:** 2026-08-31

## Decision

`platform::is_process_running()` treats a zombie as **not running**. On Unix it still starts with `kill(pid, 0)`, but a success is now qualified by `platform::process_is_zombie()`.

On macOS that probe is `sysctl(KERN_PROC_PID)` reading `p_stat` out of the returned `kinfo_proc` at hardcoded offsets, guarded by a runtime check that the `p_pid` the kernel reports matches the pid asked for. On Linux it is field 3 of `/proc/<pid>/stat`, split at the **last** `)`. Other Unixes answer `false` and keep the old behavior.

Separately, `ensure_menubar_helper_running()` now hands its spawned helper to `platform::reap_child_in_background()` instead of dropping the `Child`, which is what created the zombie in the first place.

## What forced it

Verifying the new `features.menubar` toggle, the positive control failed: with `menubar = true` and a clean pty session, no helper spawned. That reads as "the config is being ignored", and the natural next move is to go debug the config plumbing, which was fine.

The real cause was in `~/.jcode/menubar.pid`, holding pid 79587. `ps -p 79587` showed `<defunct>`: the helper had been killed earlier, and its parent (a `setsid` detached session started by an earlier jcode run) never reaped it. `kill(79587, 0)` returns 0 for a zombie, because the pid still exists in the process table. `is_process_running` therefore said "alive", and `ensure_menubar_helper_running` skipped the spawn.

The failure is permanent and silent. One crashed helper suppresses the menu bar indicator forever, across every future session, with a pid file that looks perfectly valid and a process that `kill -0` says is there. Nothing in a log says anything is wrong.

This is a general defect in the pid-file singleton pattern, not a menu bar quirk. The same `is_process_running` backs the server registry, ambient persistence, browser sessions, crash recovery, and background task reconciliation.

## Consequences including costs

- Any pid-file singleton on this codebase now recovers from a killed-and-unreaped helper on the next run instead of staying wedged.
- `process_is_zombie` is a syscall on the session startup path. `sysctl` on a single pid is a table lookup, not a process scan, and it only runs when `kill(pid, 0)` already succeeded.
- **The macOS path hardcodes `kinfo_proc` offsets** (`p_stat` at 36, `p_pid` at 40, struct size 648, verified against `offsetof` on arm64). This looks exactly like the kind of thing to "clean up" by switching to a typed binding. See the rejected alternative below before doing that. The pid cross-check exists so a layout change degrades to the old conservative answer rather than reading a random byte.
- A process that is genuinely gone in a way `kill` cannot see (permissions) still falls through to the old `ESRCH` logic, unchanged.

## Rejected alternatives

**`proc_pidinfo(PROC_PIDTBSDINFO)` and compare `pbi_status` to `SZOMB`.** This is the obvious macOS API, `libc` has bindings for both the struct and the constant, and it is what the first implementation used. **It does not work: `proc_pidinfo` refuses zombies**, returning 0 with `errno == ESRCH`, which is indistinguishable from "no such process". Measured directly with a C probe on 2026-08-31:

```
PROC_PIDTBSDINFO   ret=0 (size=136) errno=3 status=0 SZOMB=5
PROC_PIDTASKALLINFO ret=0 status=0
sysctl             r=0 len=648 p_stat=5 SZOMB=5
```

So the typed, well-bound API answers `false` for every zombie, which is exactly the bug being fixed, and it does so *quietly*. `libc` ships no Apple `kinfo_proc` binding, so `sysctl` with validated offsets is the only route that actually sees the state.

**Shelling out to `ps -o stat=`.** Correct, and used in the tests as an independent oracle. Rejected in production because it forks a process on the startup path of every session, to answer a question about whether we are leaking processes.

**Deleting the pid file when the check fails.** Treats the symptom at one call site and leaves every other `is_process_running` caller believing a corpse is alive.

**`waitpid(pid, WNOHANG)` to reap before checking.** Only works when the caller is the parent. The whole point of a pid-file singleton is that a *different, later* process reads the pid, and it can never reap another process's child.

## What would make us revisit

- `libc` gaining an Apple `kinfo_proc` binding: replace the offsets with the typed struct, keeping the pid cross-check.
- The `p_pid` guard firing in practice (a macOS release shifting the struct), which would show up as the menu bar indicator silently regressing to the wedged behavior.
- A caller that legitimately wants "this pid exists in the table, zombie or not". None exists today; it would need its own function, not a flag on this one.

## Evidence

- `crates/jcode-base/src/platform.rs` - `is_process_running`, `process_is_zombie`.
- `crates/jcode-base/src/platform_tests.rs` - `is_process_running_rejects_a_zombie_that_still_answers_kill_zero` asserts `kill(pid, 0) == 0` on the zombie *first*, so the test cannot pass by the corpse simply being gone. The shared `process_is_zombie` test helper cross-checks the shipped probe against `ps` on every call and fails loudly on disagreement; that assert is what caught the `proc_pidinfo` version, which otherwise passed as a silent `false`.
- `src/cli/commands/menubar.rs` - `ensure_menubar_helper_running` hands the child to the reaper.
- Observed instance: pid 79587, `<defunct>`, suppressing the menu bar helper across sessions on 2026-08-31.
- Related: [0026](0026-a-fire-and-forget-child-must-still-be-reaped.md), which fixed zombie *creation* for hook spawns. This ADR covers the other half: code that *reads* a pid must not trust `kill(pid, 0)` alone.
