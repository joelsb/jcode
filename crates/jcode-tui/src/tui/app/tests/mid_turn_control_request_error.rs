// A control request rejected server-side while a turn is running must not be
// charged to that turn.
//
// The server rejects `rename_session` / `clear` / `set_feature` / `transfer` /
// `set_subagent_model` / `trigger_memory_extraction` while the agent is busy,
// via `send_agent_busy_error`, which sets `retry_after_secs: Some(1)`. The
// client's `ServerEvent::Error` arm ignored the request id, so that rejection
// was read as the *running turn* being rate limited: it printed "Rate limit
// hit. Will auto-retry in 1 seconds", switched Anthropic account, marked the
// live turn idle, and re-queued the last user message - which is how a
// mid-turn `/rename` produced "+1 queued" with the queue explicitly OFF.

#[test]
fn mid_turn_control_request_rejection_does_not_rate_limit_or_requeue_the_running_turn() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    // A real turn is in flight, message id 7, with its payload retained for
    // retry the way `begin_remote_send` leaves it.
    app.is_processing = true;
    app.status = ProcessingStatus::Streaming;
    app.current_message_id = Some(7);
    app.rate_limit_pending_message = Some(PendingRemoteMessage {
        content: "create a plan to do this".to_string(),
        images: vec![],
        is_system: false,
        system_reminder: None,
        auto_retry: false,
        retry_attempts: 0,
        retry_at: None,
    });

    // The user typed `/rename backend-chatbot` mid-turn. That is request id 8,
    // and the server declines it because the agent is busy.
    app.handle_server_event(
        crate::protocol::ServerEvent::Error {
            id: 8,
            message: "Cannot handle rename_session while the session is busy. \
                      Try again after the current turn finishes."
                .to_string(),
            retry_after_secs: Some(1),
        },
        &mut remote,
    );

    assert!(
        app.is_processing,
        "the running turn must stay running: the rename was rejected, not the turn"
    );
    assert_eq!(
        app.current_message_id,
        Some(7),
        "the running turn's message id must survive an unrelated request error"
    );
    assert!(
        app.rate_limit_reset.is_none(),
        "a busy-control rejection is not a rate limit; retry_after_secs=1 on \
         someone else's request must not arm the rate-limit timer"
    );
    assert!(
        app.queued_messages().is_empty(),
        "nothing may be queued: with the queue off the user queued nothing, got {:?}",
        app.queued_messages()
    );
    assert!(
        app.rate_limit_pending_message.is_some(),
        "the running turn's payload must not be consumed by an unrelated error"
    );
    assert!(
        app.interleave_message.is_none(),
        "no interleave may be synthesized from an unrelated request error"
    );
    assert!(
        app.display_messages()
            .iter()
            .any(|message| message.role == "error"
                && message.content.contains("rename_session")),
        "the rejection must still be reported to the user"
    );
}

// The same-id case must keep working: an Error carrying the running turn's own
// id is a real turn failure and must still be handled by the turn-failure path.
#[test]
fn same_id_error_is_still_treated_as_a_turn_failure() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.is_processing = true;
    app.status = ProcessingStatus::Streaming;
    app.current_message_id = Some(7);

    app.handle_server_event(
        crate::protocol::ServerEvent::Error {
            id: 7,
            message: "upstream exploded".to_string(),
            retry_after_secs: None,
        },
        &mut remote,
    );

    assert!(
        !app.is_processing,
        "an error for the running turn's own id must settle the turn"
    );
}
