# Session Task Label Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every jcode session that has done real work shows a short 2-4 word task label as its terminal title, including sessions that never call the todo tool.

**Architecture:** A new `jcode-base` module `task_label` owns one sidecar file per session (`~/.jcode/labels/<session_id>.json`) holding `{label, source, source_hash}`. The label is produced with **zero extra provider calls** in two layers: a deterministic derivation from the latest substantive user message (always available, fixes the no-todo-tool case), and an optional model-written override carried on the **existing** todo-tool call as `plan.task_label` (better wording when the model is already writing a plan). The TUI refreshes the label at turn end, then the existing `update_terminal_title` resolution chain reads it. Herdr keeps reading the terminal title; no new channel, no protocol change.

**Tech Stack:** Rust 2021, `serde`/`serde_json`, existing `jcode_storage` helpers, `cargo test`.

**Spec:** `.local/prd/session-task-label-brief.md`

## Global Constraints

Copied from the brief; every task's requirements implicitly include these.

- **No extra provider call and no second model.** The only model-sourced path is a field on a todo-tool call the model was already making.
- **A failed or skipped label must never affect the turn.** No `?` propagation into turn code, no error surfaced to the user, no blocking IO the user waits on. Worst case is the current fallback title.
- **An explicit `/rename` still wins.** `session.custom_title` outranks anything generated.
- **Fits 48 chars with the icon.** Hard cap the label at **30 chars** (`task_label::MAX_LABEL_CHARS = 30`).
- **Not every session deserves a label.** Weak evidence produces `None` and the session keeps the animal-name fallback.
- **Client data never leaves the machine.** The label is derived locally from text already in the session file; nothing is shipped anywhere.

## Decisions the brief left open (answered here)

| Open question | Decision |
| --- | --- |
| When is it computed? | At turn end only, in the TUI, from the **latest substantive user message**. Short continuations (`ok`, `continue`, `yes`, slash commands) are not substantive, so the label sticks through a long task and only moves on a real topic change. A `source_hash` guard means an unchanged source writes nothing. |
| Where does the string live? | A sidecar file `~/.jcode/labels/<session_id>.json`, mirroring how `todo.rs` already keeps `~/.jcode/todos/<id>-plan.json`. **Not** a new `Session` field: `Session` is mirrored across `SessionJournalMeta`, `SessionStartupStub`, `RemoteStartupSessionSnapshot`, `apply_journal_meta`, the harness API and the SDK, so a field costs six edits and a protocol change for a value that is a display cache. Not `session.title` (collides with `import.rs`), not `custom_title` (breaks constraint 3). |
| How is it produced without a second call? | Layer 1 deterministic derivation (always). Layer 2 optional `plan.task_label` on the existing todo tool call. A model label is sticky: a derived label never overwrites it. |
| Does the todo path stay? | Yes, but demoted and explicitly ranked below the task label: `custom_title` > task label > `todo::load_session_title` > `session.title` > fallback. `load_session_title` stays because old sessions have no label file and the session picker (72-char titles) is a different consumer. There is exactly one writer per source, so they never race. |
| Herdr reads it how? | Unchanged: the OSC terminal title set by `update_terminal_title`. No second channel. |

## File Structure

- **Create** `crates/jcode-base/src/task_label.rs` - derivation (pure), sidecar storage, staleness guard. One responsibility: produce and persist a short label. ~250 lines including tests.
- **Modify** `crates/jcode-base/src/lib.rs` - register the module.
- **Modify** `crates/jcode-base/src/process_title.rs` - `terminal_display_title_for_id` gains the label ahead of the todo title.
- **Modify** `crates/jcode-tui/src/tui/app/tui_lifecycle_runtime.rs` - `update_terminal_title` resolution chain plus a new `refresh_task_label`.
- **Modify** `crates/jcode-tui/src/tui/app/local.rs` - call `refresh_task_label` in `finish_turn`.
- **Modify** `crates/jcode-tui/src/tui/app/remote/server_events.rs` - call it on the remote turn-complete path.
- **Modify** `crates/jcode-task-types/src/lib.rs`, `crates/jcode-app-core/src/tool/todo.rs` - optional `plan.task_label`.

---

### Task 1: Deterministic label derivation

