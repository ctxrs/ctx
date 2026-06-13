use super::*;

use serde_json::json;

pub(crate) async fn setup_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    (dir, store)
}

pub(crate) async fn create_session_with_turn(
    store: &Store,
    assistant_partial: Option<String>,
) -> (Session, TurnId) {
    let ws = store
        .create_workspace("test".into(), "/tmp/test".into(), VcsKind::Git)
        .await
        .unwrap();
    let task = store.create_task(ws.id, "task".into(), None).await.unwrap();
    let worktree = store
        .create_worktree(ws.id, "/tmp/ws".into(), "abc123".into(), None)
        .await
        .unwrap();
    let session = store
        .create_session(
            task.id,
            ws.id,
            worktree.id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".into(),
            "fake".into(),
            "implementer".into(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    store
        .set_task_primary_session(task.id, session.id, worktree.id)
        .await
        .unwrap();

    let turn_id = TurnId::new();
    let now = Utc::now();
    let turn = SessionTurn {
        turn_id,
        session_id: session.id,
        run_id: None,
        user_message_id: None,
        status: SessionTurnStatus::Running,
        start_seq: Some(1),
        end_seq: None,
        started_at: now,
        updated_at: now,
        assistant_partial,
        thought_partial: None,
        metrics_json: None,
        failure: None,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
    };
    store.insert_session_turn(turn).await.unwrap();

    (session, turn_id)
}

#[tokio::test]
async fn session_head_snapshot_excludes_assistant_partials() {
    let (_dir, store) = setup_store().await;
    let (session, turn_id) = create_session_with_turn(&store, Some("partial".to_string())).await;

    let _ = store
        .append_session_event(
            session.id,
            None,
            Some(turn_id),
            SessionEventType::AssistantChunk,
            json!({"content_fragment":"hi"}),
        )
        .await
        .unwrap();
    let _ = store
        .append_session_event(
            session.id,
            None,
            Some(turn_id),
            SessionEventType::Notice,
            json!({"msg":"done"}),
        )
        .await
        .unwrap();

    let events = store.list_session_events(session.id).await.unwrap();
    assert_eq!(events.len(), 1);
    assert!(events
        .iter()
        .all(|event| !matches!(event.event_type, SessionEventType::AssistantChunk)));

    let head = store
        .get_session_head_snapshot(session.id, 10, true)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(head.turns.len(), 1);
    assert!(head.turns[0].assistant_partial.is_none());
    assert!(head
        .events
        .iter()
        .all(|event| !matches!(event.event_type, SessionEventType::AssistantChunk)));
    assert!(head
        .events
        .iter()
        .any(|event| matches!(event.event_type, SessionEventType::Notice)));
}

#[tokio::test]
async fn active_snapshot_head_strips_assistant_partials() {
    let (_dir, store) = setup_store().await;
    let (session, _turn_id) = create_session_with_turn(&store, Some("partial".to_string())).await;
    store
        .flush_active_snapshot_head_projection_queue()
        .await
        .unwrap();

    let row = sqlx::query(
        r#"SELECT head_rev, turns_json
               FROM session_active_snapshot_heads
               WHERE session_id = ?"#,
    )
    .bind(session.id.0.to_string())
    .fetch_optional(&store.pool)
    .await
    .unwrap();
    let row = row.expect("expected durable active snapshot head projection");
    let head_rev: i64 = row.try_get("head_rev").unwrap();
    assert_eq!(
        head_rev,
        store.get_session_projection_rev(session.id).await.unwrap()
    );
    let turns_json: String = row.try_get("turns_json").unwrap();
    let turns: Vec<SessionTurn> = serde_json::from_str(&turns_json).unwrap();
    assert_eq!(turns.len(), 1);
    assert!(turns[0].assistant_partial.is_none());
}

#[tokio::test]
async fn terminal_event_flush_updates_turn_and_summary_in_one_persist_path() {
    let (_dir, store) = setup_store().await;
    let (session, turn_id) = create_session_with_turn(&store, None).await;

    let _done = store
        .append_session_event(
            session.id,
            None,
            Some(turn_id),
            SessionEventType::Done,
            json!({"context_window": {"total_tokens": 42}}),
        )
        .await
        .unwrap();
    let finished = store
        .append_session_event(
            session.id,
            None,
            Some(turn_id),
            SessionEventType::TurnFinished,
            json!({"status": "completed"}),
        )
        .await
        .unwrap();

    let events = store
        .list_session_events_for_turn(session.id, turn_id, false)
        .await
        .unwrap();
    assert_eq!(events.len(), 2);

    let turn = store
        .get_session_turn(session.id, turn_id)
        .await
        .unwrap()
        .expect("turn exists");
    assert_eq!(turn.status, SessionTurnStatus::Completed);
    assert_eq!(turn.end_seq, Some(finished.seq));
    assert_eq!(turn.metrics_json, Some(json!({"total_tokens": 42})));

    let snapshot = store
        .get_session_snapshot(session.id, 10, false)
        .await
        .unwrap()
        .expect("snapshot");
    assert_eq!(snapshot.summary.last_event_seq, Some(finished.seq));
    assert_eq!(
        snapshot.summary.activity.last_turn_status,
        Some(SessionTurnStatus::Completed)
    );
    assert!(!snapshot.summary.activity.is_working);
}

#[tokio::test]
async fn provider_terminal_events_project_read_model_before_turn_finished_persists() {
    let (_dir, store) = setup_store().await;
    let cases = [
        (
            SessionEventType::Done,
            json!({"context_window": {"total_tokens": 7}}),
            json!({"status": "completed"}),
            SessionTurnStatus::Running,
            false,
            SessionTurnStatus::Completed,
            Some(json!({"total_tokens": 7})),
        ),
        (
            SessionEventType::TurnInterrupted,
            json!({"reason": "cancelled", "provider_cancelled": true}),
            json!({"status": "interrupted", "reason": "cancelled", "provider_cancelled": true}),
            SessionTurnStatus::Interrupted,
            true,
            SessionTurnStatus::Interrupted,
            None,
        ),
    ];

    for (
        event_type,
        event_payload,
        finished_payload,
        expected_raw_status,
        expected_raw_end_seq_present,
        expected_status,
        expected_metrics,
    ) in cases
    {
        let (session, turn_id) = create_session_with_turn(&store, None).await;

        let terminal = store
            .append_session_event(session.id, None, Some(turn_id), event_type, event_payload)
            .await
            .unwrap();
        store.flush_session_event_log().await.unwrap();

        let running_turn = store
            .get_session_turn(session.id, turn_id)
            .await
            .unwrap()
            .expect("turn exists");
        assert_eq!(running_turn.status, expected_raw_status);
        assert_eq!(
            running_turn.end_seq,
            expected_raw_end_seq_present.then_some(terminal.seq)
        );

        let persisted = store
            .persist_turn_terminal_events(
                session.id,
                None,
                turn_id,
                vec![(SessionEventType::TurnFinished, finished_payload)],
            )
            .await
            .unwrap();

        assert_eq!(persisted.len(), 1);
        assert!(persisted[0].seq > terminal.seq);

        let completed_turn = store
            .get_session_turn(session.id, turn_id)
            .await
            .unwrap()
            .expect("completed turn");
        assert_eq!(completed_turn.status, expected_status);
        assert_eq!(completed_turn.end_seq, Some(persisted[0].seq));
        assert_eq!(completed_turn.metrics_json, expected_metrics);
    }
}

#[tokio::test]
async fn failed_turn_finished_projects_failure_json() {
    let (_dir, store) = setup_store().await;
    let (session, turn_id) = create_session_with_turn(&store, None).await;

    let persisted = store
        .persist_turn_terminal_events(
            session.id,
            None,
            turn_id,
            vec![(
                SessionEventType::TurnFinished,
                json!({
                    "status": "failed",
                    "message": "provider error",
                    "details": {"exit_code": 1},
                    "kind": "provider_protocol_violation",
                }),
            )],
        )
        .await
        .unwrap();

    let turn = store
        .get_session_turn(session.id, turn_id)
        .await
        .unwrap()
        .expect("turn exists");
    assert_eq!(turn.status, SessionTurnStatus::Failed);
    assert_eq!(turn.end_seq, Some(persisted[0].seq));
    let failure = turn.failure.expect("failure projection");
    assert_eq!(failure.message.as_deref(), Some("provider error"));
    assert_eq!(failure.details, Some(json!({"exit_code": 1})));
    assert_eq!(failure.kind.as_deref(), Some("provider_protocol_violation"));
}

#[tokio::test]
async fn terminal_projection_without_turn_finished_persists_missing_turn_finished() {
    let (_dir, store) = setup_store().await;
    let (session, turn_id) = create_session_with_turn(&store, None).await;

    let interrupted = store
        .append_session_event(
            session.id,
            None,
            Some(turn_id),
            SessionEventType::TurnInterrupted,
            json!({"reason": "cancelled", "provider_cancelled": true}),
        )
        .await
        .unwrap();
    store.flush_session_event_log().await.unwrap();
    store
        .update_session_turn_status(
            session.id,
            turn_id,
            SessionTurnStatus::Interrupted,
            Some(interrupted.seq),
            None,
            Utc::now(),
        )
        .await
        .unwrap();

    let persisted = store
        .persist_turn_terminal_events(
            session.id,
            None,
            turn_id,
            vec![(
                SessionEventType::TurnFinished,
                json!({"status": "interrupted", "reason": "cancelled"}),
            )],
        )
        .await
        .unwrap();

    assert_eq!(persisted.len(), 1);
    assert!(matches!(
        persisted[0].event_type,
        SessionEventType::TurnFinished
    ));
    assert!(persisted[0].seq > interrupted.seq);

    let repaired_turn = store
        .get_session_turn(session.id, turn_id)
        .await
        .unwrap()
        .expect("repaired turn");
    assert_eq!(repaired_turn.status, SessionTurnStatus::Interrupted);
    assert_eq!(repaired_turn.end_seq, Some(persisted[0].seq));

    let duplicate = store
        .persist_turn_terminal_events(
            session.id,
            None,
            turn_id,
            vec![(
                SessionEventType::TurnFinished,
                json!({"status": "interrupted", "reason": "cancelled"}),
            )],
        )
        .await
        .unwrap();
    assert!(duplicate.is_empty());

    let finished_count = store
        .list_session_events_for_turn(session.id, turn_id, false)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| matches!(event.event_type, SessionEventType::TurnFinished))
        .count();
    assert_eq!(finished_count, 1);
}

#[tokio::test]
async fn malformed_turn_finished_does_not_suppress_valid_terminal_write() {
    let (_dir, store) = setup_store().await;
    let (session, turn_id) = create_session_with_turn(&store, None).await;

    let malformed = store
        .append_session_event(
            session.id,
            None,
            Some(turn_id),
            SessionEventType::TurnFinished,
            json!({"status": "not-terminal"}),
        )
        .await
        .unwrap();
    store.flush_session_event_log().await.unwrap();

    let running_turn = store
        .get_session_turn(session.id, turn_id)
        .await
        .unwrap()
        .expect("turn exists");
    assert_eq!(running_turn.status, SessionTurnStatus::Running);
    assert_eq!(running_turn.end_seq, None);

    let persisted = store
        .persist_turn_terminal_events(
            session.id,
            None,
            turn_id,
            vec![(
                SessionEventType::TurnFinished,
                json!({
                    "status": "failed",
                    "message": "provider failed after malformed terminal event",
                }),
            )],
        )
        .await
        .unwrap();

    assert_eq!(persisted.len(), 1);
    assert!(persisted[0].seq > malformed.seq);

    let failed_turn = store
        .get_session_turn(session.id, turn_id)
        .await
        .unwrap()
        .expect("turn exists");
    assert_eq!(failed_turn.status, SessionTurnStatus::Failed);
    assert_eq!(failed_turn.end_seq, Some(persisted[0].seq));
    assert_eq!(
        failed_turn
            .failure
            .as_ref()
            .and_then(|failure| failure.message.as_deref()),
        Some("provider failed after malformed terminal event")
    );

    let finished_count = store
        .list_session_events_for_turn(session.id, turn_id, false)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| matches!(event.event_type, SessionEventType::TurnFinished))
        .count();
    assert_eq!(finished_count, 2);
}

