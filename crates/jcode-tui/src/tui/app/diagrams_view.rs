//! `/diagrams` - render the transcript's mermaid diagrams to PNG and open them.
//!
//! Terminals inside a multiplexer usually cannot show inline images (see
//! ADR-0014), so the transcript falls back to mermaid source text. That keeps
//! the information but not the picture, and a flowchart with twenty edges is
//! not something a person reads as source. This command closes that gap without
//! touching the inline rendering path: extract the mermaid blocks from the
//! transcript, render them with the same renderer the inline path uses, and hand
//! the PNGs to the OS image viewer.

use super::App;
use crate::tui::DisplayMessage;

/// One mermaid block found in the transcript, in transcript order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TranscriptDiagram {
    /// The mermaid source, without the fence lines.
    pub(super) source: String,
    /// Nearest preceding markdown heading, used as a human label.
    pub(super) label: Option<String>,
}

/// Extract fenced mermaid blocks from one markdown message.
///
/// Tracks the nearest preceding ATX heading so the user can tell which diagram
/// is which ("4. Update path") instead of getting "diagram 3 of 5". Fence
/// languages are accepted by `mermaid::is_mermaid_lang`, so this finds exactly
/// the blocks the inline renderer would have drawn, no more.
pub(super) fn extract_mermaid_blocks(markdown: &str) -> Vec<TranscriptDiagram> {
    let mut out: Vec<TranscriptDiagram> = Vec::new();
    let mut heading: Option<String> = None;
    let mut in_block = false;
    let mut current = String::new();

    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("```") {
            let lang = rest.trim();
            if in_block {
                // Any closing fence ends the block: a mermaid body has no
                // nested fences, so the first ``` we see is the terminator.
                if !current.trim().is_empty() {
                    out.push(TranscriptDiagram {
                        source: current.trim_end().to_string(),
                        label: heading.clone(),
                    });
                }
                current.clear();
                in_block = false;
            } else if crate::tui::mermaid::is_mermaid_lang(lang) {
                in_block = true;
                current.clear();
            }
            continue;
        }

        if in_block {
            current.push_str(line);
            current.push('\n');
            continue;
        }

        if let Some(title) = trimmed.strip_prefix('#') {
            let title = title.trim_start_matches('#').trim();
            if !title.is_empty() {
                heading = Some(title.to_string());
            }
        }
    }

    // An unterminated fence still holds a usable diagram: the turn may have
    // been interrupted mid-stream, and the user asked to see it.
    if in_block && !current.trim().is_empty() {
        out.push(TranscriptDiagram {
            source: current.trim_end().to_string(),
            label: heading,
        });
    }

    out
}

/// Collect the transcript's mermaid blocks, oldest first.
fn gather_transcript_diagrams(app: &App) -> Vec<TranscriptDiagram> {
    let mut out = Vec::new();
    for message in app.display_messages() {
        // Only prose carries diagrams; tool output and system notices do not,
        // and scanning them would surface jcode's own internal mermaid.
        if message.role != "assistant" && message.role != "user" {
            continue;
        }
        out.extend(extract_mermaid_blocks(&message.content));
    }
    out
}

/// Parse the `/diagrams` argument into a 1-based selection.
///
/// Returns `Err` for anything that is not empty, "last", or a positive integer,
/// so a typo reports usage instead of silently opening everything.
pub(super) fn parse_selection(arg: &str, total: usize) -> Result<Vec<usize>, String> {
    let arg = arg.trim();
    if arg.is_empty() || arg.eq_ignore_ascii_case("all") {
        return Ok((0..total).collect());
    }
    if arg.eq_ignore_ascii_case("last") {
        return Ok(total.checked_sub(1).map(|i| vec![i]).unwrap_or_default());
    }
    match arg.parse::<usize>() {
        Ok(0) => Err("Diagrams are numbered from 1.".to_string()),
        Ok(n) if n <= total => Ok(vec![n - 1]),
        Ok(n) => Err(format!(
            "No diagram {n}: this session has {total} diagram{}.",
            if total == 1 { "" } else { "s" }
        )),
        Err(_) => Err("Usage: /diagrams [all|last|<n>]".to_string()),
    }
}

pub(super) fn handle_diagrams_command(app: &mut App, trimmed: &str) -> bool {
    let arg = if trimmed == "/diagram" || trimmed == "/diagrams" {
        ""
    } else if let Some(rest) = trimmed
        .strip_prefix("/diagrams ")
        .or_else(|| trimmed.strip_prefix("/diagram "))
    {
        rest.trim()
    } else {
        return false;
    };

    let diagrams = gather_transcript_diagrams(app);
    if diagrams.is_empty() {
        app.push_display_message(DisplayMessage::system(
            "No mermaid diagrams in this session's transcript yet.".to_string(),
        ));
        return true;
    }

    if arg.eq_ignore_ascii_case("list") {
        let mut out = format!(
            "{} diagram{} in this session:\n",
            diagrams.len(),
            if diagrams.len() == 1 { "" } else { "s" }
        );
        for (i, diagram) in diagrams.iter().enumerate() {
            let label = diagram.label.as_deref().unwrap_or("(untitled)");
            let kind = diagram
                .source
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim()
                .to_string();
            out.push_str(&format!("  {}. {} · {}\n", i + 1, label, kind));
        }
        out.push_str("\nOpen one with /diagrams <n>, or all of them with /diagrams.");
        app.push_display_message(DisplayMessage::system(out));
        return true;
    }

    let selection = match parse_selection(arg, diagrams.len()) {
        Ok(selection) => selection,
        Err(error) => {
            app.push_display_message(DisplayMessage::error(error));
            return true;
        }
    };

    let width = crossterm::terminal::size().ok().map(|(w, _)| w);
    let mut opened = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for index in selection {
        let diagram = &diagrams[index];
        match crate::tui::mermaid::render_mermaid_sized(&diagram.source, width) {
            crate::tui::mermaid::RenderResult::Image { path, .. } => {
                match super::helpers::open_path_or_url_detached(&path) {
                    Ok(()) => opened += 1,
                    Err(error) => failures.push(format!(
                        "diagram {}: rendered to {} but could not open it ({error})",
                        index + 1,
                        path.display()
                    )),
                }
            }
            crate::tui::mermaid::RenderResult::Error(error) => {
                failures.push(format!("diagram {}: {error}", index + 1));
            }
        }
    }

    let mut summary = String::new();
    if opened > 0 {
        summary.push_str(&format!(
            "🖼️ Opened {opened} diagram{} in your image viewer.",
            if opened == 1 { "" } else { "s" }
        ));
    }
    for failure in &failures {
        if !summary.is_empty() {
            summary.push('\n');
        }
        summary.push_str(failure);
    }

    if opened > 0 {
        app.push_display_message(DisplayMessage::system(summary));
        app.set_status_notice(format!("Diagrams: opened {opened}"));
    } else {
        app.push_display_message(DisplayMessage::error(summary));
    }
    true
}

#[cfg(test)]
#[path = "diagrams_view_tests.rs"]
mod diagrams_view_tests;
