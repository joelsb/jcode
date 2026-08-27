//! `.claudeignore` support: paths the agent must not read.
//!
//! # Why this exists
//!
//! Every file the agent reads is billed as input tokens on every subsequent
//! turn of the conversation, because the whole transcript is re-sent each turn.
//! A single accidental read of a 200 KB lock file is therefore not a one-off
//! cost, it is a tax on the rest of the session. Before this module the only
//! protection was a hardcoded list in the `ls` tool
//! (`node_modules`, `target`, `dist`, ...) which no user could extend and
//! which no other tool consulted.
//!
//! # Why the file is named `.claudeignore`
//!
//! Compatibility with the convention users already have on disk. jcode also
//! reads `.jcodeignore` first, so a project can express a jcode-specific
//! policy without disturbing another tool's file.
//!
//! # What this is NOT
//!
//! This is a **token-cost guard, not a security boundary.** `bash` can still
//! `cat` an ignored file, and that is deliberate: making this a permission
//! system would mean auditing every subprocess, and a user who types
//! `cat secrets.env` means it. Secrets are handled by not having them on disk
//! in readable form (Doppler), not by an ignore file.
//!
//! # Semantics
//!
//! Gitignore syntax, via the same `ignore` crate ripgrep uses, so a pattern
//! behaves identically here and under `rg`. That matters because search runs
//! through ripgrep and reads run through this module: two different matchers
//! would mean a directory hidden from `grep` but readable by `read`, which is
//! the confusing half-state this module exists to avoid.
//!
//! Absolute paths outside the working directory are never ignored. The ignore
//! file describes one project tree; applying its patterns to `/etc` or to
//! another repo would be surprising and would break reads the user explicitly
//! asked for by absolute path.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Ignore-file names, in precedence order. The first one that exists wins;
/// they are not merged, so a project that ships both gets predictable
/// behaviour rather than the union of two files written for different tools.
pub const IGNORE_FILE_NAMES: &[&str] = &[".jcodeignore", ".claudeignore"];

/// A compiled ignore matcher for one working directory.
struct Matcher {
    root: PathBuf,
    /// `None` when the directory has no ignore file: nothing is ignored, and
    /// we still cache that fact so we do not stat the file on every read.
    gitignore: Option<ignore::gitignore::Gitignore>,
    /// Mtime+len of the source file when compiled, so an edit is picked up
    /// without restarting the session. `None` when there was no file.
    stamp: Option<(std::time::SystemTime, u64)>,
    source: Option<PathBuf>,
}

fn cache() -> &'static Mutex<Vec<Matcher>> {
    static CACHE: OnceLock<Mutex<Vec<Matcher>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

/// Locate the ignore file for `root`, honouring `IGNORE_FILE_NAMES` order.
pub fn ignore_file_for(root: &Path) -> Option<PathBuf> {
    IGNORE_FILE_NAMES
        .iter()
        .map(|name| root.join(name))
        .find(|candidate| candidate.is_file())
}

fn stamp_of(path: &Path) -> Option<(std::time::SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

fn build(root: &Path) -> Matcher {
    let source = ignore_file_for(root);
    let Some(ref file) = source else {
        return Matcher {
            root: root.to_path_buf(),
            gitignore: None,
            stamp: None,
            source: None,
        };
    };

    let stamp = stamp_of(file);
    let mut builder = ignore::gitignore::GitignoreBuilder::new(root);
    // A malformed line is skipped by the builder and surfaced as an error. We
    // keep the patterns that did parse rather than discarding the whole file:
    // one typo should not silently turn the guard off entirely.
    let _ = builder.add(file);
    let gitignore = builder.build().ok();

    Matcher {
        root: root.to_path_buf(),
        gitignore,
        stamp,
        source,
    }
}

/// Whether `path` is excluded by the ignore file of `working_dir`.
///
/// Returns `false` when there is no working directory, no ignore file, or the
/// path lies outside the working directory.
pub fn is_ignored(working_dir: Option<&Path>, path: &Path) -> bool {
    ignored_by(working_dir, path).is_some()
}

/// Like [`is_ignored`] but returns the ignore file that made the decision, so
/// a caller can tell the user *which* file to edit instead of just refusing.
pub fn ignored_by(working_dir: Option<&Path>, path: &Path) -> Option<PathBuf> {
    let root = working_dir?;

    // Resolve both sides before comparing. Without this a `..` segment or a
    // symlinked working directory produces a path that is inside the tree but
    // does not textually start with `root`, so the guard would silently pass.
    let root_c = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root_c.join(path)
    };
    let abs_c = std::fs::canonicalize(&abs).unwrap_or(abs);

    let rel = abs_c.strip_prefix(&root_c).ok()?;
    if rel.as_os_str().is_empty() {
        return None;
    }

    let mut guard = cache().lock().ok()?;
    let idx = match guard.iter().position(|m| m.root == root_c) {
        Some(i) => {
            // Recompile when the file changed, was created, or was deleted, so
            // editing `.claudeignore` takes effect on the next tool call.
            let current = guard[i].source.as_deref().and_then(stamp_of);
            let path_now = ignore_file_for(&root_c);
            if current != guard[i].stamp || path_now != guard[i].source {
                guard[i] = build(&root_c);
            }
            i
        }
        None => {
            guard.push(build(&root_c));
            guard.len() - 1
        }
    };

    let matcher = &guard[idx];
    let gitignore = matcher.gitignore.as_ref()?;
    let is_dir = abs_c.is_dir();
    if gitignore.matched_path_or_any_parents(rel, is_dir).is_ignore() {
        return matcher.source.clone();
    }
    None
}

