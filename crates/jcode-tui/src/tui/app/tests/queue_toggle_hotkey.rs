// The queue hotkey (Ctrl+Q by default, `keybindings.queue_toggle`) must be the
// same operation as `/queue`, not a lookalike. The predecessor binding
// (Ctrl+T/Ctrl+Tab) flipped only `queue_mode` and left
// `serialize_queued_messages` untouched, so pressing it produced a half-on
// state that `/queue status` then described incorrectly, and turning the queue
// off by chord silently stranded the messages already waiting.
//
// Every test here runs in its own `JCODE_HOME`. Pressing a bound chord records
// hotkey usage in the ambient home, and `rare_known_hotkey_sets_feedback_and_
// repeats_stop_once_familiar` asserts that Ctrl+Q is still *unfamiliar*. Without
// the temp home these presses made that sibling test fail depending on test
// order, which is exactly the kind of cross-test leak `with_temp_jcode_home`
// exists to stop.

/// Both flags move together in both directions, so `/queue status` always
/// agrees with what the chord just did.
#[test]
fn queue_hotkey_moves_both_queue_flags_together() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        app.queue_mode = false;
        app.serialize_queued_messages = false;

        app.handle_key(KeyCode::Char('q'), KeyModifiers::CONTROL)
            .unwrap();
        assert!(app.queue_mode, "Ctrl+Q should turn queue_mode on");
        assert!(
            app.serialize_queued_messages,
            "Ctrl+Q must also set serialize_queued_messages, or /queue status lies"
        );

        app.handle_key(KeyCode::Char('q'), KeyModifiers::CONTROL)
            .unwrap();
        assert!(!app.queue_mode, "second Ctrl+Q should turn queue_mode off");
        assert!(
            !app.serialize_queued_messages,
            "second Ctrl+Q must also clear serialize_queued_messages"
        );
    });
}

/// Turning the queue off by chord releases the waiting backlog, exactly as
/// `/queue off` does. Release has two shapes: idle, the messages stay in
/// `queued_messages` but `pending_queued_dispatch` tells the followup loop to
/// send them on its next tick; mid-turn, they are folded into
/// `interleave_message` and injected into the running turn. Either way nothing
/// is left parked behind a queue that no longer exists.
#[test]
fn queue_hotkey_off_releases_the_waiting_backlog_when_idle() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        app.queue_mode = true;
        app.serialize_queued_messages = true;
        app.is_processing = false;
        app.queued_messages.push("waiting one".to_string());
        app.queued_messages.push("waiting two".to_string());

        app.handle_key(KeyCode::Char('q'), KeyModifiers::CONTROL)
            .unwrap();

        assert!(!app.queue_mode);
        assert!(!app.serialize_queued_messages);
        assert!(
            app.pending_queued_dispatch,
            "an idle release must arm the followup dispatch, or the backlog never sends"
        );
    });
}

#[test]
fn queue_hotkey_off_releases_the_waiting_backlog_mid_turn() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        app.queue_mode = true;
        app.serialize_queued_messages = true;
        app.is_processing = true;
        app.queued_messages.push("waiting one".to_string());
        app.queued_messages.push("waiting two".to_string());

        app.handle_key(KeyCode::Char('q'), KeyModifiers::CONTROL)
            .unwrap();

        assert!(!app.queue_mode);
        assert!(!app.serialize_queued_messages);
        assert!(
            app.queued_messages.is_empty(),
            "a mid-turn release must empty the queue, still waiting: {:?}",
            app.queued_messages
        );
        let interleave = app
            .interleave_message
            .as_deref()
            .expect("released messages should become an interleave into the running turn");
        assert!(interleave.contains("waiting one"), "{interleave}");
        assert!(interleave.contains("waiting two"), "{interleave}");
    });
}

/// A half-on state (only `queue_mode` set, e.g. restored from an older build)
/// reads as off, so the next chord press turns the queue fully on instead of
/// appearing to do nothing.
#[test]
fn queue_hotkey_repairs_a_half_on_state_by_turning_fully_on() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        app.queue_mode = true;
        app.serialize_queued_messages = false;

        app.handle_key(KeyCode::Char('q'), KeyModifiers::CONTROL)
            .unwrap();

        assert!(app.queue_mode);
        assert!(
            app.serialize_queued_messages,
            "a half-on queue must resolve to fully ON, not fully OFF"
        );
    });
}

/// The chord and the slash command are one operation: pressing Ctrl+Q must
/// leave the app in the same state as typing `/queue`.
#[test]
fn queue_hotkey_and_slash_queue_reach_the_same_state() {
    with_temp_jcode_home(|| {
        let mut by_key = create_test_app();
        by_key.queue_mode = false;
        by_key.serialize_queued_messages = false;
        by_key
            .handle_key(KeyCode::Char('q'), KeyModifiers::CONTROL)
            .unwrap();

        let mut by_command = create_test_app();
        by_command.queue_mode = false;
        by_command.serialize_queued_messages = false;
        assert!(crate::tui::app::commands::handle_queue_command(
            &mut by_command,
            "/queue"
        ));

        assert_eq!(by_key.queue_mode, by_command.queue_mode);
        assert_eq!(
            by_key.serialize_queued_messages,
            by_command.serialize_queued_messages
        );
    });
}

/// The retired binding must be gone: Ctrl+T no longer touches the queue.
#[test]
fn ctrl_t_no_longer_toggles_the_queue() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        app.queue_mode = false;
        app.serialize_queued_messages = false;

        app.handle_key(KeyCode::Char('t'), KeyModifiers::CONTROL)
            .unwrap();

        assert!(
            !app.queue_mode,
            "Ctrl+T was removed as a queue toggle; it must not flip queue_mode"
        );
    });
}
