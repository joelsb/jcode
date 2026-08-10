//! The out-of-band opt-in must actually work.
//!
//! `JCODE_MULTIPLEXER_IMAGES=1` is the documented escape hatch for users who
//! want images inside a multiplexer and accept the repaint artifacts. If it
//! silently did nothing, the advice in ADR-0014 and the flicker fix would be a
//! dead end.
//!
//! Own test binary: the picker is a process-global `OnceLock`.

#[test]
fn opt_in_restores_images_for_wezterm_inside_tmux() {
    unsafe {
        std::env::set_var("TERM", "tmux-256color");
        std::env::set_var("TERM_PROGRAM", "tmux");
        std::env::set_var("TMUX", "/tmp/tmux-501/default,1,0");
        std::env::set_var(
            "WEZTERM_EXECUTABLE",
            "/Applications/WezTerm.app/Contents/MacOS/wezterm-gui",
        );
        std::env::set_var("WEZTERM_PANE", "0");
        std::env::set_var("JCODE_MULTIPLEXER_IMAGES", "1");
        std::env::remove_var("GHOSTTY_RESOURCES_DIR");
        std::env::remove_var("GHOSTTY_BIN_DIR");
        std::env::remove_var("KITTY_WINDOW_ID");
        std::env::remove_var("LC_TERMINAL");
        std::env::remove_var("JCODE_MERMAID_PICKER_PROBE");
    }

    jcode_tui_mermaid::init_picker();

    assert!(
        jcode_tui_mermaid::native_image_protocol_available(),
        "JCODE_MULTIPLEXER_IMAGES=1 must restore images, got {:?}",
        jcode_tui_mermaid::protocol_type()
    );
}