#[tokio::test]
async fn turn_projection_repair_ignores_tool_rows_after_terminal_seq() {
    let (_dir, store) = setup_store().await;
    let (session, turn_id) = create_session_with_turn(&store, None).await;

    let before_event = store
        .append_session_event(
            session.id,
            None,
            Some(turn_id),
            SessionEventType::Notice,
            json!({"kind": "before-terminal-tool-anchor"}),
        )
        .await
        .unwrap();
    let before_updated_at = before_event.created_at;
    store
        .upsert_session_turn_tool(SessionTurnTool {
            session_id: session.id,
            tool_call_id: "before-terminal-tool".to_string(),
            turn_id,
            tool_kind: Some("execute".to_string()),
            provider_tool_name: Some("Bash".to_string()),
            title: Some("Bash".to_string()),
            subtitle: None,
            status: Some("completed".to_string()),
            input_json: Some(json!({"cmd": "pwd"})),
            output_text: Some("/tmp/ws".to_string()),
            order_seq: 1,
            first_event_seq: Some(before_event.seq),
            input_truncated: Some(false),
            input_original_bytes: None,
            output_truncated: Some(false),
            output_original_bytes: None,
            created_at: before_updated_at,
            updated_at: before_updated_at,
        })
        .await
        .unwrap();
    store
        .update_session_turn_tool_counts(
            session.id,
            turn_id,
            SessionTurnToolCountDeltas {
                total: 1,
                pending: 0,
                running: 0,
                completed: 1,
                failed: 0,
            },
            before_updated_at,
        )
        .await
        .unwrap();

    let persisted = store
        .persist_turn_terminal_events(
            session.id,
            None,
            turn_id,
            vec![(
                SessionEventType::TurnFinished,
                json!({"status": "failed", "message": "usage limit"}),
            )],
        )
        .await
        .unwrap();
    let finished = persisted
        .into_iter()
        .next()
        .expect("turn_finished event persisted");

    let post_updated_at = finished.created_at + chrono::Duration::seconds(10);
    let post_event = SessionEvent {
        seq: finished.seq + 1,
        id: SessionEventId::new(),
        session_id: session.id,
        run_id: None,
        turn_id: Some(turn_id),
        event_type: SessionEventType::Notice,
        payload_json: json!({"kind": "post-terminal-tool-anchor"}),
        transient: false,
        created_at: post_updated_at,
    };
    store
        .persist_session_events_batch(std::slice::from_ref(&post_event))
        .await
        .unwrap();
    store
        .upsert_session_turn_tool(SessionTurnTool {
            session_id: session.id,
            tool_call_id: "post-terminal-tool".to_string(),
            turn_id,
            tool_kind: Some("execute".to_string()),
            provider_tool_name: Some("Bash".to_string()),
            title: Some("Bash".to_string()),
            subtitle: None,
            status: Some("in_progress".to_string()),
            input_json: Some(json!({"cmd": "sleep 60"})),
            output_text: None,
            order_seq: 2,
            first_event_seq: Some(post_event.seq),
            input_truncated: Some(false),
            input_original_bytes: None,
            output_truncated: None,
            output_original_bytes: None,
            created_at: post_updated_at,
            updated_at: post_updated_at,
        })
        .await
        .unwrap();
    store
        .update_session_turn_tool_counts(
            session.id,
            turn_id,
            SessionTurnToolCountDeltas {
                total: 1,
                pending: 0,
                running: 1,
                completed: 0,
                failed: 0,
            },
            post_updated_at,
        )
        .await
        .unwrap();

    let corrupted = store
        .get_session_turn(session.id, turn_id)
        .await
        .unwrap()
        .expect("turn exists");
    assert_eq!(corrupted.tool_total, 2);
    assert_eq!(corrupted.tool_running, 1);

    store
        .repair_session_turn_projection_from_events(session.id, turn_id)
        .await
        .unwrap();

    let repaired = store
        .get_session_turn(session.id, turn_id)
        .await
        .unwrap()
        .expect("turn exists");
    assert_eq!(repaired.status, SessionTurnStatus::Failed);
    assert_eq!(repaired.end_seq, Some(finished.seq));
    assert_eq!(repaired.tool_total, 1);
    assert_eq!(repaired.tool_pending, 0);
    assert_eq!(repaired.tool_running, 0);
    assert_eq!(repaired.tool_completed, 1);
    assert_eq!(repaired.tool_failed, 0);
    assert!(
        repaired.updated_at < post_updated_at,
        "post-terminal tool timestamps must not keep terminal turns fresh"
    );
}

