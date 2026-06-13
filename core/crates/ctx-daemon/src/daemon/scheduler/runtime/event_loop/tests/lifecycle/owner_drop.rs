use super::*;

#[tokio::test]
async fn event_loop_exits_without_persisting_when_host_owner_is_gone() {
    let data_dir = tempdir().expect("temp dir");
    let fixture = build_loop_fixture(data_dir.path(), "fake", "model").await;
    let LoopFixture {
        state,
        store,
        workspace_id,
        worktree_id,
        task_id,
        session_id,
        turn_id,
        run_id,
        message_id,
        workspace_root,
    } = fixture;

    let (ev_tx, ev_rx) = mpsc::channel(8);
    let (events_done_tx, events_done_rx) = oneshot::channel();
    let (start_progress_tx, _start_progress_rx) =
        tokio::sync::watch::channel(TurnStartProgress::Pending);
    let host_weak = state
        .session_scheduler_worker_host
        .worker_host()
        .event_loop_host_weak();
    let loop_task = tokio::spawn(run_turn_event_loop(TurnEventLoop {
        host_weak,
        store: store.clone(),
        session_id,
        task_id,
        workspace_id,
        worktree_id,
        provider_id: "fake".to_string(),
        model_id: "model".to_string(),
        session_root_kind: "primary".to_string(),
        execution_environment_label: "host".to_string(),
        perf_run_id: None,
        workdir_root: workspace_root.clone(),
        workdir_canonical: Some(workspace_root.clone()),
        workdir_str: workspace_root.to_string_lossy().to_string(),
        run_started_at: Instant::now(),
        run_id,
        turn_id,
        message_id,
        provider_session_ref: None,
        codex_home: None,
        context_window_metrics: None,
        ev_rx,
        events_done_tx,
        start_progress_tx,
        order_seq_state: Arc::new(Mutex::new(OrderSeqState::new(1))),
    }));

    drop(state);

    ev_tx
        .send(NormalizedEvent {
            event_type: SessionEventType::TurnStarted,
            payload_json: json!({}),
        })
        .await
        .expect("send event after dropping host owner");
    drop(ev_tx);

    events_done_rx.await.expect("event loop completion");
    loop_task.await.expect("event loop join");

    let events = store
        .list_session_events_for_turn(session_id, turn_id, false)
        .await
        .expect("load persisted events");
    assert!(events.is_empty());
}
