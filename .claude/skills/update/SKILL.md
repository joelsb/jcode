---
name: update
description: Pull latest upstream into master, rebase `mine` branch on top, resolve conflicts preserving DAIN patches, then build+install.
disable-model-invocation: true
---

# Update jcode from upstream

Pull origin/master and rebase the `mine` branch so DAIN patches always sit on top of upstream.

## Pre-flight

1. Run `git status` - if there are uncommitted changes, stash them with `git stash -u` and note it for post-flight.
2. Record current branch: `git rev-parse --abbrev-ref HEAD` - restore it at the end.

## Step 1 - Update master

```
git checkout master
git pull origin master
```

If pull fails (network, auth), stop and report the error. Do not continue.

## Step 2 - Rebase mine on top of master

```
git checkout mine
git rebase master
```

## Step 3 - Handle conflicts

If rebase produces conflicts:

1. For each conflicting file, read both sides of the conflict.
2. **DAIN patches win** - our code (the `mine` branch side, marked `HEAD` during rebase which is the replayed commit) takes priority. Upstream changes should be integrated only where they don't contradict our patches.
3. Key files where DAIN patches likely conflict:
   - Anything under `src/` that touches provider fallback, rate-limit handling, or account rotation
   - `AGENTS.md` / `CLAUDE.md` - keep our version, incorporate any new upstream sections that don't overlap
   - Config-related files - keep our additions, accept upstream structural changes
4. After resolving each file: `git add <file>`
5. Continue: `git rebase --continue`
6. Repeat until rebase completes.

If a conflict is too ambiguous to resolve automatically (e.g. upstream rewrote an entire module your patch touches), **abort the rebase** with `git rebase --abort`, report the situation, and let the user decide.

## Step 4 - Verify build

```bash
source "$HOME/.cargo/env"
cargo build --release
```

If build fails:
1. Read the compiler errors.
2. Fix issues caused by upstream API changes conflicting with DAIN patches.
3. Commit fixes as a new commit: `fix: resolve upstream compatibility after rebase`
4. Rebuild and confirm success.

## Step 5 - Install

```bash
bash scripts/install_release.sh --fast
```

## Post-flight

1. If changes were stashed in pre-flight, run `git stash pop`.
2. Restore the original branch if it was not `mine`.
3. Report: old master commit, new master commit, number of DAIN commits rebased, build status.

## Summary format

```
Updated master: <old-sha>..<new-sha> (<N> new upstream commits)
Rebased mine: <N> DAIN patches on top
Conflicts: none | resolved <N> files
Build: OK
Install: OK
```
