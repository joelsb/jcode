// `/clear` must drop the discarded session's transcript images.
//
// Symptom: after `/clear`, every screenshot from the previous session was
// re-rendered under every new message, forever, and re-sent to the model as
// visual context. Cause: both `/clear` implementations cleared
// `display_messages` and `pending_images` but never
// `remote_side_pane_images`. An image whose anchor message no longer exists
// deliberately falls back to the trailing "unplaced" inline section
// (`ui_prepare.rs`) so nothing silently disappears, and after a clear that
// safety net catches the entire old image set.
//
// There are two `/clear` implementations. The one users hit in the product TUI
// is the `RemoteClient` handler in `remote/key_handling.rs`; the
// `reset_current_session` path in `commands_review.rs` runs under replay, the
// test harness, or a dropped server connection. Both funnel through
// `clear_transcript_images_for_new_session`, so cover the shared seam plus the
// caller that can be driven directly from a test.

use crate::tui::app::tests::create_test_app;

fn image_from_old_session(label: &str) -> crate::session::RenderedImage {
    crate::session::RenderedImage {
        media_type: "image/png".to_string(),
        // Not a real PNG: nothing in these assertions decodes the payload, and
        // a distinct string per image is what makes the set observable.
        data: format!("payload-{label}"),
        label: Some(label.to_string()),
        source: crate::session::RenderedImageSource::UserInput,
        // Anchored at a message index that will not exist after the clear.
        // This is exactly the state that produced the bug.
        anchor: None,
    }
}

#[test]
fn clear_transcript_images_for_new_session_drops_every_image() {
    let mut app = create_test_app();
    app.remote_side_pane_images = vec![
        image_from_old_session("shot-one"),
        image_from_old_session("shot-two"),
    ];
    app.interleave_images = vec![("image/png".to_string(), "pending-payload".to_string())];
    assert_eq!(
        app.remote_side_pane_images.len(),
        2,
        "precondition: the old session's images should be staged"
    );

    crate::tui::app::commands_review::clear_transcript_images_for_new_session(&mut app);

    assert!(
        app.remote_side_pane_images.is_empty(),
        "/clear must drop the discarded session's transcript images"
    );
    assert!(
        app.interleave_images.is_empty(),
        "/clear must drop images staged for an interleaved send"
    );
}

#[test]
fn reset_current_session_drops_transcript_images() {
    let mut app = create_test_app();
    app.remote_side_pane_images = vec![image_from_old_session("shot-one")];

    crate::tui::app::commands_review::reset_current_session(&mut app);

    assert!(
        app.remote_side_pane_images.is_empty(),
        "reset_current_session must clear transcript images, or old screenshots \
         re-render under every new message"
    );
}

#[test]
fn image_set_signature_is_recomputed_after_a_clear() {
    // The signature is cached and only recomputed when an image mutation
    // invalidates it. A clear that forgot to invalidate would leave the
    // prepared-frame caches keyed on the old, non-empty image set, so the
    // stale images would keep being drawn even though the vector is empty.
    use crate::tui::TuiState as _;

    let mut app = create_test_app();
    app.remote_side_pane_images = vec![image_from_old_session("shot-one")];
    let before = app.side_pane_images_signature();
    assert_eq!(before.0, 1, "precondition: one image in the signature");

    crate::tui::app::commands_review::clear_transcript_images_for_new_session(&mut app);

    assert_eq!(
        app.side_pane_images_signature().0,
        0,
        "the cached image signature must be invalidated by a clear"
    );
}

/// The remote `/clear` handler is the path users actually hit, and it is inline
/// in a very large async key handler that a unit test cannot drive. Pin it by
/// source instead, the same way the shared-dispatch-table test does: it must
/// call the shared helper rather than hand-rolling its own image reset.
#[test]
fn remote_clear_handler_calls_the_shared_image_reset() {
    let source = include_str!("../remote/key_handling.rs");
    assert!(
        source.contains("clear_transcript_images_for_new_session"),
        "the remote /clear handler must clear transcript images through the \
         shared helper, or old images survive the clear in the product TUI"
    );
}
