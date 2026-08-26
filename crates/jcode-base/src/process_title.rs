const LINUX_PROCESS_TITLE_LIMIT: usize = 15;
#[cfg(target_os = "linux")]
const KILLALL_PROCESS_NAME: &str = "jcode";

pub fn compact_process_title(prefix: &str, name: Option<&str>) -> String {
    let mut title = prefix.to_string();
    if let Some(name) = name.filter(|name| !name.is_empty()) {
        let remaining = LINUX_PROCESS_TITLE_LIMIT.saturating_sub(title.len());
        if remaining > 0 {
            title.push_str(&name.chars().take(remaining).collect::<String>());
        }
    }
    title
}

pub fn session_name(session_id: &str) -> String {
    crate::id::extract_session_name(session_id)
        .map(|name| name.to_string())
        .unwrap_or_else(|| session_id.to_string())
}

fn normalized_display_title(title: &str) -> Option<String> {
    let normalized = title.split_whitespace().collect::<Vec<_>>().join(" ");
    (!normalized.is_empty()).then_some(normalized)
}

fn capitalize_ascii_label(label: &str) -> String {
    let mut chars = label.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{}…", truncated)
    } else {
        truncated
    }
}

pub fn terminal_session_label(session_name: &str, display_title: Option<&str>) -> String {
    let fallback = capitalize_ascii_label(session_name);
    let Some(title) = display_title.and_then(normalized_display_title) else {
        return fallback;
    };
    if title.eq_ignore_ascii_case(session_name) || title.eq_ignore_ascii_case(&fallback) {
        return fallback;
    }
    format!("{} ({})", truncate_chars(&title, 48), session_name)
}

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

/// Build the deliberately minimal terminal window title.
///
/// `icon` is optional and is normally `None`. Terminal-title consumers that
/// matter here (Herdr's agent sidebar) already render their own state icon
/// column, so a second emoji carries no information and costs characters in a
/// row that is the only view of ten concurrent jobs. Surfaces where the title
/// stands alone, such as the macOS menubar dropdown, still pass one.
///
/// Never repeats `jcode` or the memorable animal name in window chrome.
pub fn terminal_window_title(
    icon: Option<&str>,
    display_title: Option<&str>,
    fallback_label: Option<&str>,
    is_selfdev: bool,
) -> String {
    let display_title = display_title
        .and_then(normalized_display_title)
        .map(|title| truncate_chars(&title, 48));
    let suffix = if is_selfdev { " [self-dev]" } else { "" };
    let prefix = match icon.map(str::trim).filter(|icon| !icon.is_empty()) {
        Some(icon) => format!("{icon} "),
        None => String::new(),
    };
    let title = match display_title {
        Some(title) => format!("{prefix}{title}{suffix}"),
        None => match fallback_label.and_then(normalized_display_title) {
            Some(label) => format!("{prefix}{label}{suffix}"),
            None => format!("{}{suffix}", prefix.trim_end()),
        },
    };
    crate::output_style::terminal_text(&title).into_owned()
}

pub fn terminal_session_label_for_id(session_id: &str) -> String {
    let session_name = session_name(session_id);
    let display_title = terminal_display_title_for_id(session_id);
    match display_title.as_deref() {
        Some(title) => terminal_session_label(&session_name, Some(title)),
        None => session_name,
    }
}

pub fn set_title(title: impl AsRef<str>) {
    proctitle::set_title(title.as_ref());
    set_killall_process_name();
}

fn set_killall_process_name() {
    #[cfg(target_os = "linux")]
    unsafe {
        let mut name = [0u8; 16];
        let bytes = KILLALL_PROCESS_NAME.as_bytes();
        let len = bytes.len().min(name.len().saturating_sub(1));
        name[..len].copy_from_slice(&bytes[..len]);
        let _ = libc::prctl(libc::PR_SET_NAME, name.as_ptr(), 0, 0, 0);
    }
}

pub fn set_server_title(server_name: &str) {
    set_title(compact_process_title("jcode:s:", Some(server_name)));
}

pub fn set_client_generic_title(is_selfdev: bool) {
    let prefix = if is_selfdev {
        "jcode:selfdev"
    } else {
        "jcode:client"
    };
    set_title(compact_process_title(prefix, None));
}

pub fn set_client_session_title(session_id: &str, is_selfdev: bool) {
    set_client_display_title(&session_name(session_id), is_selfdev);
}

pub fn set_client_display_title(session_name: &str, is_selfdev: bool) {
    let prefix = if is_selfdev { "jcode:d:" } else { "jcode:c:" };
    set_title(compact_process_title(prefix, Some(session_name)));
}