**Files:**
- Create: `crates/jcode-base/src/task_label.rs`
- Modify: `crates/jcode-base/src/lib.rs:104` (add `pub mod task_label;` next to `pub mod todo;`)
- Test: inline `#[cfg(test)] mod tests` in `crates/jcode-base/src/task_label.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces:
  - `pub const MAX_LABEL_CHARS: usize = 30;`
  - `pub fn derive_label(user_texts: &[&str]) -> Option<String>` - newest message **last**.
  - `pub fn source_hash(text: &str) -> u64`

- [ ] **Step 1: Write the failing tests**

Create `crates/jcode-base/src/task_label.rs` with only the test module plus `use` lines, so the file compiles as a module and the tests fail on missing functions.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_a_short_label_from_a_plain_request() {
        assert_eq!(
            derive_label(&["Add retry logic to the KumoMTA bounce parser"]),
            Some("Retry Logic Kumomta Bounce".to_string())
        );
    }

    #[test]
    fn keeps_a_ticket_key_first_and_uppercase() {
        assert_eq!(
            derive_label(&["Implement the DI-2507 POST /api/chat/questions plan task-by-task with TDD"]),
            Some("DI-2507 Implement POST Plan".to_string())
        );
    }

    #[test]
    fn falls_back_to_a_path_stem_when_the_prose_is_all_stopwords() {
        assert_eq!(
            derive_label(&["read ~/MYNE/Projects/tools/jcode/.local/prd/session-task-label-brief.md and do it"]),
            Some("Read Session Task Label".to_string())
        );
    }

    #[test]
    fn ignores_continuations_and_keeps_the_last_substantive_message() {
        let texts = ["Rewrite the segment sync job", "continue", "ok", "yes go on"];
        assert_eq!(
            derive_label(&texts),
            Some("Rewrite Segment Sync Job".to_string())
        );
    }

    #[test]
    fn a_later_substantive_message_moves_the_label() {
        let texts = ["Rewrite the segment sync job", "now fix the Mautic webhook timeout"];
        assert_eq!(
            derive_label(&texts),
            Some("Fix Mautic Webhook Timeout".to_string())
        );
    }

    #[test]
    fn weak_evidence_produces_no_label() {
        assert_eq!(derive_label(&[]), None);
        assert_eq!(derive_label(&["hi"]), None);
        assert_eq!(derive_label(&["thanks!"]), None);
        assert_eq!(derive_label(&["/model opus"]), None);
    }

    #[test]
    fn the_label_never_exceeds_the_char_cap() {
        let label = derive_label(&[
            "Investigate deliverability regression across Microsoft Outlook inbox placement seeds",
        ])
        .expect("label");
        assert!(
            label.chars().count() <= MAX_LABEL_CHARS,
            "label {label:?} exceeded {MAX_LABEL_CHARS} chars"
        );
        assert!(!label.ends_with(' '));
    }

    #[test]
    fn the_source_hash_is_stable_and_discriminating() {
        assert_eq!(source_hash("same text"), source_hash("same text"));
        assert_ne!(source_hash("same text"), source_hash("other text"));
    }
}
```

Add the module registration in `crates/jcode-base/src/lib.rs`, immediately before `pub mod todo;`:

```rust
pub mod task_label;
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p jcode-base task_label`
Expected: FAIL, `cannot find function derive_label in this scope` (and the same for `source_hash`, `MAX_LABEL_CHARS`).

- [ ] **Step 3: Write the implementation**

Put this **above** the test module in `crates/jcode-base/src/task_label.rs`.

