use super::*;

/// Queued turns must remain queued after a daemon restart, but they are not
/// executing work. Reconcile should only interrupt turns that were actually
/// running or starting.
#[tokio::test]
async fn reconcile_running_turns_leaves_queued_turns_queued() {
    let temp = tempdir().unwrap();
    let stores = StoreManager::open(temp.path()).await.unwrap();
    let state = Arc::new(DaemonState::new(
        temp.path().to_path_buf(),
        stores.clone(),
        HashMap::new(),
        "http://localhost".to_string(),
        None,
    ));

    let workspace = state
        .global_store()
        .create_workspace(
            "ws".to_string(),
            temp.path().join("ws").to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .unwrap();
    let store = state.store_for_workspace(workspace.id).await.unwrap();
    let worktree = store
        .create_worktree(
            workspace.id,
            temp.path().join("ws").to_string_lossy().to_string(),
            "deadbeef".to_string(),
            None,
        )
        .await
        .unwrap();
    let task = store
        .create_task(workspace.id, "task".to_string(), None)
        .await
        .unwrap();
    let session = store
        .create_session(
            task.id,
            workspace.id,
            worktree.id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".to_string(),
            "model".to_string(),
            "implementer".to_string(),
            None,
            None,
            None,
        )
        .await
        .unwrap();

    // Register the session in the global index so store_for_session can resolve it.
    state
        .global_store()
        .upsert_workspace_session_index(session.id, workspace.id)
        .await
        .unwrap();

    let now = Utc::now();
    let queued_turn_id = TurnId::new();
    let queued_turn = SessionTurn {
        turn_id: queued_turn_id,
        session_id: session.id,
        run_id: None,
        user_message_id: None,
        status: SessionTurnStatus::Queued,
        start_seq: None,
        end_seq: None,
        started_at: now,
        updated_at: now,
        assistant_partial: None,
        thought_partial: None,
        metrics_json: None,
        failure: None,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
    };
    store.insert_session_turn(queued_turn).await.unwrap();

    let running_turn_id = TurnId::new();
    let running_turn = SessionTurn {
        turn_id: running_turn_id,
        session_id: session.id,
        run_id: Some(RunId::new()),
        user_message_id: None,
        status: SessionTurnStatus::Running,
        start_seq: Some(1),
        end_seq: None,
        started_at: now,
        updated_at: now,
        assistant_partial: None,
        thought_partial: None,
        metrics_json: None,
        failure: None,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
    };
    store.insert_session_turn(running_turn).await.unwrap();

    let activity = daemon_turn_activity_summary(&state).await.unwrap();
    assert!(!activity.idle);
    assert_eq!(activity.active_turn_count, 1);
    assert_eq!(activity.queued_turn_count, 1);
    assert_eq!(activity.running_turn_count, 1);

    reconcile_running_turns(&state).await.unwrap();

    let queued_after = store
        .get_session_turn(session.id, queued_turn_id)
        .await
        .unwrap()
        .expect("queued turn must still exist");
    assert_eq!(
        queued_after.status,
        SessionTurnStatus::Queued,
        "queued turn must remain queued after reconcile_running_turns"
    );

    let running_after = store
        .get_session_turn(session.id, running_turn_id)
        .await
        .unwrap()
        .expect("running turn must still exist");
    assert_eq!(
        running_after.status,
        SessionTurnStatus::Interrupted,
        "running turn must be interrupted after reconcile_running_turns"
    );
    let terminal_events = store
        .list_session_events_for_turn(session.id, running_turn_id, false)
        .await
        .unwrap();
    assert!(
        terminal_events
            .iter()
            .any(|event| matches!(&event.event_type, SessionEventType::TurnInterrupted)),
        "interrupted reconcile should persist a turn_interrupted event"
    );
    assert!(
        terminal_events.iter().any(|event| {
            matches!(&event.event_type, SessionEventType::TurnFinished)
                && event
                    .payload_json
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    == Some("interrupted")
        }),
        "interrupted reconcile should persist terminal turn_finished event"
    );

    let activity_after = daemon_turn_activity_summary(&state).await.unwrap();
    assert!(activity_after.idle);
    assert_eq!(activity_after.active_turn_count, 0);
    assert_eq!(activity_after.queued_turn_count, 1);
    assert_eq!(activity_after.running_turn_count, 0);
}
