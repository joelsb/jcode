//! End-to-end guard for the real failure this covers: running jcode inside tmux
//! under WezTerm produced no image protocol, so every mermaid diagram rendered
//! as raw source text.
//!
//! tmux sets `TERM_PROGRAM=tmux` and `TERM=tmux-256color`, masking the outer
//! terminal. The terminal's own markers (`WEZTERM_EXECUTABLE`, `WEZTERM_PANE`)
//! survive into the child environment and must recover the protocol.
//!
//! This runs in its own test binary because the picker is a process-global
//! `OnceLock`: any other test that touches it first would decide the outcome.

#[test]
fn wezterm_inside_tmux_still_gets_an_image_protocol() {
    unsafe {
        std::env::set_var("TERM", "tmux-256color");
        std::env::set_var("TERM_PROGRAM", "tmux");
        std::env::set_var("TMUX", "/tmp/tmux-501/default,1,0");
        std::env::set_var(
            "WEZTERM_EXECUTABLE",
            "/Applications/WezTerm.app/Contents/MacOS/wezterm-gui",
        );
        std::env::set_var("WEZTERM_PANE", "0");
        std::env::remove_var("KITTY_WINDOW_ID");
        std::env::remove_var("LC_TERMINAL");
        std::env::remove_var("JCODE_MERMAID_PICKER_PROBE");
    }

    jcode_tui_mermaid::init_picker();

    assert!(
        jcode_tui_mermaid::native_image_protocol_available(),
        "WezTerm behind tmux must expose a native image protocol, got {:?}",
        jcode_tui_mermaid::protocol_type()
    );
    assert!(
        !jcode_tui_mermaid::uses_text_image_fallback(),
        "mermaid must not fall back to source text for WezTerm behind tmux"
    );
}
