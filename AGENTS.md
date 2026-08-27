# Repository Guidelines

## Development Workflow

- **Never reload without explicit approval.** Do not run `selfdev reload`,
  `selfdev build-reload`, `jcode server stop`, or anything else that restarts the
  daemon or swaps the running binary until Joel has said yes to that specific
  reload. Building is fine and needs no approval; *activating* the build is not.

  A reload restarts the process serving Joel's live session. It interrupts
  whatever he is mid-way through, kills in-flight background tasks (a `bg wait`
  returns "interrupted by server reload" and the underlying cargo run is
  orphaned, still holding the compile lock), and can leave the session marked
  "processing" so every following turn is rejected with "Already processing a
  message". A finished feature is not worth a destroyed session.

  Ask, naming it: "build is ready, reload now?" Then wait. If the work needs a
  reload to be verifiable, say so and stop there - hand back the state, do not
  reload to prove it yourself.

- **Stay on your own branch** - Do not take, cherry-pick, merge, or copy code from other
  people's or other agents' branches unless the source branch belongs to a repository
  maintainer and the user explicitly asks you to integrate it. Only work from your branch
  and its base (e.g. `main`) otherwise. Never integrate branches owned by non-maintainers
  or other agents yourself; tell the user and let them decide how to proceed.

- **New feature/fix workflow** - Always branch off `mine`, never off `master` directly:
  1. Update first: `git checkout master && git pull origin master`
  2. Rebase mine on top: `git checkout mine && git rebase master`
  3. Create feature branch from mine: `git checkout -b feat/my-feature mine`
  4. Do the work, commit
  5. Merge back into mine: `git checkout mine && git merge feat/my-feature`
  6. Delete feature branch: `git branch -d feat/my-feature`

  This ensures every new feature starts from latest upstream + all DAIN patches.
  The `/update` skill handles steps 1-2 automatically.

## Install Notes
- `~/.local/bin/jcode` is the launcher symlink used from `PATH`.
- `~/.jcode/builds/current/jcode` is the active local/source-build channel; self-dev builds and `scripts/install_release.sh` point the launcher here.
- `~/.jcode/builds/stable/jcode` is the stable release channel; `scripts/install.sh` installs this and points the launcher here.
- `~/.jcode/builds/versions/<version>/jcode` stores immutable binaries.
- `~/.jcode/builds/canary/jcode` still exists for canary/testing flows, but it is not the primary self-dev install path.
- On Windows, the equivalents are `%LOCALAPPDATA%\\jcode\\bin\\jcode.exe` for the launcher, `%LOCALAPPDATA%\\jcode\\builds\\stable\\jcode.exe` for stable, and `%LOCALAPPDATA%\\jcode\\builds\\versions\\<version>\\jcode.exe` for immutable installs; `scripts/install.ps1` currently installs the stable channel.
- Ensure `~/.local/bin` is **before** `~/.cargo/bin` in `PATH`.

## `.claudeignore`

The repo root carries a `.claudeignore` listing paths the agent must not read
(build output, lock files, vendored trees, binaries, fixture corpora). Gitignore
syntax. `.jcodeignore` wins if present; the two are never merged.

Enforced by `read`, `ls`, and `agentgrep` in `grep`/`find` modes. **Not** by
`bash`, deliberately: this is a token-cost guard, not a permission system, and
`cat` remains the documented escape hatch. Secrets belong in Doppler, never
behind an ignore file. See `docs/adr/0028`.

Two consequences worth holding:

- Search and listing report what they withheld ("suppression, not absence"). A
  short `ls` or an empty grep in a repo with an ignore file is not evidence the
  file does not exist. Re-check with `bash grep` before concluding.
- Editing `.claudeignore` takes effect on the next tool call, no restart needed.

## Verifying a change at runtime

`cargo build` alone proves nothing about behavior. `jcode run` and interactive
sessions are served by the long-lived daemon at
`~/.jcode/builds/shared-server/jcode`, which is a symlink into
`~/.jcode/builds/versions/<version>/`. Until that symlink is repointed and the
daemon restarted (`jcode self-dev --build`), a freshly built binary is inert and
every runtime check silently measures the old code.

To test a change without disturbing the shared daemon or the caller's session,
run your build against its own socket:

```bash
cargo build --profile selfdev
./target/selfdev/jcode run --no-update --socket /run/user/1000/jcode-mytest.sock '<prompt>'
```

Two things that waste time otherwise:

- `crate::logging::info` writes to a log file, not stderr, so instrumenting a
  code path with it produces no visible output under `--trace`. Use `eprintln!`
  for throwaway diagnostics and delete it before committing.
- Confirm which binary you are actually inspecting. `strings` on
  `builds/shared-server/jcode` reads a 70-byte symlink, not a program; resolve it
  with `readlink -f` first.

## DAIN Custom Patches

All DAIN patches live on the `mine` branch, rebased on `master` when upstream updates.
Workflow: `git checkout master && git pull`, then `git checkout mine && git rebase master`, then build+install.

- **Account-first failover**: When rate-limited, try same model on next OAuth account before downgrading to a smaller model. See `docs/DAIN_ACCOUNT_FIRST_FAILOVER.md`.
- **Configuration reference**: Our config, aliases, accounts. See `docs/DAIN_CONFIGURATION_REFERENCE.md`.

## Build Shortcut

```bash
source "$HOME/.cargo/env"
cargo build --release
bash scripts/install_release.sh --fast
```

## Installing a tool or plugin (Joel's machine)

Applies where this checkout lives under `~/MYNE/Projects/tools/`.

Installing a jcode plugin, a Herdr plugin, or any other tool that has a git repository is not finished when the installer succeeds. An installer drops a prebuilt binary into a state directory (`~/.local/state/herdr/plugins/` for Herdr plugins) that holds no source, no upstream remote and no history, so the next session cannot read the tool's code, diff it against upstream, or patch it.

So in the same piece of work, also:

1. Clone the upstream repo to `~/MYNE/Projects/tools/<name>/`.
2. Add `/<name>/` to `~/MYNE/Projects/tools/.gitignore`. Each tool stays its own upstream checkout and is never vendored into the `tools` repo.
3. Add its row to `~/MYNE/Projects/tools/TOOLS.md`: tool, dir, upstream, local branch, what it is, and how it is installed (plugin id, binary path).
4. Run `bash ~/MYNE/Projects/tools/scripts/tools-status.sh` and require `ok`.

A tool installed but absent from `TOOLS.md` is invisible to the next session, which is how a plugin gets reinstalled, or debugged with no source to read. The full rules for that registry live in `~/MYNE/Projects/tools/AGENTS.md`.

Official docs: https://jcode.sh/docs#configuration
