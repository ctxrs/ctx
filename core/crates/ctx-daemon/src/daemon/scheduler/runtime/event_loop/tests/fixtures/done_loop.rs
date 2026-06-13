use super::*;

pub(in crate::daemon::scheduler::runtime::event_loop::tests) async fn run_done_event_loop(
    fixture: LoopFixture,
    provider_id: &str,
    model_id: &str,
    provider_session_ref: &str,
    codex_home: Option<&Path>,
) -> (LoopFixture, SessionTurn) {
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
        provider_id: provider_id.to_string(),
        model_id: model_id.to_string(),
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
        provider_session_ref: Some(provider_session_ref.to_string()),
        codex_home: codex_home.map(|path| path.to_path_buf()),
        context_window_metrics: None,
        ev_rx,
        events_done_tx,
        start_progress_tx,
        order_seq_state: Arc::new(Mutex::new(OrderSeqState::new(1))),
    }));

    ev_tx
        .send(NormalizedEvent {
            event_type: SessionEventType::Done,
            payload_json: json!({}),
        })
        .await
        .expect("send done event");
    drop(ev_tx);

    events_done_rx.await.expect("event loop completion");
    loop_task.await.expect("event loop join");
    crate::daemon::scheduler::terminal::finalize_completed_turn(
        &fixture.state,
        fixture.session_id,
        Some(fixture.run_id),
        fixture.turn_id,
        fixture.message_id,
    )
    .await
    .expect("finalize completed turn");

    let turn = fixture
        .store
        .get_session_turn(fixture.session_id, fixture.turn_id)
        .await
        .expect("load turn")
        .expect("turn exists");
    (fixture, turn)
}
