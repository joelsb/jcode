// Three related behaviours that must not drift apart:
//
// 1. `/xx` (and `/quit`, `/exit`) exit jcode straight away.
// 2. The todo card moved from Alt+X to Alt+T.
// 3. Alt+X now exits immediately - unlike Ctrl+C, with no confirming press.
//
// Every test runs in its own `JCODE_HOME` because pressing a bound chord
// records hotkey usage in the ambient home, which leaks into the
// hotkey-familiarity tests depending on test order.

#[test]
fn slash_xx_exits_immediately() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        assert!(!app.should_quit);

        assert!(
            super::commands_dispatch::dispatch_local_command(&mut app, "/xx"),
            "/xx must be claimed by the shared dispatch table, not fall through to skills"
        );
        assert!(app.should_quit, "/xx must request exit");
    });
}

#[test]
fn slash_quit_and_slash_exit_are_the_same_operation() {
    with_temp_jcode_home(|| {
        for command in ["/quit", "/exit"] {
            let mut app = create_test_app();
            assert!(
                super::commands_dispatch::dispatch_local_command(&mut app, command),
                "{command} must be dispatched locally"
            );
            assert!(app.should_quit, "{command} must request exit");
        }
    });
}

/// A near-miss must not exit. `/xxy` is a skill-name shape, and quitting on it
/// would lose the session to a typo.
#[test]
fn a_command_that_merely_starts_with_xx_does_not_exit() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        assert!(
            !super::commands::handle_quit_command(&mut app, "/xxy"),
            "/xxy is not the quit command"
        );
        assert!(!app.should_quit, "/xxy must not exit jcode");
    });
}

#[test]
fn alt_x_exits_immediately_with_no_confirming_press() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        assert!(!app.should_quit);

        app.handle_key(KeyCode::Char('x'), KeyModifiers::ALT)
            .unwrap();

        assert!(
            app.should_quit,
            "Alt+X must exit on the first press, not arm a confirmation"
        );
        assert!(
            app.quit_pending.is_none(),
            "Alt+X is the immediate path; it must not go through the Ctrl+C two-press flow"
        );
    });
}

/// The chord and the command are one operation.
#[test]
fn alt_x_and_slash_xx_reach_the_same_state() {
    with_temp_jcode_home(|| {
        let mut by_key = create_test_app();
        by_key
            .handle_key(KeyCode::Char('x'), KeyModifiers::ALT)
            .unwrap();

        let mut by_command = create_test_app();
        assert!(super::commands::handle_quit_command(&mut by_command, "/xx"));

        assert_eq!(by_key.should_quit, by_command.should_quit);
        assert_eq!(by_key.quit_pending.is_some(), by_command.quit_pending.is_some());
    });
}

/// Alt+X used to show the todo card. That must be gone, or the old muscle
/// memory silently exits the session instead of showing todos... which is the
/// intended change, so the guard is that Alt+X does NOT push a card.
#[test]
fn alt_x_no_longer_shows_the_todo_card() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();

        app.handle_key(KeyCode::Char('x'), KeyModifiers::ALT)
            .unwrap();

        assert!(
            !app.display_messages.iter().any(|m| m.role == "todos"),
            "Alt+X is the quit chord now; it must not also toggle the todo card"
        );
    });
}

#[test]
fn alt_t_shows_the_todo_card() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        assert!(!app.display_messages.iter().any(|m| m.role == "todos"));

        app.handle_key(KeyCode::Char('t'), KeyModifiers::ALT)
            .unwrap();

        assert!(
            app.display_messages.iter().any(|m| m.role == "todos"),
            "Alt+T must show the inline todo card"
        );
        assert!(
            !app.should_quit,
            "Alt+T must not exit jcode; only Alt+X does"
        );
    });
}

/// Alt+T was the diagram-pane toggle before the todo card took it, so the
/// diagram pane needs its own chord or it becomes unreachable.
#[test]
fn the_diagram_pane_chord_moved_off_alt_t() {
    let toggles = crate::tui::keybind::load_toggle_keys();
    let diagram = toggles
        .diagram_pane
        .binding()
        .expect("the diagram pane must stay bound to something");
    assert_ne!(
        (diagram.code, diagram.modifiers),
        (KeyCode::Char('t'), KeyModifiers::ALT),
        "Alt+T now shows the todo card; the diagram pane must live elsewhere"
    );

    let todo = toggles
        .todo_card
        .binding()
        .expect("the todo card must stay bound");
    assert_eq!(
        (todo.code, todo.modifiers),
        (KeyCode::Char('t'), KeyModifiers::ALT),
        "the todo card default is Alt+T"
    );

    let quit = toggles
        .quit_now
        .binding()
        .expect("quit_now must be bound by default");
    assert_eq!(
        (quit.code, quit.modifiers),
        (KeyCode::Char('x'), KeyModifiers::ALT),
        "the immediate-exit default is Alt+X"
    );
}

/// `/xx` has to be discoverable: an unregistered command gets no completion and
/// no `/help` line, which is how a working command stays invisible.
#[test]
fn xx_is_registered_for_completion_and_help() {
    let registered = super::state_ui_input_helpers::registered_command_entries()
        .any(|(name, _)| name == "/xx");
    assert!(registered, "/xx must appear in the command registry");
}
