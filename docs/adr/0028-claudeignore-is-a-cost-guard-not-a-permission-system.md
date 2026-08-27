# 0028 - .claudeignore is a token-cost guard, not a permission system

Date: 2026-08-27
Status: Accepted

## Decision

jcode honours a project-level ignore file (`.jcodeignore`, else `.claudeignore`) in three tools: `read`, `ls`, and `agentgrep` in its `grep` and `find` modes. `bash` is deliberately **not** gated by it.

## What forced it

Two things, one measured and one structural.

**The cost is per-turn, not per-read.** A conversation re-sends its whole transcript on every turn, so a file read once is billed on every subsequent turn of that session. Measured across 396 sessions on this machine on 2026-08-27: calls made at 400k+ context accounted for **44% of all weighted token spend**, and a single session reached 790k context and cost ~9.4M weighted tokens. A read is therefore the most expensive thing a tool can do, and it had no user-controllable guard.

**The only prior guard was unreachable.** `crates/jcode-app-core/src/tool/ls.rs` carried a hardcoded `DEFAULT_IGNORE` list (`node_modules`, `target`, `dist`, ...). No user could extend it, and no other tool consulted it, so `read` and `agentgrep` would happily walk into a vendored tree that `ls` hid.

## Why `bash` is exempt

This is the load-bearing decision, and the one most likely to be "fixed" by someone later.

Gating `bash` would turn a cost guard into a permission system, which would require intercepting every subprocess that can open a file (`cat`, `grep`, `python`, `awk`, a Makefile, a test runner). That is unbounded work with a false sense of completeness at the end of it, because the first uncovered path silently re-opens the hole.

More importantly it would be the wrong promise. A user who types `cat vendor/big.json` means it. The failure mode this ADR addresses is the **agent** wandering into expensive files on its own initiative, not a human deliberately reading one.

So the guarantee is deliberately narrow and stated in the module docs: **an ignore file changes what the agent reads by default, and never claims a file is unreachable.** Secrets are protected by not being on disk in readable form (Doppler, per the DAIN rules), never by an ignore file. `refusal_message` names `bash` as the documented escape hatch precisely so nobody mistakes the guard for a boundary.

## Why gitignore syntax, from the `ignore` crate

Search runs through ripgrep (via the external `agentgrep` crate) and reads run through this module. Two different pattern matchers would produce a directory hidden from `grep` but readable by `read`, which is a more confusing state than no guard at all. Using the same crate ripgrep uses means a pattern behaves identically in both. The crate was already in `Cargo.lock` transitively, so this added no new dependency tree.

## Why a post-filter for search rather than an `rg` flag

`GrepArgs` belongs to the external `agentgrep` crate and has no field for an extra ignore file, so passing `--ignore-file` would mean forking it. Filtering returned paths costs one match per result, negligible against the search itself, and it keeps one matcher as the single source of truth. Note ripgrep **already** honours `.gitignore` and `.ignore` on its own; this only adds the jcode-specific files on top.

## Consequences

- A read of an ignored path fails with a message naming both the file and the ignore file that excluded it. A bare refusal would send the reader hunting for an invisible rule.
- `ls` prunes ignored directories rather than merely hiding them, so the walk does not descend. Pruning is where the saving actually comes from on a large vendored tree.
- Ignore files are cached per working directory and recompiled on mtime/size change, so editing `.claudeignore` takes effect on the next tool call with no restart. Without this, users conclude the feature is broken.
- Precedence is first-match, **not merge**: `.jcodeignore` wins outright if present. Merging two files written for different tools produces a union nobody authored.
- Absolute paths outside the working directory are never ignored. An ignore file describes one project tree; applying its patterns to `/etc` or a sibling repo would break reads the user asked for explicitly.
- A malformed line is skipped while the rest of the file still applies, so one typo cannot silently disable the whole guard.

## Rejected alternatives

- **Gate `bash` too.** See above. Unbounded surface, and it would promise a security property this cannot deliver.
- **Reuse `.gitignore` only.** The two express different intents. `target/` is correctly gitignored *and* correctly unreadable, but `CHANGELOG.md` is committed and still must not be read, while a local uncommitted note may be gitignored yet worth reading. Conflating them means one file cannot say both things.
- **Config-file list in `config.toml`.** Would not travel with the repo, so every clone and every teammate would need to re-declare it. Project policy belongs in the project.
- **Merge `.jcodeignore` and `.claudeignore`.** Unpredictable union of two files with different authors and intents.
- **Extend `DEFAULT_IGNORE` in `ls`.** Fixes nothing for `read`, which is the tool that actually costs tokens.

## What would make us revisit

The `agentgrep` crate gaining an ignore-file argument, at which point the search post-filter can become a flag and the walk gets cheaper. Or a genuine requirement for an agent-proof read boundary, which would be a different feature with a different name, and would have to start by enumerating subprocess file access rather than reusing this module.

## Evidence

- Module and tests: `crates/jcode-tool-core/src/agent_ignore.rs`, 10 unit tests covering directory patterns, glob + negation, precedence, live edit, deletion, out-of-tree absolute paths, and the refusal message. `cargo test -p jcode-tool-core agent_ignore`: 10 passed, 0 failed.
- Enforcement points: `tool/read.rs` (before `path.exists()`), `tool/ls.rs` (`collect_entries`, prunes the walk), `tool/agentgrep.rs` (`filter_grep_result_to_ignored`, `filter_find_result_to_ignored`).
- Prior state confirmed absent: `agentgrep` for `claudeignore|jcodeignore|\.gitignore` across all `.rs` files returned 0 matches in 0 files before this change.
- ripgrep behaviour verified directly in a scratch repo: a pattern in `.ignore` excluded a planted needle, and the same pattern in `.claudeignore` did **not**, which is what made a jcode-side matcher necessary.
- Cost figures from `~/.jcode/sessions/*.journal.jsonl` usage records, weighting cache reads at 0.1x, cache writes at 1.25x and output at 5x input.