```rust
//! Short task labels for terminal window titles.
//!
//! Herdr's agent sidebar shows one row per pane and its top line is the
//! terminal title, so the title is an index into ten concurrent jobs rather
//! than decoration. `todo::load_session_title` only helps sessions that call
//! the todo tool, and it returns a prose sentence that gets clipped mid-word.
//! This module derives a 2-4 word label from the latest substantive user
//! message instead, with no provider call at all.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Hard cap. `process_title::terminal_window_title` truncates at 48 chars
/// including the icon, so anything longer is silently destroyed.
pub const MAX_LABEL_CHARS: usize = 30;

const MAX_LABEL_WORDS: usize = 4;
/// Below this many keyword-bearing words a message is not evidence of a task.
const MIN_KEYWORDS: usize = 2;

/// Words that carry no task identity. Deliberately small: over-filtering
/// produces confident wrong labels, which the brief rates worse than none.
const STOPWORDS: &[&str] = &[
    "a", "about", "actually", "add", "after", "all", "also", "an", "and", "any", "are", "as", "at",
    "back", "basically", "be", "been", "before", "being", "both", "but", "by", "can", "could",
    "did", "do", "does", "doing", "done", "each", "else", "etc", "even", "every", "first", "for",
    "from", "get", "go", "had", "has", "have", "he", "her", "here", "hers", "him", "his", "how",
    "i", "if", "in", "into", "is", "it", "its", "just", "let", "lets", "like", "make", "may", "me",
    "might", "more", "most", "much", "must", "my", "need", "needs", "new", "no", "not", "now",
    "of", "off", "ok", "okay", "on", "once", "one", "only", "or", "other", "our", "out", "over",
    "own", "per", "please", "really", "same", "see", "set", "shall", "she", "should", "so", "some",
    "still", "such", "sure", "take", "than", "that", "the", "their", "them", "then", "there",
    "these", "they", "thing", "things", "this", "those", "through", "to", "too", "try", "under",
    "up", "us", "use", "using", "very", "via", "want", "was", "way", "we", "well", "were", "what",
    "when", "where", "which", "while", "who", "why", "will", "with", "would", "yes", "yet", "you",
    "your", "yours",
];

/// A stable hash of the text a label was derived from, so an unchanged source
/// rewrites nothing.
pub fn source_hash(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.trim().hash(&mut hasher);
    hasher.finish()
}

/// Derive a short label from user message texts, newest **last**.
/// Returns `None` when the evidence is too weak to name the work.
pub fn derive_label(user_texts: &[&str]) -> Option<String> {
    let source = latest_substantive(user_texts)?;
    label_from_text(source)
}

/// The newest message that looks like a task rather than a continuation.
pub fn latest_substantive<'a>(user_texts: &[&'a str]) -> Option<&'a str> {
    user_texts
        .iter()
        .rev()
        .copied()
        .find(|text| is_substantive(text))
}

fn is_substantive(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.starts_with('/') {
        return false;
    }
    keywords(trimmed).len() >= MIN_KEYWORDS || !path_stem_words(trimmed).is_empty()
}

/// Ticket keys such as `DI-2507`. Kept verbatim and placed first: it is the
/// single most identifying token a DAIN task can carry.
fn ticket_key(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(trim_token)
        .find(|token| is_ticket_key(token))
        .map(|token| token.to_uppercase())
}

fn is_ticket_key(token: &str) -> bool {
    let Some((prefix, number)) = token.split_once('-') else {
        return false;
    };
    let prefix_ok = (2..=10).contains(&prefix.chars().count())
        && prefix.chars().all(|c| c.is_ascii_alphabetic());
    let number_ok = !number.is_empty() && number.chars().all(|c| c.is_ascii_digit());
    prefix_ok && number_ok
}

fn trim_token(token: &str) -> &str {
    token.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_' && c != '/' && c != '.')
}

fn is_pathish(token: &str) -> bool {
    token.contains('/') || token.contains('\\') || token.starts_with('~') || token.contains("://")
}

/// Identity-bearing words: not stopwords, not paths, not the ticket key.
fn keywords(text: &str) -> Vec<String> {
    first_clause(text)
        .split_whitespace()
        .map(trim_token)
        .filter(|token| {
            let lower = token.to_lowercase();
            token.chars().count() >= 2
                && !is_pathish(token)
                && !is_ticket_key(token)
                && token.chars().any(|c| c.is_alphabetic())
                && !STOPWORDS.contains(&lower.as_str())
        })
        .map(str::to_string)
        .collect()
}

/// A request whose prose is all stopwords ("read <path> and do it") still names
/// its work in the file it points at, so fall back to that file's stem.
fn path_stem_words(text: &str) -> Vec<String> {
    let Some(token) = first_clause(text)
        .split_whitespace()
        .map(trim_token)
        .find(|token| is_pathish(token))
    else {
        return Vec::new();
    };
    let stem = token
        .rsplit('/')
        .next()
        .unwrap_or(token)
        .split('.')
        .next()
        .unwrap_or("");
    stem.split(['-', '_'])
        .filter(|word| {
            word.chars().count() >= 2 && !STOPWORDS.contains(&word.to_lowercase().as_str())
        })
        .map(str::to_string)
        .collect()
}

/// Only the first clause names the task; everything after a sentence break or
/// a conjunction is qualification.
fn first_clause(text: &str) -> &str {
    let trimmed = text.trim();
    let end = trimmed
        .find(['.', '?', '!', '\n', ':', ';'])
        .unwrap_or(trimmed.len());
    let clause = trimmed[..end].trim();
    if clause.is_empty() { trimmed } else { clause }
}

fn title_case(word: &str) -> String {
    if word.chars().all(|c| !c.is_lowercase()) {
        return word.to_string();
    }
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
        None => String::new(),
    }
}

fn label_from_text(text: &str) -> Option<String> {
    let ticket = ticket_key(text);
    let mut words: Vec<String> = Vec::new();
    if let Some(ticket) = ticket {
        words.push(ticket);
    }
    let mut candidates = keywords(text);
    if candidates.len() < MIN_KEYWORDS {
        candidates.extend(path_stem_words(text));
    }
    for word in candidates {
        if words.len() >= MAX_LABEL_WORDS {
            break;
        }
        let cased = title_case(&word);
        if !words.iter().any(|existing| existing.eq_ignore_ascii_case(&cased)) {
            words.push(cased);
        }
    }
    if words.is_empty() {
        return None;
    }
    Some(clamp_words(&words))
}

/// Join under the char cap, dropping whole trailing words rather than clipping
/// one mid-word the way the old 48-char truncation did.
fn clamp_words(words: &[String]) -> String {
    let mut label = String::new();
    for word in words {
        let candidate = if label.is_empty() {
            word.clone()
        } else {
            format!("{label} {word}")
        };
        if candidate.chars().count() > MAX_LABEL_CHARS {
            break;
        }
        label = candidate;
    }
    if label.is_empty() {
        words[0].chars().take(MAX_LABEL_CHARS).collect()
    } else {
        label
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p jcode-base task_label`
Expected: PASS, 8 tests.

If `derives_a_short_label_from_a_plain_request` or the ticket test disagrees on wording, fix the **test expectation** to the actual output only after checking the output is a sensible 2-4 word label; do not weaken the char cap, the `None` cases, or the continuation test to make things pass.

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-base/src/task_label.rs crates/jcode-base/src/lib.rs
git commit -m "feat(task-label): derive a short task label from user messages"
```

---

### Task 2: Sidecar storage with a staleness guard

**Files:**
- Modify: `crates/jcode-base/src/task_label.rs`
- Test: inline `#[cfg(test)] mod tests` in the same file

**Interfaces:**
- Consumes: `derive_label`, `source_hash`, `latest_substantive`, `MAX_LABEL_CHARS` from Task 1.
- Produces:
  - `pub struct StoredTaskLabel { pub label: String, pub source: TaskLabelSource, pub source_hash: u64 }`
  - `pub enum TaskLabelSource { Derived, Model }` (serde `snake_case`)
  - `pub fn load(session_id: &str) -> Option<StoredTaskLabel>`
  - `pub fn load_label(session_id: &str) -> Option<String>`
  - `pub fn store_model_label(session_id: &str, label: &str) -> bool`
  - `pub fn refresh_derived(session_id: &str, user_texts: &[&str]) -> bool`

Every function is infallible from the caller's view: errors become `false`/`None`. Nothing returns `Result`, so no caller can accidentally `?` a label failure into a turn.

- [ ] **Step 1: Write the failing tests**

