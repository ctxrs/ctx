use super::*;

#[tokio::test]
async fn event_loop_persists_assistant_complete_after_completed_terminal_event() {
    let data_dir = tempdir().expect("temp dir");
    let fixture = build_loop_fixture(data_dir.path(), "fake", "model").await;

    let (ev_tx, ev_rx) = mpsc::channel(8);
    let (events_done_tx, events_done_rx) = oneshot::channel();
    let (start_progress_tx, _start_progress_rx) =
        tokio::sync::watch::channel(TurnStartProgress::Pending);
    let loop_task = tokio::spawn(run_turn_event_loop(TurnEventLoop {
        host_weak: fixture
            .state
            .session_scheduler_worker_host
            .worker_host()
            .event_loop_host_weak(),
        store: fixture.store.clone(),
        session_id: fixture.session_id,
        task_id: fixture.task_id,
        workspace_id: fixture.workspace_id,
        worktree_id: fixture.worktree_id,
        provider_id: "fake".to_string(),
        model_id: "model".to_string(),
        session_root_kind: "primary".to_string(),
        execution_environment_label: "host".to_string(),
        perf_run_id: None,
        workdir_root: fixture.workspace_root.clone(),
        workdir_canonical: Some(fixture.workspace_root.clone()),
        workdir_str: fixture.workspace_root.to_string_lossy().to_string(),
        run_started_at: Instant::now(),
        run_id: fixture.run_id,
        turn_id: fixture.turn_id,
        message_id: fixture.message_id,
        provider_session_ref: None,
        codex_home: None,
        context_window_metrics: None,
        ev_rx,
        events_done_tx,
        start_progress_tx,
        order_seq_state: Arc::new(Mutex::new(OrderSeqState::new(1))),
    }));

    fixture
        .store
        .persist_turn_terminal_events(
            fixture.session_id,
            Some(fixture.run_id),
            fixture.turn_id,
            vec![(
                SessionEventType::TurnFinished,
                json!({"status": "completed"}),
            )],
        )
        .await
        .expect("persist completed terminal event before assistant complete");

    ev_tx
        .send(NormalizedEvent {
            event_type: SessionEventType::AssistantComplete,
            payload_json: json!({
                "full_content": "late assistant answer",
                "message_id": "provider-message-1",
                "order_seq": 2,
            }),
        })
        .await
        .expect("send late assistant complete");
    drop(ev_tx);

    events_done_rx.await.expect("event loop completion");
    loop_task.await.expect("event loop join");

    let messages = fixture
        .store
        .list_messages_for_session(fixture.session_id)
        .await
        .expect("load messages");
    assert!(messages.iter().any(|message| {
        matches!(message.role, ctx_core::models::MessageRole::Assistant)
            && message.content == "late assistant answer"
    }));

    let events = fixture
        .store
        .list_session_events_for_turn(fixture.session_id, fixture.turn_id, false)
        .await
        .expect("load turn events");
    assert!(events.iter().any(|event| {
        matches!(event.event_type, SessionEventType::AssistantMessageInserted)
            && event
                .payload_json
                .get("content")
                .and_then(|value| value.as_str())
                == Some("late assistant answer")
    }));
}
