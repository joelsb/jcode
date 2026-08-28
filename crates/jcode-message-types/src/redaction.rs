//! Redaction of credential-shaped strings before they are persisted.
//!
//! Tool output is stored verbatim in the session transcript, which is a plain
//! JSON file on disk. Any command whose output happens to contain a credential
//! therefore writes that credential to disk, where it outlives the token,
//! survives session resume, and is invisible in normal use.
//!
//! This is not hypothetical. On 2026-08-28 a sweep of the local transcripts
//! found **30 distinct credentials across 16 files** - Anthropic OAuth access
//! and refresh tokens, Anthropic API keys, an OpenAI key and a GitHub token -
//! all written by agents reading an auth file or an env var and printing it.
//! None was the currently-live token by then, but four refresh tokens were
//! recent enough to still mint access tokens.
//!
//! ## Why redact at persistence rather than in a hook
//!
//! jcode's `post_tool` hook is an observer: fire-and-forget, dispatched after
//! the result is already recorded. It cannot rewrite what it sees. A `pre_tool`
//! gate can inspect the *command*, which is where the existing external guard
//! works, but it cannot know what a command will print. Only the write path
//! sees the actual bytes, so that is the only place a guarantee can hold.
//!
//! ## What is deliberately NOT done
//!
//! The credential is replaced in the **stored** copy only, after the model has
//! already been handed the real output for the current turn. Redacting before
//! the model sees it would break legitimate work (an agent that just minted a
//! token and needs to use it). The goal is that nothing durable retains a
//! secret, not that the secret never exists in memory.

/// A credential pattern: literal prefix plus the shape of the body that follows.
struct Pattern {
    /// Literal prefix, matched exactly. A prefix search is far cheaper than a
    /// regex over every tool result, and every credential worth catching here
    /// has one.
    prefix: &'static str,
    /// Minimum body length after the prefix. Below this it is a placeholder or
    /// an example, not a credential, and redacting it only creates confusion.
    min_body: usize,
    /// Human-readable label used in the replacement marker, so a reader knows
    /// what was removed and can reason about severity without the value.
    label: &'static str,
}

/// Patterns are ordered longest-prefix-first so `sk-ant-oat01-` is matched as
/// an OAuth access token rather than by a hypothetical shorter `sk-ant-` rule.
const PATTERNS: &[Pattern] = &[
    Pattern { prefix: "sk-ant-oat", min_body: 20, label: "ANTHROPIC_OAUTH_ACCESS" },
    Pattern { prefix: "sk-ant-ort", min_body: 20, label: "ANTHROPIC_OAUTH_REFRESH" },
    Pattern { prefix: "sk-ant-api", min_body: 20, label: "ANTHROPIC_API_KEY" },
    Pattern { prefix: "sk-proj-", min_body: 20, label: "OPENAI_KEY" },
    Pattern { prefix: "ghp_", min_body: 32, label: "GITHUB_TOKEN" },
    Pattern { prefix: "ghs_", min_body: 32, label: "GITHUB_TOKEN" },
    Pattern { prefix: "gho_", min_body: 32, label: "GITHUB_TOKEN" },
    Pattern { prefix: "ghu_", min_body: 32, label: "GITHUB_TOKEN" },
    Pattern { prefix: "ghr_", min_body: 32, label: "GITHUB_TOKEN" },
    Pattern { prefix: "xoxb-", min_body: 20, label: "SLACK_TOKEN" },
    Pattern { prefix: "xoxp-", min_body: 20, label: "SLACK_TOKEN" },
    Pattern { prefix: "AKIA", min_body: 16, label: "AWS_ACCESS_KEY_ID" },
];

