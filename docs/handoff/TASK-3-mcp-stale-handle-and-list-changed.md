# TASK 3: a dead MCP child stays "connected" forever, and `tools/list_changed` is ignored

**Status: NOT STARTED. Nothing has been fixed. This file is the whole brief.**

**Branch:** `mine` (clean at `16b9a72f7` when this was written)
**Owner:** whoever picks this up. Self-contained; read this file only.
**Reported by:** Joel, 2026-08-26, from a live session that lost its Athena tools mid-task.

Two independent defects in `crates/jcode-base/src/mcp/`. They were found together because they hit the same session eight minutes apart, but they are separate fixes and can land separately.

---

## Defect A: a dead MCP child process keeps serving requests from a tombstone handle

### The symptom the user sees

Three consecutive MCP tool calls returned, instantly:

```
Error: Failed to send request
```

No timeout, no reconnect, no diagnostic. Every tool on that server, in every session sharing it, for the rest of the daemon's life. The user's only escape was `mcp action=reload`, which they had to guess at.

### Reproduction, deterministic

1. Start a jcode session with a shared stdio MCP server configured (e.g. `router` in `~/.jcode/mcp.json`).
2. Call one of its tools. It works.
3. From outside, kill the child: `pkill -f mcp-router/router.py`.
4. Call the same tool again.

Expected: an error naming the dead process, then a reconnect (or at minimum a reconnect on the following call).
Actual: `Error: Failed to send request`, forever, and the server is still listed as connected.

### The mechanism, traced

| Step | File:line | What happens |
|---|---|---|
| 1 | `client.rs:203-208` | The writer task does `stdin.write_all(...)`; on error it just `break`s out of the loop. Breaking drops the `mpsc::Receiver`. |
| 2 | `client.rs:41-44` | The next `writer_tx.send(msg).await` therefore returns `Err`, wrapped as `.context("Failed to send request")`. That is the user-visible string, character for character. |
| 3 | `pool.rs` `handles` map | The `McpHandle` is never removed. Nothing observes the writer task exiting. |
| 4 | `manager.rs:333-340` | `call_tool` takes the "fast path: already connected via pool handle" and returns the handle's error. Connect-on-first-call at `manager.rs:352` is unreachable, because the stale entry makes the server look connected. |

The load-bearing fact: **`McpClient::is_running()` is defined at `client.rs:327` and called from nowhere in the repo.**

```
$ rg -n '\.is_running\(\)' crates/jcode-base/src/mcp/
(no matches)
```

The liveness check was written and never wired up. That is the entire bug.

### Why this is worse than it looks: the shared pool

`pool.rs` is one process per server for the whole daemon (`SharedMcpPool`, `SHARED_POOL` singleton). So **one session killing an MCP server silently and permanently poisons every other live session** on that daemon.

That is exactly what happened on 2026-08-26. Verified from `~/.jcode/logs/jcode-2026-08-26.log` and the session transcripts:

| Time (local) | Event |
|---|---|
| 11:12:45 | session `chipmunk` runs `kill 83000` on the router, deliberately, to force an Atlassian OAuth restart |
| 16:01:38 | session `cat` calls `mcp__router__router_list` → `Error: Failed to send request` |
| 16:01:52 | same call, same error |
| 16:01:59 | `mcp__router__router_help` → same error |
| 16:02:06 | user-agent calls `mcp action=reload` |
| 16:02:07 | `ps` shows a **new** router process; everything works again |

`reload()` works because `pool.rs` `reload()` calls `disconnect_all()` first, which clears the `handles` map.

Note the daemon **did** log the router's stderr and **did** log `MCP: Connected to 'router' with 12 tools` on every reconnect - but logged nothing at all when the child died. There is no death event in the log to search for.

### What a fix has to do

Pick one; the first is smaller, the second is more honest.

1. **Mark the handle dead when the writer task exits.** Give `McpHandle` a shared `alive: Arc<AtomicBool>`; the writer task clears it on `break`, and `pool.rs`/`manager.rs` treat a not-alive handle as not-connected, falling through to the existing connect-on-first-call path.
2. **Check liveness before the fast path.** Call the already-written `is_running()` in `manager.rs::call_tool` and `pool.rs::call_tool`, evicting the handle when it returns false. Costs a `try_wait()` per call, and needs the `&mut` to be reachable from the pool (the `McpClient` lives in `pool.clients`, the handle in `pool.handles`).

Either way: **evict the handle, log the death, and let the next call reconnect.** A user should never have to know `mcp action=reload` exists.

