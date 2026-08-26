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
    "a", "about", "actually", "after", "all", "also", "an", "and", "any", "are", "as", "at",
    "back", "basically", "be", "been", "before", "being", "both", "but", "by", "can", "could",
    "did", "do", "does", "doing", "done", "each", "else", "etc", "even", "every", "first", "for",
    "from", "get", "go", "had", "has", "have", "he", "her", "here", "hers", "him", "his", "how",
    "i", "if", "in", "into", "is", "it", "its", "just", "let", "lets", "like", "may", "me",
    "might", "more", "most", "much", "must", "my", "need", "needs", "new", "no", "not", "now",
    "of", "off", "ok", "okay", "on", "once", "one", "only", "or", "other", "our", "out", "over",
    "own", "per", "please", "really", "same", "see", "shall", "she", "should", "so", "some",
    "still", "such", "sure", "than", "that", "the", "their", "them", "then", "there",
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
    first_clause(text)
        .split_whitespace()
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
    token
        .trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_' && c != '/' && c != '.')
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

/// Only the first clause names the task; everything after a sentence break is
/// qualification.
///
/// The break must be punctuation followed by whitespace or end-of-text. A bare
/// `.` also appears inside `~/dir/.local/file.md`, and breaking there truncated
/// the one token that named the work.
fn first_clause(text: &str) -> &str {
    let trimmed = text.trim();
    let bytes = trimmed.as_bytes();
    let end = trimmed
        .char_indices()
        .find(|(index, c)| {
            if !matches!(c, '.' | '?' | '!' | '\n' | ':' | ';') {
                return false;
            }
            let next = index + c.len_utf8();
            next >= bytes.len() || trimmed[next..].starts_with(char::is_whitespace)
        })
        .map(|(index, _)| index)
        .unwrap_or(trimmed.len());
    let clause = trimmed[..end].trim();
    if clause.is_empty() { trimmed } else { clause }
}

/// Title-case a word, but never rewrite one that already carries its own
/// capitalization. `KumoMTA`, `iOS` and `POST` are proper nouns whose shape is
/// the recognizable part; lowercasing their tail makes the label read wrong.
fn title_case(word: &str) -> String {
    let has_inner_uppercase = word.chars().skip(1).any(char::is_uppercase);
    if has_inner_uppercase || word.chars().all(|c| !c.is_lowercase()) {
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
        if !words
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&cased))
        {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_a_short_label_from_a_plain_request() {
        assert_eq!(
            derive_label(&["Add retry logic to the KumoMTA bounce parser"]),
            Some("Add Retry Logic KumoMTA".to_string())
        );
    }

    #[test]
    fn a_proper_noun_keeps_its_own_capitalization() {
        assert_eq!(
            derive_label(&["debug the KumoMTA iOS SDK handshake"]),
            Some("Debug KumoMTA iOS SDK".to_string())
        );
    }

    #[test]
    fn keeps_a_ticket_key_first_and_uppercase() {
        assert_eq!(
            derive_label(&[
                "Implement the DI-2507 POST /api/chat/questions plan task-by-task with TDD"
            ]),
            Some("DI-2507 Implement POST Plan".to_string())
        );
    }

    #[test]
    fn falls_back_to_a_path_stem_when_the_prose_is_all_stopwords() {
        assert_eq!(
            derive_label(&[
                "read ~/MYNE/Projects/tools/jcode/.local/prd/session-task-label-brief.md and do it"
            ]),
            Some("Read Session Task Label".to_string())
        );
    }

    #[test]
    fn a_dot_inside_a_path_does_not_end_the_clause() {
        assert_eq!(
            first_clause("read ~/tools/jcode/.local/prd/brief.md and do it"),
            "read ~/tools/jcode/.local/prd/brief.md and do it"
        );
        assert_eq!(
            first_clause("Fix the parser. Then ship it."),
            "Fix the parser"
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
        let texts = [
            "Rewrite the segment sync job",
            "now fix the Mautic webhook timeout",
        ];
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