/// Characters that can appear inside a credential body. Anything else ends it.
/// Deliberately excludes `.` and `/`: including them would swallow a trailing
/// sentence period, and a URL path, into the redacted span.
fn is_body_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// Replaces every credential-shaped substring with a marker naming its kind.
///
/// Returns `None` when nothing matched, so the overwhelmingly common case
/// allocates nothing and the caller can keep its existing string.
pub fn redact_credentials(text: &str) -> Option<String> {
    // Cheap pre-check: no candidate prefix means no work. Tool results are
    // large and frequent, so this path has to stay close to free.
    if !PATTERNS.iter().any(|p| text.contains(p.prefix)) {
        return None;
    }

    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut index = 0usize;
    let mut redacted_any = false;

    while index < text.len() {
        // Only consider a match on a char boundary, so multi-byte content is
        // never split.
        if !text.is_char_boundary(index) {
            out.push(bytes[index] as char);
            index += 1;
            continue;
        }
        let rest = &text[index..];

        let matched = PATTERNS.iter().find_map(|pattern| {
            let after = rest.strip_prefix(pattern.prefix)?;
            let body_len = after
                .char_indices()
                .find(|(_, c)| !is_body_char(*c))
                .map(|(i, _)| i)
                .unwrap_or(after.len());
            if body_len >= pattern.min_body {
                Some((pattern, pattern.prefix.len() + body_len))
            } else {
                None
            }
        });

        match matched {
            Some((pattern, span)) => {
                out.push_str(&format!("[REDACTED:{}]", pattern.label));
                index += span;
                redacted_any = true;
            }
            None => {
                let ch = rest.chars().next().expect("rest is non-empty");
                out.push(ch);
                index += ch.len_utf8();
            }
        }
    }

    if redacted_any { Some(out) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact shapes found in the real leak: Anthropic OAuth access and
    /// refresh tokens, an Anthropic API key, plus the other providers' formats.
    #[test]
    fn redacts_every_credential_shape_we_have_actually_seen() {
        let cases = [
            ("sk-ant-oat01-K9Ttk3aqEeqsqR4egSSBZsJrQEm42Bk8MQgKzVilvNh0", "ANTHROPIC_OAUTH_ACCESS"),
            ("sk-ant-ort01-K9Ttk3aqEeqsqR4egSSBZsJrQEm42Bk8MQgKzVilvNh0", "ANTHROPIC_OAUTH_REFRESH"),
            ("sk-ant-api03-K9Ttk3aqEeqsqR4egSSBZsJrQEm42Bk8MQgKzVilvNh0", "ANTHROPIC_API_KEY"),
            ("sk-proj-K9Ttk3aqEeqsqR4egSSBZsJrQEm42Bk8MQgKzVilvNh0", "OPENAI_KEY"),
            ("ghp_K9Ttk3aqEeqsqR4egSSBZsJrQEm42Bk8MQgK", "GITHUB_TOKEN"),
            ("xoxb-K9Ttk3aqEeqsqR4egSSBZsJrQEm42Bk8MQgK", "SLACK_TOKEN"),
            ("AKIAIOSFODNN7EXAMPLE", "AWS_ACCESS_KEY_ID"),
        ];
        for (secret, label) in cases {
            let input = format!("token is {secret} ok");
            let out = redact_credentials(&input).expect("must redact");
            assert!(!out.contains(secret), "{label}: value survived -> {out}");
            assert!(out.contains(&format!("[REDACTED:{label}]")), "{label}: wrong marker -> {out}");
        }
    }

    /// The distinguishing bit: `oat` and `ort` differ by one character and by
    /// weeks of lifetime, so a reader of a redacted transcript must still be
    /// able to tell which kind leaked without seeing the value.
    #[test]
    fn distinguishes_access_from_refresh_tokens() {
        let both = "access sk-ant-oat01-AAAAAAAAAAAAAAAAAAAAAA then refresh sk-ant-ort01-BBBBBBBBBBBBBBBBBBBBBB";
        let out = redact_credentials(both).expect("must redact");
        assert!(out.contains("[REDACTED:ANTHROPIC_OAUTH_ACCESS]"), "{out}");
        assert!(out.contains("[REDACTED:ANTHROPIC_OAUTH_REFRESH]"), "{out}");
    }

    /// Multiple credentials in one blob, which is the realistic case: the leak
    /// came from printing a whole auth file containing several accounts.
    #[test]
    fn redacts_all_occurrences_not_just_the_first() {
        let json = concat!(
            r#"[{"label":"claude-1","access":"sk-ant-oat01-AAAAAAAAAAAAAAAAAAAAAA","#,
            r#""refresh":"sk-ant-ort01-BBBBBBBBBBBBBBBBBBBBBB"},"#,
            r#"{"label":"claude-2","access":"sk-ant-oat01-CCCCCCCCCCCCCCCCCCCCCC"}]"#
        );
        let out = redact_credentials(json).expect("must redact");
        assert!(!out.contains("sk-ant-oat01-A"), "{out}");
        assert!(!out.contains("sk-ant-oat01-C"), "{out}");
        assert!(!out.contains("sk-ant-ort01-B"), "{out}");
        assert_eq!(out.matches("[REDACTED:").count(), 3, "{out}");
        // Surrounding structure must survive, or a transcript becomes unreadable.
        assert!(out.contains(r#""label":"claude-1""#), "{out}");
        assert!(out.contains(r#""label":"claude-2""#), "{out}");
    }

    /// A redactor that fires on ordinary text is worse than none: it destroys
    /// transcripts and gets switched off. These must all pass through untouched.
    #[test]
    fn leaves_non_credentials_completely_alone() {
        let safe = [
            "the sk-ant- prefix identifies Anthropic credentials",
            "sk-ant-oat01-SHORT",                       // body too short
            "set ANTHROPIC_API_KEY in the environment",
            "AKIA",                                     // prefix with no body
            "ghp_short",
            "https://example.com/sk-proj-docs",         // too short after prefix
            "a normal sentence with no secrets at all",
            "",
        ];
        for text in safe {
            assert!(
                redact_credentials(text).is_none(),
                "false positive on: {text:?}"
            );
        }
    }

    /// Redaction must not corrupt surrounding punctuation or swallow the rest
    /// of a sentence, which is what a greedy body pattern would do.
    #[test]
    fn stops_at_the_credential_boundary() {
        let text = "key=sk-ant-oat01-AAAAAAAAAAAAAAAAAAAAAA. Next sentence.";
        let out = redact_credentials(text).expect("must redact");
        assert_eq!(out, "key=[REDACTED:ANTHROPIC_OAUTH_ACCESS]. Next sentence.", "{out}");
    }

    /// Multi-byte content must survive byte-index arithmetic intact.
    #[test]
    fn preserves_multibyte_text() {
        let text = "señor 🔑 sk-ant-oat01-AAAAAAAAAAAAAAAAAAAAAA café";
        let out = redact_credentials(text).expect("must redact");
        assert!(out.starts_with("señor 🔑 "), "{out}");
        assert!(out.ends_with(" café"), "{out}");
        assert!(!out.contains("sk-ant-oat01-A"), "{out}");
    }

    /// Longest-prefix ordering: `sk-ant-api` must not be reported as an OAuth
    /// token just because a shorter rule also matched.
    #[test]
    fn picks_the_most_specific_pattern() {
        let out = redact_credentials("sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAA").expect("must redact");
        assert!(out.contains("ANTHROPIC_API_KEY"), "{out}");
        assert!(!out.contains("OAUTH"), "{out}");
    }
}
