use super::*;

impl ToolEventLoopFixture {
    pub(in crate::daemon::scheduler::runtime::event_loop::tests::tools) async fn run_event(
        &self,
        event: NormalizedEvent,
    ) {
        let (ev_tx, ev_rx) = mpsc::channel(8);
        let (events_done_tx, events_done_rx) = oneshot::channel();
        let (start_progress_tx, _start_progress_rx) =
            tokio::sync::watch::channel(TurnStartProgress::Pending);
        let loop_task = tokio::spawn(run_turn_event_loop(TurnEventLoop {
            host_weak: self
                .state
                .session_scheduler_worker_host
                .worker_host()
                .event_loop_host_weak(),
            store: self.store.clone(),
            session_id: self.session_id,
            task_id: self.task_id,
            workspace_id: self.workspace_id,
            worktree_id: self.worktree_id,
            provider_id: "fake".to_string(),
            model_id: "model".to_string(),
            session_root_kind: "primary".to_string(),
            execution_environment_label: "host".to_string(),
            perf_run_id: None,
            workdir_root: self.workspace_root.clone(),
            workdir_canonical: Some(self.workspace_root.clone()),
            workdir_str: self.workspace_root.to_string_lossy().to_string(),
            run_started_at: Instant::now(),
            run_id: self.run_id,
            turn_id: self.turn_id,
            message_id: self.message_id,
            provider_session_ref: None,
            codex_home: None,
            context_window_metrics: None,
            ev_rx,
            events_done_tx,
            start_progress_tx,
            order_seq_state: Arc::new(Mutex::new(OrderSeqState::new(1))),
        }));

        ev_tx.send(event).await.expect("send tool event");
        drop(ev_tx);

        events_done_rx.await.expect("event loop completion");
        loop_task.await.expect("event loop join");
    }
}