Append to the existing `mod tests` in `crates/jcode-base/src/task_label.rs`:

```rust
    use crate::storage::lock_test_env;

    struct TempHome {
        _dir: tempfile::TempDir,
        previous: Option<std::ffi::OsString>,
    }

    impl TempHome {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("temp dir");
            let previous = std::env::var_os("JCODE_HOME");
            crate::env::set_var("JCODE_HOME", dir.path());
            Self { _dir: dir, previous }
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => crate::env::set_var("JCODE_HOME", value),
                None => crate::env::remove_var("JCODE_HOME"),
            }
        }
    }

    #[test]
    fn a_derived_label_round_trips_through_storage() {
        let _guard = lock_test_env();
        let _home = TempHome::new();

        assert!(refresh_derived("session_fox_1", &["Rewrite the segment sync job"]));
        assert_eq!(
            load_label("session_fox_1"),
            Some("Rewrite Segment Sync Job".to_string())
        );
        assert_eq!(load("session_fox_1").expect("stored").source, TaskLabelSource::Derived);
    }

    #[test]
    fn an_unchanged_source_does_not_rewrite_the_label() {
        let _guard = lock_test_env();
        let _home = TempHome::new();

        let texts = ["Rewrite the segment sync job"];
        assert!(refresh_derived("session_fox_2", &texts));
        assert!(
            !refresh_derived("session_fox_2", &texts),
            "an unchanged source must be a no-op so the sidebar label does not churn"
        );
    }

    #[test]
    fn a_continuation_turn_does_not_move_the_label() {
        let _guard = lock_test_env();
        let _home = TempHome::new();

        assert!(refresh_derived("session_fox_3", &["Rewrite the segment sync job"]));
        assert!(!refresh_derived(
            "session_fox_3",
            &["Rewrite the segment sync job", "continue"]
        ));
        assert_eq!(
            load_label("session_fox_3"),
            Some("Rewrite Segment Sync Job".to_string())
        );
    }

    #[test]
    fn a_model_label_outranks_and_survives_derivation() {
        let _guard = lock_test_env();
        let _home = TempHome::new();

        assert!(store_model_label("session_fox_4", "DI-2507 Chat Endpoint"));
        assert!(!refresh_derived(
            "session_fox_4",
            &["Rewrite the segment sync job"]
        ));
        assert_eq!(
            load_label("session_fox_4"),
            Some("DI-2507 Chat Endpoint".to_string())
        );
    }

    #[test]
    fn a_model_label_is_trimmed_capped_and_rejected_when_empty() {
        let _guard = lock_test_env();
        let _home = TempHome::new();

        assert!(!store_model_label("session_fox_5", "   "));
        assert_eq!(load_label("session_fox_5"), None);

        assert!(store_model_label(
            "session_fox_5",
            "  An Extremely Long Model Written Task Label That Overflows  "
        ));
        let label = load_label("session_fox_5").expect("label");
        assert!(label.chars().count() <= MAX_LABEL_CHARS);
        assert_eq!(label.trim(), label);
    }

    #[test]
    fn a_weak_session_stores_nothing() {
        let _guard = lock_test_env();
        let _home = TempHome::new();

        assert!(!refresh_derived("session_fox_6", &["hi"]));
        assert_eq!(load_label("session_fox_6"), None);
    }

    #[test]
    fn a_missing_or_corrupt_file_reads_as_none_without_erroring() {
        let _guard = lock_test_env();
        let _home = TempHome::new();

        assert_eq!(load_label("session_fox_7"), None);

        let path = crate::storage::jcode_dir()
            .expect("home")
            .join("labels")
            .join("session_fox_7.json");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, b"{ not json").expect("write");

        assert_eq!(load_label("session_fox_7"), None);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p jcode-base task_label`
Expected: FAIL, `cannot find function refresh_derived in this scope` plus the same for `load`, `load_label`, `store_model_label`, `TaskLabelSource`.

- [ ] **Step 3: Write the implementation**

Add to the top of `crates/jcode-base/src/task_label.rs`:

```rust
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
```

Add above the test module:

