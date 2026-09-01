# Bug: an account rotation does not reach sessions in other processes

Reported by Joel 2026-09-01 with a screenshot: one session showed
`⚡ Rate limited; switched Anthropic account: claude-1 → claude-2 [×2]`
and moved on, while other sessions did not follow.

Status: diagnosed, not fixed. No code changed for this yet.

## What is actually happening

The rotation announcement travels on `Bus::global()`, which is a
**process-local** `tokio::broadcast` channel:

```rust
// crates/jcode-base/src/bus.rs:516
pub fn global() -> &'static Bus {
    static INSTANCE: OnceLock<Bus> = OnceLock::new();
    INSTANCE.get_or_init(Bus::new)
}
```

`client_lifecycle.rs:902` forwards `BusEvent::AnthropicAccountRotated` to every
client **of that daemon**, and the client then calls
`set_active_account_override` in its own process
(`server_events.rs:2402`). Both halves are per-process:

- `RUNTIME_ACTIVE_OVERRIDES` (`auth/account_store.rs:11`) is a process-global
  `LazyLock<RwLock<HashMap>>`, so the override exists only inside the process
  that received the event.
- A `OnceLock` broadcast channel cannot cross a process boundary at all.

So the blast radius of a rotation is exactly one OS process.

## Measured on this machine, 2026-09-01 14:12

Four live jcode processes, and only one of them is a daemon:

| PID | Role | Listens on |
|---|---|---|
| 76120 | `builds/stable/jcode serve --socket .../T/jcode.sock` | `jcode.sock`, `jcode-debug.sock` |
| 54361 | `jcode -m claude-opus-5` (binary `builds/versions/0aeff5b4a-dirty/jcode`) | nothing |
| 83963 | `jcode -m claude-opus-5` | nothing |
| 25824 | `builds/current/jcode --resume session_kangaroo_...` | nothing |

`lsof` shows **no shared unix socket peer address** between 54361, 83963, 25824
and the daemon 76120: each process's peer set is disjoint. They are not clients
of the daemon, they are independent processes each running their own agent, each
with its own `Bus::global()` and its own override map.

`~/.jcode/auth.json` currently has `active_anthropic_account = "claude-2"` and
labels `["claude-1", "claude-2"]`, and the log confirms the rotation was real:

```
12:13:22.642 Same-provider account failover: Anthropic rate-limited (... 429 ...)
12:13:22.666 Could not refresh usage for Anthropic account 'claude-2' after rotation
```

A process that was not the one handling that 429 never learns any of this.

## Why the existing ADRs do not cover it

- **ADR-0011** is about the opposite direction: a rotation reaching clients it
  should *not* affect, filtered client-side by provider
  (`ignores_anthropic_account_events`, `tui_state.rs:154`). Its consequences
  section says "Two Anthropic sessions on one daemon both adopt the override" -
  correct, and it is why single-daemon fan-out works today.
- **ADR-0009** defers per-session account identity.

Neither considers **multiple daemons / standalone processes**, which is the case
here. So this is a third, unrecorded case: cross-process propagation.

## Design question before any fix

The override is deliberately *not* persisted (`server_events.rs:2400`): "the
rotation lasts as long as the daemon says it does, and must not rewrite the
user's configured account". Any cross-process mechanism has to respect that, so
the obvious cheap fix - write the rotation to `auth.json` - is the one the
current design explicitly rejects.

Options, roughly by cost:

1. **A short-lived rotation hint file** under `~/.jcode/` (label + expiry),
   read when a process resolves the active account and ignored once stale.
   Keeps "not persisted" in spirit; needs a staleness rule nobody has picked.
2. **Route all sessions through one daemon** so the existing in-process fan-out
   is sufficient. Changes how Joel launches sessions; does not fix the general
   case.
3. **Accept per-process rotation** and only fix the reporting, so a session that
   did not rotate does not *claim* an account it is not using.

Not attempted: which of these is wanted is a product decision, and verifying any
of them needs two live processes plus one genuinely rate-limited account.

## How to verify a fix

Needs a real exhausted account, so it cannot be faked with a unit test alone:

1. Start two sessions in separate processes on the same Anthropic account.
2. Drive one to a 429 so it rotates.
3. Assert the second process's `/account` and usage widget report the new label.

A unit test can cover the mechanism (a rotation hint written by process A is
observed by a fresh resolution in process B), but the end-to-end claim needs
step 2 for real.
