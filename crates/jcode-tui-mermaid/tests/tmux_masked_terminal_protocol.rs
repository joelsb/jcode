//! Guards for image-protocol selection when a multiplexer masks the outer
//! terminal.
//!
//! tmux sets `TERM_PROGRAM=tmux` and `TERM=tmux-256color`, so the generic
//! signals carry no graphics hint. The terminal's own markers survive, and
//! `Ghostty` behind tmux must still get the Kitty protocol: Kitty draws through
//! Unicode placeholders that occupy real cells, so a tmux repaint reissues them
//! and the image survives.
//!
//! The WezTerm-behind-tmux counterpart lives in its own test binary
//! (`tmux_masked_terminal_out_of_band.rs`) because the picker is a
//! process-global `OnceLock`: the first test to touch it decides the outcome
//! for the whole process.

#[test]
fn ghostty_inside_tmux_gets_the_repaint_safe_protocol() {
    unsafe {
        std::env::set_var("TERM", "tmux-256color");
        std::env::set_var("TERM_PROGRAM", "tmux");
        std::env::set_var("TMUX", "/tmp/tmux-501/default,1,0");
        std::env::set_var("GHOSTTY_RESOURCES_DIR", "/usr/share/ghostty");
        std::env::remove_var("WEZTERM_EXECUTABLE");
        std::env::remove_var("WEZTERM_PANE");
        std::env::remove_var("KITTY_WINDOW_ID");
        std::env::remove_var("LC_TERMINAL");
        std::env::remove_var("JCODE_MERMAID_PICKER_PROBE");
        std::env::remove_var("JCODE_MULTIPLEXER_IMAGES");
    }

    jcode_tui_mermaid::init_picker();

    assert!(
        jcode_tui_mermaid::native_image_protocol_available(),
        "Ghostty behind tmux must expose a native image protocol, got {:?}",
        jcode_tui_mermaid::protocol_type()
    );
    assert!(
        !jcode_tui_mermaid::uses_text_image_fallback(),
        "mermaid must not fall back to source text for Ghostty behind tmux"
    );
}
