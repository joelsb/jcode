//! `display.mermaid_halfblocks` must make the half-block raster count as a way
//! to draw a diagram.
//!
//! Half-blocks are colored text cells, so they are the only image path that
//! survives a multiplexer repaint. Without this opt-in, a terminal seen from
//! inside tmux gets mermaid source forever.

use ratatui_image::picker::ProtocolType;

#[test]
fn halfblocks_count_as_drawable_only_when_opted_in() {
    jcode_tui_mermaid::force_test_halfblocks_picker();
    assert_eq!(
        jcode_tui_mermaid::protocol_type(),
        Some(ProtocolType::Halfblocks),
        "precondition: this test is about the halfblocks-only terminal"
    );

    jcode_tui_mermaid::set_halfblocks_allowed(false);
    assert!(
        !jcode_tui_mermaid::native_image_protocol_available(),
        "off: halfblocks must not count, so mermaid shows source"
    );
    assert!(
        jcode_tui_mermaid::uses_text_image_fallback(),
        "off: the text-fallback note applies"
    );

    jcode_tui_mermaid::set_halfblocks_allowed(true);
    assert!(
        jcode_tui_mermaid::native_image_protocol_available(),
        "on: halfblocks must count as drawable, or the option does nothing"
    );
    assert!(
        !jcode_tui_mermaid::uses_text_image_fallback(),
        "on: images are being drawn, so there is no text fallback to warn about"
    );

    // Leave the process-global flag as found.
    jcode_tui_mermaid::set_halfblocks_allowed(false);
}