```rust
/// Where a stored label came from. A model-written label is sticky: the
/// deterministic path never overwrites it, because the model had the whole
/// session in context and the heuristic had one sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskLabelSource {
    Derived,
    Model,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredTaskLabel {
    pub label: String,
    pub source: TaskLabelSource,
    /// Hash of the text this label was derived from. `0` for model labels.
    #[serde(default)]
    pub source_hash: u64,
}

fn label_path(session_id: &str) -> Option<PathBuf> {
    let dir = crate::storage::jcode_dir().ok()?.join("labels");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(format!("{session_id}.json")))
}

/// Read the stored label. Any failure (missing file, corrupt JSON, unreadable
/// home) reads as `None`; a display cache must never be an error path.
pub fn load(session_id: &str) -> Option<StoredTaskLabel> {
    let dir = crate::storage::jcode_dir().ok()?.join("labels");
    let path = dir.join(format!("{session_id}.json"));
    if !path.exists() {
        return None;
    }
    let stored: StoredTaskLabel = crate::storage::read_json(&path).ok()?;
    (!stored.label.trim().is_empty()).then_some(stored)
}

pub fn load_label(session_id: &str) -> Option<String> {
    load(session_id).map(|stored| stored.label)
}

fn write(session_id: &str, stored: &StoredTaskLabel) -> bool {
    let Some(path) = label_path(session_id) else {
        return false;
    };
    crate::storage::write_json_fast(&path, stored).is_ok()
}

fn clamp_label(label: &str) -> Option<String> {
    let normalized = label.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    let clamped: String = normalized.chars().take(MAX_LABEL_CHARS).collect();
    let clamped = clamped.trim_end().to_string();
    (!clamped.is_empty()).then_some(clamped)
}

/// Store a model-written label. Returns whether anything changed on disk.
pub fn store_model_label(session_id: &str, label: &str) -> bool {
    let Some(label) = clamp_label(label) else {
        return false;
    };
    if load(session_id).is_some_and(|stored| {
        stored.source == TaskLabelSource::Model && stored.label == label
    }) {
        return false;
    }
    write(
        session_id,
        &StoredTaskLabel {
            label,
            source: TaskLabelSource::Model,
            source_hash: 0,
        },
    )
}

/// Recompute the deterministic label from `user_texts` (newest last) and store
/// it when it actually changed. Returns whether anything changed on disk.
///
/// No-ops when: the evidence is too weak, a model label already exists, or the
/// source text is the one the current label already came from. That last guard
/// is what stops the sidebar row rewriting itself every turn.
pub fn refresh_derived(session_id: &str, user_texts: &[&str]) -> bool {
    let existing = load(session_id);
    if existing
        .as_ref()
        .is_some_and(|stored| stored.source == TaskLabelSource::Model)
    {
        return false;
    }
    let Some(source) = latest_substantive(user_texts) else {
        return false;
    };
    let hash = source_hash(source);
    if existing.as_ref().is_some_and(|stored| stored.source_hash == hash) {
        return false;
    }
    let Some(label) = derive_label(user_texts).and_then(|label| clamp_label(&label)) else {
        return false;
    };
    if existing.as_ref().is_some_and(|stored| stored.label == label) {
        return false;
    }
    write(
        session_id,
        &StoredTaskLabel {
            label,
            source: TaskLabelSource::Derived,
            source_hash: hash,
        },
    )
}
```

If `tempfile` is not already a dev-dependency of `jcode-base`, it is (used by `process_title.rs` tests); no manifest change is needed.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p jcode-base task_label`
Expected: PASS, 15 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-base/src/task_label.rs
git commit -m "feat(task-label): persist labels in a per-session sidecar with a staleness guard"
```

---

### Task 3: Wire the label into title resolution

**Files:**
- Modify: `crates/jcode-base/src/process_title.rs:59-69` (`terminal_display_title_for_id`)
- Modify: `crates/jcode-tui/src/tui/app/tui_lifecycle_runtime.rs:60-122` (`update_terminal_title`)
- Test: `crates/jcode-base/src/process_title.rs` inline tests

**Interfaces:**
- Consumes: `task_label::load_label` from Task 2.
- Produces: the ranked chain `custom_title` > task label > todo title > `session.title` > fallback, used by both `terminal_display_title_for_id` and `update_terminal_title`.

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `crates/jcode-base/src/process_title.rs`:

```rust
    #[test]
    fn terminal_display_title_prefers_the_task_label_over_the_todo_title() {
        let _guard = lock_test_env();
        let previous_home = std::env::var_os("JCODE_HOME");
        let temp = tempfile::tempdir().expect("temp dir");
        crate::env::set_var("JCODE_HOME", temp.path());

        let session_id = "session_fox_789";
        let session = crate::session::Session::create_with_id(
            session_id.to_string(),
            None,
            Some("Generated title".to_string()),
        );
        session.save().expect("save session");
        crate::todo::save_todos(
            session_id,
            &[crate::todo::TodoItem {
                content: "Synchronize terminal window names".to_string(),
                status: "in_progress".to_string(),
                priority: "high".to_string(),
                id: "window-title".to_string(),
                group: Some("resume title sync".to_string()),
                confidence: None,
                completion_confidence: None,
                confidence_history: Vec::new(),
                blocked_by: Vec::new(),
                assigned_to: None,
            }],
        )
        .expect("save todos");
        assert!(crate::task_label::store_model_label(session_id, "DI-2507 Chat Endpoint"));

        assert_eq!(
            terminal_display_title_for_id(session_id).as_deref(),
            Some("DI-2507 Chat Endpoint")
        );

        match previous_home {
            Some(previous_home) => crate::env::set_var("JCODE_HOME", previous_home),
            None => crate::env::remove_var("JCODE_HOME"),
        }
    }

    #[test]
    fn an_explicit_rename_still_outranks_the_task_label() {
        let _guard = lock_test_env();
        let previous_home = std::env::var_os("JCODE_HOME");
        let temp = tempfile::tempdir().expect("temp dir");
        crate::env::set_var("JCODE_HOME", temp.path());

        let session_id = "session_fox_790";
        let mut session =
            crate::session::Session::create_with_id(session_id.to_string(), None, None);
        session.rename_title(Some("Manual name".to_string()));
        session.save().expect("save session");
        assert!(crate::task_label::store_model_label(session_id, "Derived Name"));

        assert_eq!(
            terminal_display_title_for_id(session_id).as_deref(),
            Some("Manual name")
        );

        match previous_home {
            Some(previous_home) => crate::env::set_var("JCODE_HOME", previous_home),
            None => crate::env::remove_var("JCODE_HOME"),
        }
    }
```

Check `TodoItem`'s current field list before pasting; if it has gained a field, add it rather than editing the assertions.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p jcode-base process_title`
Expected: FAIL on `terminal_display_title_prefers_the_task_label_over_the_todo_title`, actual `Some("resume title sync")`.

