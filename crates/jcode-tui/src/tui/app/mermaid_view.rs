//! `/mermaid` - control how diagrams are drawn in the transcript.
//!
//! The interesting knob is `halfblocks`. Terminals without a native image
//! protocol (Kitty/iTerm2/Sixel), which includes most terminals seen from
//! inside tmux, otherwise fall back to showing mermaid *source*. ratatui-image
//! can still draw the diagram as Unicode half-blocks: coarse, but made of
//! ordinary colored text cells, so it is the one image path that survives a
//! multiplexer repaint. See docs/adr/0014.

use super::App;
use crate::tui::DisplayMessage;

/// Human-readable description of where diagrams currently stand.
fn status_line() -> String {
    let protocol = crate::tui::mermaid::protocol_type();
    let halfblocks = crate::tui::mermaid::halfblocks_allowed();
    let drawing = if crate::tui::mermaid::native_image_protocol_available() {
        match protocol {
            Some(p) => format!("images ({p:?})"),
            None => "images".to_string(),
        }
    } else {
        "mermaid source text".to_string()
    };
    format!(
        "Diagrams: {drawing}\nTerminal image protocol: {}\nHalf-block fallback: {}",
        protocol
            .map(|p| format!("{p:?}"))
            .unwrap_or_else(|| "none detected".to_string()),
        if halfblocks { "on" } else { "off" },
    )
}

pub(super) fn handle_mermaid_command(app: &mut App, trimmed: &str) -> bool {
    let arg = if trimmed == "/mermaid" {
        ""
    } else if let Some(rest) = trimmed.strip_prefix("/mermaid ") {
        rest.trim()
    } else {
        return false;
    };

    let lower = arg.to_ascii_lowercase();
    let (subcommand, value) = match lower.split_once(char::is_whitespace) {
        Some((head, tail)) => (head.to_string(), tail.trim().to_string()),
        None => (lower.clone(), String::new()),
    };

    match subcommand.as_str() {
        "" | "status" => {
            let mut out = status_line();
            out.push_str(
                "\n\nUse /mermaid halfblocks on to draw diagrams as coarse colored blocks \
                 (works inside tmux), or /diagrams to open them full-size in an image viewer.",
            );
            app.push_display_message(DisplayMessage::system(out));
        }
        "halfblocks" | "blocks" => {
            let enabled = match value.as_str() {
                "" | "toggle" => !crate::tui::mermaid::halfblocks_allowed(),
                "on" | "true" | "1" | "yes" => true,
                "off" | "false" | "0" | "no" => false,
                _ => {
                    app.push_display_message(DisplayMessage::error(
                        "Usage: /mermaid halfblocks [on|off]".to_string(),
                    ));
                    return true;
                }
            };

            crate::tui::mermaid::set_halfblocks_allowed(enabled);
            // The markdown renderer decides source-vs-image at render time and
            // caches the result against the message text, which has not
            // changed. Bumping the display-messages version is what forces the
            // transcript to be prepared again under the new decision.
            app.bump_display_messages_version();

            match crate::config::Config::set_mermaid_halfblocks(enabled) {
                Ok(()) => {
                    let mut out = format!(
                        "Half-block diagrams: {}. {}",
                        if enabled { "ON" } else { "OFF" },
                        if enabled {
                            "Diagrams now draw inline as colored blocks. They are coarse, so use \
                             /diagrams for a full-resolution view."
                        } else {
                            "Diagrams fall back to mermaid source; use /diagrams to view them as \
                             images."
                        }
                    );
                    out.push_str("\n\n");
                    out.push_str(&status_line());
                    app.push_display_message(DisplayMessage::system(out));
                    app.set_status_notice(format!(
                        "Half-block diagrams: {}",
                        if enabled { "ON" } else { "OFF" }
                    ));
                }
                Err(error) => {
                    app.push_display_message(DisplayMessage::error(format!(
                        "Applied for this session, but could not save display.mermaid_halfblocks: {error}"
                    )));
                }
            }
        }
        _ => {
            app.push_display_message(DisplayMessage::error(
                "Usage: /mermaid [status|halfblocks [on|off]]".to_string(),
            ));
        }
    }

    true
}