pub fn set_client_remote_display_title(server_name: &str, session_name: &str, is_selfdev: bool) {
    if server_name.is_empty() || server_name.eq_ignore_ascii_case("jcode") {
        set_client_display_title(session_name, is_selfdev);
        return;
    }
    let prefix = if is_selfdev { "jcode:d:" } else { "jcode:c:" };
    set_title(format!("{prefix}{server_name}/{session_name}"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::lock_test_env;

    #[test]
    fn terminal_session_label_includes_custom_title_and_short_name() {
        assert_eq!(
            terminal_session_label("fox", Some("Release planning")),
            "Release planning (fox)"
        );
        assert_eq!(terminal_session_label("fox", Some("Fox")), "Fox");
        assert_eq!(terminal_session_label("fox", None), "Fox");
    }

    #[test]
    fn terminal_window_title_omits_the_icon_when_there_is_none() {
        // Herdr renders its own state_icon column, so a second emoji in the
        // title says nothing and costs ~2 of the 48 chars the row gets.
        assert_eq!(
            terminal_window_title(None, Some("DI-2507 Chat Endpoint"), None, false),
            "DI-2507 Chat Endpoint"
        );
        assert_eq!(
            terminal_window_title(None, None, Some("jcode Octopus"), false),
            "jcode Octopus"
        );
        assert_eq!(
            terminal_window_title(None, Some("Release planning"), None, true),
            "Release planning [self-dev]"
        );
        assert_eq!(terminal_window_title(None, None, None, false), "");
    }

    #[test]
    fn terminal_window_title_omits_product_and_animal_names() {
        assert_eq!(
            terminal_window_title(
                Some("🐙"),
                Some("resume window title"),
                Some("jcode Octopus"),
                false
            ),
            "🐙 resume window title"
        );
        assert_eq!(
            terminal_window_title(Some("🐙"), None, Some("jcode Octopus"), false),
            "🐙 jcode Octopus"
        );
        assert_eq!(
            terminal_window_title(
                Some("🐙"),
                Some("resume window title"),
                Some("jcode Octopus"),
                true
            ),
            "🐙 resume window title [self-dev]"
        );
    }

    #[test]
    fn terminal_session_label_for_id_reads_custom_title_from_session() {
        let _guard = lock_test_env();
        let previous_home = std::env::var_os("JCODE_HOME");
        let temp = tempfile::tempdir().expect("temp dir");
        crate::env::set_var("JCODE_HOME", temp.path());

        let mut session = crate::session::Session::create_with_id(
            "session_fox_123".to_string(),
            None,
            Some("Generated title".to_string()),
        );
        session.rename_title(Some("Release planning".to_string()));
        session.save().expect("save session");

        assert_eq!(
            terminal_session_label_for_id("session_fox_123"),
            "Release planning (fox)"
        );

        if let Some(previous_home) = previous_home {
            crate::env::set_var("JCODE_HOME", previous_home);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
    }

    #[test]
    fn terminal_session_label_for_id_prefers_todo_title_over_generated_title() {
        let _guard = lock_test_env();
        let previous_home = std::env::var_os("JCODE_HOME");
        let temp = tempfile::tempdir().expect("temp dir");
        crate::env::set_var("JCODE_HOME", temp.path());

        let session_id = "session_fox_456";
        let mut session = crate::session::Session::create_with_id(
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
                confidence: Some(crate::todo::ConfidenceState::Plausible),
                completion_confidence: None,
                confidence_history: Vec::new(),
                blocked_by: Vec::new(),
                assigned_to: None,
            }],
        )
        .expect("save todos");

        assert_eq!(
            terminal_session_label_for_id(session_id),
            "resume title sync (fox)"
        );

        if let Some(previous_home) = previous_home {
            crate::env::set_var("JCODE_HOME", previous_home);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
    }

    #[test]
    fn terminal_display_title_prefers_the_task_label_over_the_todo_title() {
        let _guard = lock_test_env();
        let previous_home = std::env::var_os("JCODE_HOME");
        let temp = tempfile::tempdir().expect("temp dir");
        crate::env::set_var("JCODE_HOME", temp.path());

        let session_id = "session_fox_789";
        let mut session = crate::session::Session::create_with_id(
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
        assert!(crate::task_label::store_model_label(
            session_id,
            "DI-2507 Chat Endpoint"
        ));

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
        assert!(crate::task_label::store_model_label(
            session_id,
            "Derived Name"
        ));

        assert_eq!(
            terminal_display_title_for_id(session_id).as_deref(),
            Some("Manual name")
        );

        match previous_home {
            Some(previous_home) => crate::env::set_var("JCODE_HOME", previous_home),
            None => crate::env::remove_var("JCODE_HOME"),
        }
    }
}