- [ ] **Step 3: Write the implementation**

In `crates/jcode-base/src/process_title.rs`, replace the body of `terminal_display_title_for_id`:

```rust
/// Resolve the human-authored title used by terminal windows and `/resume`.
/// Ranked, one writer per source: an explicit rename wins, then the short task
/// label, then the todo/goal-derived title (kept for sessions that predate the
/// label file), then the generated session title.
pub fn terminal_display_title_for_id(session_id: &str) -> Option<String> {
    crate::session::Session::load_startup_stub(session_id)
        .ok()
        .and_then(|session| {
            session
                .custom_title
                .filter(|title| !title.trim().is_empty())
                .or_else(|| crate::task_label::load_label(session_id))
                .or_else(|| crate::todo::load_session_title(session_id))
                .or(session.title)
        })
}
```

In `crates/jcode-tui/src/tui/app/tui_lifecycle_runtime.rs`, replace lines 75-88 (the `todo_title` / `display_title` block) with:

```rust
        // Keep the live terminal title aligned with /resume: an explicit rename
        // wins, then the short task label, then the todo/goal title, then the
        // generated title. Herdr's agent sidebar renders this string, so a
        // clipped prose sentence costs a full row and says less than a label.
        let generated_title = self.session.custom_title.is_none().then(|| {
            crate::task_label::load_label(session_id)
                .or_else(|| crate::todo::load_session_title(session_id))
        });
        let generated_title = generated_title.flatten();
        let display_title = self
            .session
            .custom_title
            .as_deref()
            .or(generated_title.as_deref())
            .or(self.session.title.as_deref());
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p jcode-base process_title && cargo build -p jcode --bin jcode`
Expected: PASS on all `process_title` tests (including the two pre-existing todo-title tests, which still pass because no label file exists in them), and a clean build.

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-base/src/process_title.rs crates/jcode-tui/src/tui/app/tui_lifecycle_runtime.rs
git commit -m "feat(task-label): rank the task label above the todo title in terminal titles"
```

---

### Task 4: Refresh the label at turn end (local and remote)

**Files:**
- Modify: `crates/jcode-tui/src/tui/app/tui_lifecycle_runtime.rs` (add `refresh_task_label`)
- Modify: `crates/jcode-tui/src/tui/app/local.rs:511-535` (`finish_turn`)
- Modify: `crates/jcode-tui/src/tui/app/remote/server_events.rs` (the turn-complete arm near line 1057 that calls `schedule_turn_end_followups`)
- Test: `crates/jcode-base/src/task_label.rs` (message-extraction helper)

**Interfaces:**
- Consumes: `task_label::refresh_derived` from Task 2, the chain from Task 3.
- Produces:
  - `task_label::user_texts_from_stored(messages: &[crate::session::StoredMessage]) -> Vec<String>`
  - `App::refresh_task_label(&self)` - `pub(super)`, infallible, called before `update_terminal_title` at turn end.

- [ ] **Step 1: Write the failing test**

Append to `mod tests` in `crates/jcode-base/src/task_label.rs`:

```rust
    #[test]
    fn user_texts_are_extracted_in_order_ignoring_non_user_and_tool_blocks() {
        use crate::message::{ContentBlock, Role};

        let message = |role: Role, text: &str| crate::session::StoredMessage {
            id: text.to_string(),
            role,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
            display_role: None,
            timestamp: None,
            tool_duration_ms: None,
            token_usage: None,
        };

        let messages = vec![
            message(Role::User, "Rewrite the segment sync job"),
            message(Role::Assistant, "On it"),
            message(Role::User, "now fix the Mautic webhook timeout"),
        ];

        assert_eq!(
            user_texts_from_stored(&messages),
            vec![
                "Rewrite the segment sync job".to_string(),
                "now fix the Mautic webhook timeout".to_string(),
            ]
        );
    }

    #[test]
    fn a_system_display_role_message_is_not_treated_as_a_user_request() {
        use crate::message::{ContentBlock, Role};

        let messages = vec![crate::session::StoredMessage {
            id: "reminder".to_string(),
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "<system-reminder>build finished</system-reminder>".to_string(),
                cache_control: None,
            }],
            display_role: Some(crate::session::StoredDisplayRole::System),
            timestamp: None,
            tool_duration_ms: None,
            token_usage: None,
        }];

        assert!(user_texts_from_stored(&messages).is_empty());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p jcode-base task_label`
Expected: FAIL, `cannot find function user_texts_from_stored in this scope`.

- [ ] **Step 3: Write the implementation**

Add to `crates/jcode-base/src/task_label.rs`:

```rust
/// Plain user request texts, oldest first. Skips assistant/tool messages,
/// system-role injections and reminder blocks, so a `<system-reminder>` or a
/// background-task note can never become the session's label.
pub fn user_texts_from_stored(messages: &[crate::session::StoredMessage]) -> Vec<String> {
    use crate::message::{ContentBlock, Role};

    messages
        .iter()
        .filter(|message| {
            matches!(message.role, Role::User) && message.display_role.is_none()
        })
        .filter_map(|message| {
            let text = message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            let text = text.trim();
            let is_injected = text.starts_with("<system-reminder>")
                || text.starts_with("[NOTIFICATION]")
                || text.starts_with("<system>");
            (!text.is_empty() && !is_injected).then(|| text.to_string())
        })
        .collect()
}
```

Add to `crates/jcode-tui/src/tui/app/tui_lifecycle_runtime.rs`, directly above `update_terminal_title`:

```rust
    /// Recompute the short task label from the session's user messages.
    ///
    /// Called at turn end, after the turn has already completed. Every failure
    /// mode inside is swallowed by `task_label` itself, so this can never fail
    /// a turn, block it, or surface an error: the worst case is that the title
    /// keeps its previous value or falls back to the animal name.
    pub(super) fn refresh_task_label(&self) {
        if self.is_replay || self.suppress_terminal_title_updates {
            return;
        }
        let session_id = if self.is_remote {
            self.remote_session_id
                .as_deref()
                .unwrap_or(&self.session.id)
        } else {
            &self.session.id
        };
        if self.session.custom_title.is_some() {
            return;
        }
        let texts = crate::task_label::user_texts_from_stored(&self.session.messages);
        let refs = texts.iter().map(String::as_str).collect::<Vec<_>>();
        crate::task_label::refresh_derived(session_id, &refs);
    }
