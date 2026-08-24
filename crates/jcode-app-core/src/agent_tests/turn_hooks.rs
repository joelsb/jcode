
/// A provider that ends the turn immediately with a short text reply.
struct ImmediateTextProvider;

#[async_trait]
impl Provider for ImmediateTextProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let (tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(4);
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(StreamEvent::TextDelta("ok".to_string())))
                .await;
            let _ = tx
                .send(Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                }))
                .await;
        });
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "openai"
    }

    fn model(&self) -> String {
        "test-model".to_string()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

/// The non-streaming turn path must fire `turn_end`.
///
/// `jcode run`, ambient runs, debug jobs and swarm workers all go through
/// `run_once_capture` into the plain turn loop, which used to complete without
/// emitting any turn hook at all. Only the streaming path fired them. A
/// hook-driven status integration therefore saw a turn start and never finish,
/// and sat on "working" indefinitely.
#[cfg(unix)]
#[tokio::test]
async fn non_streaming_turn_fires_turn_end_hook() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let record = temp.path().join("turn_end.txt");
    let script = temp.path().join("observe.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s|%s' \"$JCODE_HOOK_EVENT\" \"$JCODE_HOOK_STATUS\" > {}\n",
            crate::terminal_launch::sh_escape(&record.to_string_lossy())
        ),
    )
    .expect("write script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let previous = std::env::var_os("JCODE_HOOK_TURN_END");
    crate::env::set_var("JCODE_HOOK_TURN_END", script.to_string_lossy().to_string());
    // config() serves a process-wide cache on a short throttle, so a freshly
    // set env override is invisible until the cache is invalidated. Passes in
    // isolation without this and fails in the full suite, where an earlier
    // test has already warmed the cache.
    crate::config::invalidate_config_cache();

    let provider: Arc<dyn Provider> = Arc::new(ImmediateTextProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    let _ = agent.run_once_capture("hello").await;

    let mut recorded = String::new();
    for _ in 0..100 {
        if let Ok(data) = std::fs::read_to_string(&record)
            && !data.is_empty()
        {
            recorded = data;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    match previous {
        Some(value) => crate::env::set_var("JCODE_HOOK_TURN_END", value),
        None => crate::env::remove_var("JCODE_HOOK_TURN_END"),
    }
    crate::config::invalidate_config_cache();

    assert_eq!(
        recorded, "turn_end|ok",
        "the non-streaming turn path must fire the turn_end hook"
    );
}
