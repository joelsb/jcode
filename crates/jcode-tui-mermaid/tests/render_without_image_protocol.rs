//! `/diagrams` must produce a real PNG even when no image protocol is
//! available, because that is exactly the situation it exists for: inside a
//! multiplexer the inline path falls back to text, and this command is the way
//! the user still gets a picture. If rendering were gated on protocol
//! availability, the command would be silently useless in its only use case.

#[test]
fn renders_a_png_without_any_image_protocol() {
    // No picker init, no protocol markers: the text-fallback situation.
    assert!(
        !jcode_tui_mermaid::native_image_protocol_available(),
        "precondition: this test is about the no-protocol case"
    );

    let source = "flowchart TD\n  A[parent] --> B[child]\n";
    match jcode_tui_mermaid::render_mermaid_sized(source, Some(120)) {
        jcode_tui_mermaid::RenderResult::Image {
            path,
            width,
            height,
            ..
        } => {
            assert!(path.exists(), "rendered PNG must exist on disk: {path:?}");
            let bytes = std::fs::metadata(&path).expect("stat PNG").len();
            assert!(bytes > 1000, "PNG suspiciously small ({bytes} bytes)");
            assert!(width > 0 && height > 0, "got {width}x{height}");
            let header = std::fs::read(&path).expect("read PNG");
            assert_eq!(&header[..4], b"\x89PNG", "not a PNG: {:?}", &header[..4]);
        }
        jcode_tui_mermaid::RenderResult::Error(error) => {
            panic!("render failed with no image protocol: {error}");
        }
    }
}