```

In `crates/jcode-tui/src/tui/app/local.rs`, inside `finish_turn`, immediately after the `note_runtime_memory_event_force` line:

```rust
    app.refresh_task_label();
    app.update_terminal_title();
```

In `crates/jcode-tui/src/tui/app/remote/server_events.rs`, in the turn-complete arm that calls `app.schedule_turn_end_followups()` around line 1057, add the same two lines immediately before that call:

```rust
            app.refresh_task_label();
            app.update_terminal_title();
```

- [ ] **Step 4: Run the tests and build**

Run: `cargo test -p jcode-base task_label && cargo build --profile selfdev -p jcode --bin jcode`
Expected: PASS, 17 tests, and a clean build.

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-base/src/task_label.rs crates/jcode-tui/src/tui/app/tui_lifecycle_runtime.rs crates/jcode-tui/src/tui/app/local.rs crates/jcode-tui/src/tui/app/remote/server_events.rs
git commit -m "feat(task-label): refresh the label at turn end on the local and remote paths"
```

---

### Task 5: Optional model-written label on the existing todo call

**Files:**
- Modify: `crates/jcode-task-types/src/lib.rs:472-496` (`TodoPlan`)
- Modify: `crates/jcode-app-core/src/tool/todo.rs:396-412` (`merge_plan`) and `:779-794` (the `plan` JSON schema)
- Test: `crates/jcode-app-core/src/tool/todo.rs` inline tests

**Interfaces:**
- Consumes: `task_label::store_model_label` from Task 2.
- Produces: `TodoPlan.task_label: Option<String>`, persisted through the todo plan file and written through to the label sidecar on every todo-tool write.

This layer is **optional quality**, not the fix: Task 4 already covers sessions that never call the todo tool. It costs one schema property and no extra call.

- [ ] **Step 1: Write the failing tests**

Add to the test module in `crates/jcode-app-core/src/tool/todo.rs`:

```rust
    #[test]
    fn the_plan_schema_exposes_an_optional_task_label() {
        let schema = TodoTool::new().parameters();
        let plan_props = schema["properties"]["plan"]["properties"]
            .as_object()
            .expect("plan properties");
        assert!(plan_props.contains_key("task_label"));
        let required = schema["properties"]["plan"]["required"]
            .as_array()
            .expect("plan required");
        assert!(
            !required.iter().any(|value| value == "task_label"),
            "task_label must stay optional so an omitted label never fails a todo write"
        );
    }

    #[test]
    fn an_omitted_task_label_inherits_the_stored_one() {
        let stored = TodoPlan {
            user_intention: Some("ship it".to_string()),
            task_label: Some("DI-2507 Chat Endpoint".to_string()),
            ..Default::default()
        };
        let merged = merge_plan(
            &stored,
            Some(TodoPlan {
                user_intention: Some("ship it".to_string()),
                ..Default::default()
            }),
        );
        assert_eq!(merged.task_label.as_deref(), Some("DI-2507 Chat Endpoint"));
    }
```

