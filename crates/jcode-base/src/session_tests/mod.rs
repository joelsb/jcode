use super::*;
use std::ffi::OsString;

fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    crate::storage::lock_test_env()
}

struct EnvVarGuard {
    key: &'static str,
    prev: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let prev = std::env::var_os(key);
        crate::env::set_var(key, value);
        Self { key, prev }
    }

    fn remove(key: &'static str) -> Self {
        let prev = std::env::var_os(key);
        crate::env::remove_var(key);
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(prev) = &self.prev {
            crate::env::set_var(self.key, prev);
        } else {
            crate::env::remove_var(self.key);
        }
    }
}

#[path = "cases.rs"]
mod cases;

/// Account attribution must key off the provider we authenticated against, not
/// off the session's routing namespace.
mod account_label_attribution {
    use crate::session::{active_account_label_for_provider, derive_session_provider_key};

    /// Regression for a misattribution observed 2026-08-27: with
    /// `JCODE_RUNTIME_PROVIDER=openai` set (an OpenAI-compatible routing hint),
    /// an Anthropic session had its usage labelled `openai-1`, which would bill
    /// Claude tokens to the OpenAI account in every downstream consumer.
    ///
    /// The two functions must disagree here, and that disagreement is the
    /// point: routing key follows the env var, account label follows the
    /// provider that actually holds the credentials.
    #[test]
    fn runtime_provider_env_must_not_move_the_billed_account() {
        // SAFETY: single-threaded scope; restored before returning.
        let restore = std::env::var("JCODE_RUNTIME_PROVIDER").ok();
        unsafe { std::env::set_var("JCODE_RUNTIME_PROVIDER", "openai") };

        let routing_key = derive_session_provider_key("anthropic");
        let label = active_account_label_for_provider("anthropic");

        match restore {
            Some(value) => unsafe { std::env::set_var("JCODE_RUNTIME_PROVIDER", value) },
            None => unsafe { std::env::remove_var("JCODE_RUNTIME_PROVIDER") },
        }

        assert_eq!(
            routing_key.as_deref(),
            Some("openai"),
            "routing key is expected to follow the env var"
        );
        let label = label.expect("anthropic must resolve to an anthropic account");
        assert!(
            label.starts_with("claude-"),
            "anthropic usage must bill a claude-* account, got {label}"
        );
    }

    #[test]
    fn openai_provider_bills_an_openai_account() {
        let label = active_account_label_for_provider("openai")
            .expect("openai must resolve to an openai account");
        assert!(
            label.starts_with("openai-"),
            "openai usage must bill an openai-* account, got {label}"
        );
    }

    /// Providers with no account identity of ours must stay unlabelled rather
    /// than defaulting into somebody's tab.
    #[test]
    fn unowned_providers_have_no_account_label() {
        for provider in ["openrouter", "omniroute", "openai-compatible", "gemini", ""] {
            assert_eq!(
                active_account_label_for_provider(provider),
                None,
                "{provider} must not claim one of our accounts"
            );
        }
    }
}

/// Credentials must not survive into the persisted transcript. Unit-testing the
/// redactor proves the function works; this proves it is actually WIRED to the
/// write path, which is the part that was missing when 30 credentials landed in
/// 16 transcript files.
mod credential_redaction {
    use crate::session::Session;
    use jcode_message_types::{ContentBlock, Role};

    const FAKE_ACCESS: &str = "sk-ant-oat01-AAAAAAAAAAAAAAAAAAAAAAAA";
    const FAKE_REFRESH: &str = "sk-ant-ort01-BBBBBBBBBBBBBBBBBBBBBBBB";

    /// The realistic path: a tool prints an auth file and the result is stored.
    #[test]
    fn tool_result_credentials_are_not_persisted() {
        let mut session = Session::create_with_id("ses_redact".into(), None, None);
        session.add_message_ext(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: "toolu_1".into(),
                content: format!("{{\"access\":\"{FAKE_ACCESS}\",\"refresh\":\"{FAKE_REFRESH}\"}}"),
                is_error: None,
            }],
            None,
            None,
        );

        let stored = serde_json::to_string(&session.messages).expect("serialize");
        assert!(
            !stored.contains(FAKE_ACCESS),
            "access token reached the transcript"
        );
        assert!(
            !stored.contains(FAKE_REFRESH),
            "refresh token reached the transcript"
        );
        assert!(
            stored.contains("ANTHROPIC_OAUTH_ACCESS"),
            "no marker: {stored}"
        );
        assert!(
            stored.contains("ANTHROPIC_OAUTH_REFRESH"),
            "no marker: {stored}"
        );
    }

    /// A model echoing a token back is the same durable leak by another route.
    #[test]
    fn assistant_text_credentials_are_not_persisted() {
        let mut session = Session::create_with_id("ses_redact2".into(), None, None);
        session.add_message_ext(
            Role::Assistant,
            vec![ContentBlock::Text {
                text: format!("Your token is {FAKE_ACCESS} - keep it safe."),
                cache_control: None,
            }],
            None,
            None,
        );

        let stored = serde_json::to_string(&session.messages).expect("serialize");
        assert!(
            !stored.contains(FAKE_ACCESS),
            "token reached the transcript"
        );
        assert!(
            stored.contains("keep it safe"),
            "surrounding text was destroyed"
        );
    }

    /// Ordinary output must be byte-identical, or every transcript is suspect.
    #[test]
    fn ordinary_tool_output_is_untouched() {
        let mut session = Session::create_with_id("ses_redact3".into(), None, None);
        let ordinary = "total 24\ndrwxr-xr-x  3 user  staff   96 Aug 28 09:00 .";
        session.add_message_ext(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: "toolu_2".into(),
                content: ordinary.to_string(),
                is_error: None,
            }],
            None,
            None,
        );
        match &session.messages[0].content[0] {
            ContentBlock::ToolResult { content, .. } => assert_eq!(content, ordinary),
            other => panic!("unexpected block: {other:?}"),
        }
    }
}