Also worth fixing while you are here: the error string. `Failed to send request` names the mpsc channel, not the world. `MCP server 'router' is not running (child process exited); reconnecting` tells the user and the model what actually happened.

### Tests that would have caught it

- Spawn a real stdio MCP child, kill it, call a tool. Assert the error names the dead process, and assert the **second** call succeeds against a reconnected child. Asserting only on the first call's error text would pass against a version that never reconnects.
- Assert `connected_servers()` no longer lists the server once its child is gone. Today it does, which is the lie the fast path believes.
- Plant the defect back (comment out the eviction) and confirm the test fails. A test written against this bug can easily pass for the wrong reason, because both the broken and fixed paths return `Err` on the first call.

---

## Defect B: `notifications/tools/list_changed` is parsed away and dropped

### The symptom

Mid-session, the user's agent called `mcp__router__router_configure` to expose six Athena tools. The router accepted it and replied:

```
Updated 'athena': expose=[...]. 6 tool(s) now visible: check_connection, ...
```

The very next call to `check_connection` returned:

```
Error: Unknown tool: check_connection. Available tools: agentgrep, apply_patch, bash, ...
```

The session's tool list was frozen at spawn. The workaround was to route every call through `mcp__router__router_batch`, which is an existing tool and therefore visible. That worked, but it is a workaround for a protocol feature jcode declines to implement.

### The mechanism

The server does its half correctly. In `~/tools/mcp-router/router.py`:

- `router.py:1792-1793` declares `ToolsCapability(listChanged=True)` and `ResourcesCapability(listChanged=True)`.
- `router.py:1400` `_notify_tool_list_changed()` sends `notifications/tools/list_changed`, called from `router_configure` (`:1379`, `:1398`) and from hot-reload (`:1618`).
- Its own smoke test asserts the notification is emitted: `tests/test_stdio_smoke.py:56`.

jcode's side:

- `protocol.rs:107-122` has `list_changed` fields on the capability structs. They are **parsed and then never read**.
- `client.rs:224-231` - the reader task deserialises each line as `JsonRpcResponse` and dispatches only `if let Some(id) = response.id`. A JSON-RPC **notification has no `id`**, so it matches nothing and falls off the end of the branch. It is not even logged; the `else` arm is only for lines that fail to parse as JSON at all.
- `rg -n 'list_changed|listChanged' crates/` returns only `protocol.rs` and one test fixture. **There is no handler anywhere.**

### What a fix has to do

1. In the reader task, when `response.id` is `None`, treat the message as a notification and dispatch on `method`.
2. On `notifications/tools/list_changed`, call the existing `refresh_tools()` and propagate the new list to sessions holding that handle.
3. Same for `notifications/resources/list_changed` if resources are exposed.

The hard part is step 3 of the propagation, not the parse: sessions snapshot the tool list, and the *system prompt* carries the tool schemas. Changing the advertised tool list mid-session invalidates the prompt cache (see `#206`, and the `advertised N cached tool(s) at spawn` log line). Decide deliberately whether a refresh applies to the live turn or the next one, and write it down. That cost is real and is probably why this was skipped - but the current behaviour is worse, because `router_configure` reports success for something that cannot work.

### Test

Use a stub MCP server that emits `notifications/tools/list_changed` after a specific tool call, then assert the new tool name is callable **and** that the previously-cached tools still are. Assert on a *named* tool appearing, never on the list length changing.

---

## What was NOT done

Nothing was changed. No code, no tests, no config. `mine` is clean at `16b9a72f7` apart from an untracked `.local/`.

The only action taken on the day was `mcp action=reload` in the affected session, which restarted the router process (live PID started 16:02:07).

## Where the evidence lives

- Session transcript: `~/.jcode/sessions/session_cat_1787736825820_4d53308e47463c73.json` (and its `.journal.jsonl`), messages around the `Failed to send request` results.
- Daemon log: `~/.jcode/logs/jcode-2026-08-26.log`. Search `MCP: Connecting to 'router'` for the reconnects; note the **absence** of any line at 11:12:45 when the child was killed.
- The kill: session `chipmunk`, `~/.jcode/sessions/session_chipmunk_1787740092367_d252ab8077c45544.json`, timestamp `2026-08-26T11:12:45Z`, command `kill 83000`.
- Server side of Defect B: `~/tools/mcp-router/router.py` at the line numbers above.

## Priority argument

Defect A is the one to fix first. It is silent, it is cross-session, it has no log line, and its only cure is a command the user has no reason to know. Defect B is loud - the model gets a clear `Unknown tool` error and can route around it, as this session did.