Check the exact name of the schema accessor on `TodoTool` in that file (it is the `Tool` trait's parameters method) and use whatever the neighbouring `plan_props` test at line ~1005 already uses.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p jcode-app-core todo`
Expected: FAIL, `struct TodoPlan has no field named task_label`.

- [ ] **Step 3: Write the implementation**

In `crates/jcode-task-types/src/lib.rs`, add to `TodoPlan` after `user_intention`:

```rust
    /// A 2-4 word label naming the current work, shown as the terminal window
    /// title and therefore as the agent's row in external session dashboards.
    /// Optional: an omitted label inherits the stored one, and a session that
    /// never writes one still gets a label derived from its user messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_label: Option<String>,
```

In `crates/jcode-app-core/src/tool/todo.rs`, in `merge_plan`, after the `user_intention` inheritance:

```rust
    if plan.task_label.is_none() {
        plan.task_label = stored.task_label.clone();
    }
```

In the same file's `plan` schema object, add after the `user_intention` property:

```rust
                        "task_label": {
                            "type": "string",
                            "description": "2-4 word label naming the current work, under 30 chars, e.g. 'DI-2507 Chat Endpoint'. Shown as the terminal title. Omit to keep the existing one."
                        },
```

Where the todo tool persists the merged plan (immediately after the `save_plan`/plan-write call), write through to the label store:

```rust
    if let Some(label) = merged_plan.task_label.as_deref() {
        // Best effort: a title cache must never fail a todo write.
        let _ = crate::task_label::store_model_label(session_id, label);
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p jcode-app-core todo && cargo test -p jcode-base task_label`
Expected: PASS, including the pre-existing `merge_plan` and schema tests.

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-task-types/src/lib.rs crates/jcode-app-core/src/tool/todo.rs
git commit -m "feat(task-label): let the todo tool name the session in the call it already makes"
```

---

### Task 6: Acceptance in Joel's actual sidebar

**Files:**
- Modify: none unless a check fails.
- Test: the live runtime, `herdr agent list`.

This is the task the brief actually asks for. "It compiles" is not the bar; defect B must be **demonstrated fixed on a session that never called the todo tool**.

- [ ] **Step 1: Build and point the daemon at the new binary**

```bash
cargo build --profile selfdev -p jcode --bin jcode
```

Then reload onto it with `selfdev build-reload`. A freshly built binary is inert until the shared-server symlink is repointed and the daemon restarted, so every runtime check before this step silently measures the old code.

- [ ] **Step 2: Prove defect B is fixed (no todo tool involved)**

```bash
./target/selfdev/jcode run --no-update --socket /run/user/$(id -u)/jcode-labeltest.sock \
  'Rewrite the Mautic segment sync job to batch its API writes. Do not use the todo tool.'
```

Then read the label the session stored, and confirm the session really has no todo file:

```bash
ls ~/.jcode/labels/ | tail -3
cat ~/.jcode/labels/<session_id>.json
ls ~/.jcode/todos/ | grep <session_id> || echo "no todo file: defect B case confirmed"
```

Expected: a `label` of 2-4 words under 30 chars naming the segment sync work, `"source": "derived"`, and **no** todo file for that session.

- [ ] **Step 3: Read it where Joel reads it**

Start an interactive session in a Herdr pane, give it one real task, let one turn finish, then:

```bash
herdr agent list
```

Expected: that pane's `terminal_title_stripped` is the short label, not `jcode/citadel Cat` and not a sentence ending in `…`. Record the exact string observed.

- [ ] **Step 4: Confirm `/rename` still wins**

In that pane run `/rename Release planning`, then send another message and let the turn finish.

```bash
herdr agent list
```

Expected: still `Release planning` after the turn. The label file may exist but must not be shown.

- [ ] **Step 5: Break the label path and confirm the turn survives**

```bash
chmod 000 ~/.jcode/labels
```

Run one more turn in the pane.

Expected: the turn completes normally, no error is shown, and the title keeps its previous value or falls back cleanly. Then restore:

```bash
chmod 755 ~/.jcode/labels
```

- [ ] **Step 6: Full test sweep and commit the evidence**

```bash
cargo test -p jcode-base task_label
cargo test -p jcode-base process_title
cargo test -p jcode-app-core todo
```

Write the observed `herdr agent list` strings from steps 3 and 4 into the commit body, then:

```bash
git commit --allow-empty -m "test(task-label): record sidebar acceptance evidence"
```

---

## Self-Review

**Spec coverage**

| Brief requirement | Task |
| --- | --- |
| Defect A: label instead of a clipped sentence | 1 (4-word cap, 30 chars, word-boundary clamp), 3 (ranked above the todo sentence) |
| Defect B: a session that never calls the todo tool | 1, 2, 4 (derivation from user messages), demonstrated in 6 step 2 |
| Constraint 1: no extra provider call, no second model | 1 and 2 are pure local computation; 5 rides an existing tool call |
| Constraint 2: never affects the turn | 2 (no `Result` escapes), 4 (`refresh_task_label` is infallible, runs after the turn), 6 step 5 (planted failure) |
| Constraint 3: `/rename` wins | 3 (chain order, test), 4 (early return on `custom_title`), 6 step 4 |
| Constraint 4: fits 48 chars with the icon | 1 (`MAX_LABEL_CHARS = 30`, cap test), 2 (`clamp_label` on the model path) |
| Constraint 5: weak evidence gets no label | 1 (`MIN_KEYWORDS`, `None` cases), 2 (`a_weak_session_stores_nothing`) |
| Open question: when computed | 4 (turn end) + 2 (`source_hash` guard, continuation test) |
| Open question: where it lives | 2 (sidecar, rationale in the decisions table) |
| Open question: how produced | 1 + 5 |
| Open question: does the todo path stay | 3 (ranked, single writer each, rationale) |
| Open question: how Herdr reads it | Unchanged terminal title; no protocol touched anywhere in this plan |
| Out of scope: Herdr config, smart-rename, tab renaming | Untouched |

**Known gap, stated rather than hidden:** the refresh hook is TUI-side, so `jcode run` one-shots and non-TUI harness sessions get a label only if they call the todo tool. Herdr panes are TUI, so the brief's acceptance case is covered. Moving the hook into `Agent::fire_turn_end_hook` would cover everything and is the obvious follow-up if a non-TUI pane ever needs it.

**Placeholder scan:** no TBDs, no "add error handling", every code step carries real code, every test carries real assertions.

**Type consistency:** `derive_label`, `latest_substantive`, `source_hash`, `MAX_LABEL_CHARS` (Task 1) are used under exactly those names in Tasks 2-4. `StoredTaskLabel`/`TaskLabelSource`/`load`/`load_label`/`store_model_label`/`refresh_derived` (Task 2) are used under exactly those names in Tasks 3-5. `user_texts_from_stored` (Task 4) matches its call site. `TodoPlan.task_label` (Task 5) matches `merge_plan` and the schema.
