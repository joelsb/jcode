//! WezTerm behind tmux must NOT silently switch to images.
//!
//! WezTerm's only viable path in ratatui-image is the iTerm2 protocol, which
//! writes an out-of-band escape with no cell content behind it. tmux repaints
//! constantly (status line, scroll, pane focus) and knows nothing about those
//! pixels, so diagrams flicker, duplicate, and smear. Readable mermaid source
//! beats an unusable flickering image, so the fallback must stay text unless the
//! user explicitly opts in.
//!
//! Own test binary: the picker is a process-global `OnceLock`, so the first test
//! to initialize it decides the outcome for the whole process.

#[test]
fn wezterm_inside_tmux_keeps_the_text_fallback_by_default() {
    unsafe {
        std::env::set_var("TERM", "tmux-256color");
        std::env::set_var("TERM_PROGRAM", "tmux");
        std::env::set_var("TMUX", "/tmp/tmux-501/default,1,0");
        std::env::set_var(
            "WEZTERM_EXECUTABLE",
            "/Applications/WezTerm.app/Contents/MacOS/wezterm-gui",
        );
        std::env::set_var("WEZTERM_PANE", "0");
        std::env::remove_var("GHOSTTY_RESOURCES_DIR");
        std::env::remove_var("GHOSTTY_BIN_DIR");
        std::env::remove_var("KITTY_WINDOW_ID");
        std::env::remove_var("LC_TERMINAL");
        std::env::remove_var("JCODE_MERMAID_PICKER_PROBE");
        std::env::remove_var("JCODE_MULTIPLEXER_IMAGES");
    }

    jcode_tui_mermaid::init_picker();

    assert!(
        jcode_tui_mermaid::uses_text_image_fallback(),
        "WezTerm behind tmux must keep readable mermaid source, got protocol {:?}",
        jcode_tui_mermaid::protocol_type()
    );
    assert!(
        !jcode_tui_mermaid::native_image_protocol_available(),
        "an out-of-band protocol behind tmux must not count as usable"
    );
}