/// The refusal message shown when a tool declines to read an ignored path.
///
/// Names the offending file and the ignore file that excluded it, because a
/// bare "not allowed" sends the reader hunting for a rule they cannot see.
pub fn refusal_message(path: &str, ignore_file: &Path) -> String {
    let name = ignore_file
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| ".claudeignore".to_string());
    format!(
        "Refusing to read {path}: excluded by {name}. \
         This guard exists to keep large or irrelevant files out of the context window, \
         where they would be re-billed on every later turn. \
         If you genuinely need this file, remove the pattern from {} or read it via bash.",
        ignore_file.display()
    )
}

/// Drop cached matchers. Tests use this; production relies on mtime checks.
pub fn clear_cache() {
    if let Ok(mut guard) = cache().lock() {
        guard.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jcode-agentignore-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn no_ignore_file_ignores_nothing() {
        clear_cache();
        let dir = tmpdir("none");
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        assert!(!is_ignored(Some(&dir), Path::new("a.txt")));
    }

    #[test]
    fn directory_pattern_excludes_contents() {
        clear_cache();
        let dir = tmpdir("dir");
        std::fs::create_dir_all(dir.join("vendor/deep")).unwrap();
        std::fs::write(dir.join("vendor/deep/f.txt"), "x").unwrap();
        std::fs::write(dir.join("keep.txt"), "x").unwrap();
        std::fs::write(dir.join(".claudeignore"), "vendor/\n").unwrap();

        assert!(is_ignored(Some(&dir), Path::new("vendor/deep/f.txt")));
        assert!(!is_ignored(Some(&dir), Path::new("keep.txt")));
    }

    #[test]
    fn glob_and_negation_behave_like_gitignore() {
        clear_cache();
        let dir = tmpdir("glob");
        std::fs::write(dir.join("a.lock"), "x").unwrap();
        std::fs::write(dir.join("keep.lock"), "x").unwrap();
        std::fs::write(dir.join(".claudeignore"), "*.lock\n!keep.lock\n").unwrap();

        assert!(is_ignored(Some(&dir), Path::new("a.lock")));
        assert!(
            !is_ignored(Some(&dir), Path::new("keep.lock")),
            "negation must re-include, as in gitignore"
        );
    }

    #[test]
    fn jcodeignore_takes_precedence_over_claudeignore() {
        clear_cache();
        let dir = tmpdir("prec");
        std::fs::write(dir.join("only_claude.txt"), "x").unwrap();
        std::fs::write(dir.join("only_jcode.txt"), "x").unwrap();
        std::fs::write(dir.join(".claudeignore"), "only_claude.txt\n").unwrap();
        std::fs::write(dir.join(".jcodeignore"), "only_jcode.txt\n").unwrap();

        // Not merged: the jcode file wins outright.
        assert!(is_ignored(Some(&dir), Path::new("only_jcode.txt")));
        assert!(!is_ignored(Some(&dir), Path::new("only_claude.txt")));
    }

    #[test]
    fn absolute_path_outside_working_dir_is_never_ignored() {
        clear_cache();
        let dir = tmpdir("outside");
        let other = tmpdir("outside-other");
        std::fs::write(dir.join(".claudeignore"), "*.txt\n").unwrap();
        let far = other.join("x.txt");
        std::fs::write(&far, "x").unwrap();

        assert!(
            !is_ignored(Some(&dir), &far),
            "an ignore file describes its own tree only"
        );
    }

    #[test]
    fn no_working_dir_ignores_nothing() {
        clear_cache();
        assert!(!is_ignored(None, Path::new("/etc/hosts")));
    }

    #[test]
    fn edit_to_ignore_file_takes_effect_without_restart() {
        clear_cache();
        let dir = tmpdir("reload");
        std::fs::write(dir.join("f.txt"), "x").unwrap();
        std::fs::write(dir.join(".claudeignore"), "nothing_here\n").unwrap();
        assert!(!is_ignored(Some(&dir), Path::new("f.txt")));

        // Same length would leave (mtime, len) unchanged on a coarse clock, so
        // this pattern is deliberately a different length.
        std::fs::write(dir.join(".claudeignore"), "f.txt\nplus_more\n").unwrap();
        assert!(
            is_ignored(Some(&dir), Path::new("f.txt")),
            "a live edit must be picked up, or users will think the file is broken"
        );
    }

    #[test]
    fn deleting_ignore_file_re_enables_reads() {
        clear_cache();
        let dir = tmpdir("delete");
        std::fs::write(dir.join("f.txt"), "x").unwrap();
        std::fs::write(dir.join(".claudeignore"), "f.txt\n").unwrap();
        assert!(is_ignored(Some(&dir), Path::new("f.txt")));

        std::fs::remove_file(dir.join(".claudeignore")).unwrap();
        assert!(!is_ignored(Some(&dir), Path::new("f.txt")));
    }

    #[test]
    fn refusal_message_names_the_file_and_the_rule_source() {
        let msg = refusal_message("vendor/big.json", Path::new("/proj/.claudeignore"));
        assert!(msg.contains("vendor/big.json"));
        assert!(msg.contains(".claudeignore"));
        assert!(msg.contains("bash"), "must state the documented escape hatch");
    }

    #[test]
    fn ignored_by_reports_which_file_decided() {
        clear_cache();
        let dir = tmpdir("which");
        std::fs::write(dir.join("f.txt"), "x").unwrap();
        std::fs::write(dir.join(".claudeignore"), "f.txt\n").unwrap();
        let who = ignored_by(Some(&dir), Path::new("f.txt")).expect("should be ignored");
        assert_eq!(who.file_name().unwrap(), ".claudeignore");
    }
}