#[tokio::test]
async fn append_session_event_rejects_durable_events_after_terminal_turn() {
    let (_dir, store) = setup_store().await;
    let (session, turn_id) = create_session_with_turn(&store, None).await;

    store
        .persist_turn_terminal_events(
            session.id,
            None,
            turn_id,
            vec![(
                SessionEventType::TurnFinished,
                json!({"status": "failed", "message": "terminal"}),
            )],
        )
        .await
        .unwrap();

    let err = store
        .append_session_event(
            session.id,
            None,
            Some(turn_id),
            SessionEventType::ToolCall,
            json!({
                "tool_call_id": "late-tool",
                "status": "running",
            }),
        )
        .await
        .expect_err("durable post-terminal events must be rejected");
    assert!(
        err.to_string().contains("after turn terminalization"),
        "unexpected error: {err:#}"
    );

    let events = store
        .list_session_events_for_turn(session.id, turn_id, false)
        .await
        .unwrap();
    assert!(events
        .iter()
        .all(|event| !matches!(event.event_type, SessionEventType::ToolCall)));
}

#[tokio::test]
async fn get_terminal_event_for_run_flushes_buffered_events_before_reading() {
    let (_dir, store) = setup_store().await;
    let (session, turn_id) = create_session_with_turn(&store, None).await;
    let run_id = RunId::new();
    let write_guard = store.write_gate.lock().await;
    let queued_event = SessionEvent {
        seq: store.event_log.next_seq(),
        id: SessionEventId::new(),
        session_id: session.id,
        run_id: Some(run_id),
        turn_id: Some(turn_id),
        event_type: SessionEventType::TurnFinished,
        payload_json: json!({ "status": "completed" }),
        transient: false,
        created_at: Utc::now(),
    };
    store
        .event_log
        .enqueue(queued_event.clone())
        .await
        .expect("buffer terminal event");

    let store_for_read = store.clone();
    let read_task = tokio::spawn(async move {
        store_for_read
            .get_terminal_event_for_run(session.id, run_id)
            .await
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !read_task.is_finished(),
        "terminal-event read should wait for buffered event-log persistence"
    );

    drop(write_guard);

    let terminal = read_task
        .await
        .expect("join read task")
        .expect("read terminal event")
        .expect("terminal event exists");
    assert_eq!(terminal.id, queued_event.id);
}

#[tokio::test]
async fn subagent_label_is_unique_per_task() {
    let (_dir, store) = setup_store().await;
    let ws = store
        .create_workspace("test".into(), "/tmp/test".into(), VcsKind::Git)
        .await
        .unwrap();
    let task = store.create_task(ws.id, "task".into(), None).await.unwrap();
    let worktree = store
        .create_worktree(ws.id, "/tmp/ws".into(), "abc123".into(), None)
        .await
        .unwrap();
    let parent = store
        .create_session(
            task.id,
            ws.id,
            worktree.id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".into(),
            "fake".into(),
            "assistant".into(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    let child_one = store
        .create_session(
            task.id,
            ws.id,
            worktree.id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".into(),
            "fake".into(),
            "subagent".into(),
            Some(parent.id),
            Some("sub_agent".into()),
            None,
        )
        .await
        .unwrap();
    let child_two = store
        .create_session(
            task.id,
            ws.id,
            worktree.id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".into(),
            "fake".into(),
            "subagent".into(),
            Some(parent.id),
            Some("sub_agent".into()),
            None,
        )
        .await
        .unwrap();

    store
        .update_session_title(child_one.id, "Dup".into())
        .await
        .unwrap();

    let err = store
        .update_session_title(child_two.id, "Dup".into())
        .await
        .unwrap_err();
    assert!(err.to_string().to_lowercase().contains("unique"));
}

#[tokio::test]
async fn malformed_mobile_profile_scopes_json_fails_closed() {
    let (_dir, store) = setup_store().await;
    let profile_id = ConnectionProfileId::new();
    let token_hash = "ctxm_bad_scopes_hash";

    sqlx::query(
        r#"INSERT INTO mobile_connection_profiles
           (id, label, base_url, token_hash, token_prefix, scopes_json, created_at)
           VALUES (?, ?, ?, ?, ?, ?, ?)"#,
    )
    .bind(profile_id.0.to_string())
    .bind("mobile")
    .bind("https://example.com")
    .bind(token_hash)
    .bind("ctxm_bad")
    .bind("{not-json")
    .bind(Utc::now().to_rfc3339())
    .execute(&store.pool)
    .await
    .unwrap();

    let err = store
        .get_mobile_connection_profile(profile_id)
        .await
        .expect_err("malformed scope JSON should fail closed");
    assert!(
        format!("{err:#}").contains("invalid mobile profile scopes_json"),
        "unexpected error: {err:#}"
    );

    let err = store
        .get_mobile_connection_profile_by_token_hash(token_hash)
        .await
        .expect_err("token lookup should also fail closed");
    assert!(
        format!("{err:#}").contains("invalid mobile profile scopes_json"),
        "unexpected error: {err:#}"
    );
}
