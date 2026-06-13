use super::*;

#[tokio::test]
async fn event_loop_drops_provider_events_after_turn_terminalized_by_store() {
    let data_dir = tempdir().expect("temp dir");
    let fixture = build_loop_fixture(data_dir.path(), "fake", "model").await;

    crate::daemon::scheduler::terminal::finalize_failed_turn(
        &fixture.state,
        fixture.session_id,
        Some(fixture.run_id),
        fixture.turn_id,
        fixture.message_id,
        crate::daemon::scheduler::terminal::FailedTurnTerminalization {
            message: "provider usage limit exceeded",
            reason: Some("usage_limit"),
            details: None,
            kind: Some(json!("usageLimitExceeded")),
        },
    )
    .await
    .expect("terminalize turn");

    let terminal_events = fixture
        .store
        .list_session_events_for_turn(fixture.session_id, fixture.turn_id, false)
        .await
        .expect("load terminal events");
    assert_eq!(terminal_events.len(), 1);
    assert!(terminal_events.iter().any(|event| {
        matches!(event.event_type, SessionEventType::TurnFinished)
            && event
                .payload_json
                .get("status")
                .and_then(|value| value.as_str())
                == Some("failed")
    }));

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

    ev_tx
        .send(NormalizedEvent {
            event_type: SessionEventType::ToolCall,
            payload_json: json!({
                "tool_call_id": "post-terminal-tool",
                "status": "running",
                "toolCall": {
                    "name": "Bash",
                    "kind": "execute"
                },
                "rawInput": {
                    "command": "pwd"
                }
            }),
        })
        .await
        .expect("send post-terminal tool event");
    ev_tx
        .send(NormalizedEvent {
            event_type: SessionEventType::AssistantComplete,
            payload_json: json!({"full_content": "late answer"}),
        })
        .await
        .expect("send post-terminal assistant event");
    drop(ev_tx);

    events_done_rx.await.expect("event loop completion");
    loop_task.await.expect("event loop join");

    let events = fixture
        .store
        .list_session_events_for_turn(fixture.session_id, fixture.turn_id, false)
        .await
        .expect("load persisted events");
    assert_eq!(events.len(), terminal_events.len());
    assert!(events.iter().all(|event| {
        !matches!(
            event.event_type,
            SessionEventType::ToolCall | SessionEventType::AssistantComplete
        )
    }));

    let turn = fixture
        .store
        .get_session_turn(fixture.session_id, fixture.turn_id)
        .await
        .expect("load turn")
        .expect("turn exists");
    assert_eq!(turn.status, SessionTurnStatus::Failed);
    assert_eq!(turn.tool_total, 0);
    assert_eq!(turn.tool_running, 0);
}
