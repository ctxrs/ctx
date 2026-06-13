use super::*;
use chrono::Utc;
use ctx_core::ids::{SessionEventId, SessionId, TaskId, TurnId, WorkspaceId, WorktreeId};
use ctx_core::models::{ExecutionEnvironment, Session, SessionEvent, SessionStatus};
use ctx_core::models::{SessionActivityState, SessionEventType, SessionTurn, SessionTurnStatus};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn test_session() -> Session {
    Session {
        id: SessionId::new(),
        task_id: TaskId::new(),
        workspace_id: WorkspaceId::new(),
        worktree_id: WorktreeId::new(),
        execution_environment: ExecutionEnvironment::Host,
        parent_session_id: None,
        relationship: None,
        provider_id: "fake".to_string(),
        model_id: "fake-model".to_string(),
        reasoning_effort: None,
        title: String::new(),
        agent_role: "assistant".to_string(),
        status: SessionStatus::Active,
        provider_session_ref: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn test_event(event_type: SessionEventType, payload_json: serde_json::Value) -> SessionEvent {
    SessionEvent {
        seq: 1,
        id: SessionEventId::new(),
        session_id: SessionId::new(),
        run_id: None,
        turn_id: None,
        event_type,
        payload_json,
        transient: false,
        created_at: Utc::now(),
    }
}

#[test]
fn queued_turns_do_not_publish_working_activity() {
    let queued = derive_summary_activity(&test_event(SessionEventType::TurnQueued, json!({})))
        .expect("queued turns should publish summary activity");
    assert!(!queued.is_working);
    assert_eq!(queued.last_turn_status, Some(SessionTurnStatus::Queued));

    let running = derive_summary_activity(&test_event(SessionEventType::TurnStarted, json!({})))
        .expect("running turns should publish summary activity");
    assert!(running.is_working);
    assert_eq!(running.last_turn_status, Some(SessionTurnStatus::Running));
}

#[test]
fn turn_finished_summary_activity_uses_embedded_status() {
    let interrupted = derive_summary_activity(&test_event(
        SessionEventType::TurnFinished,
        json!({"status": "interrupted"}),
    ))
    .expect("interrupt finish should publish summary activity");
    assert_eq!(
        interrupted.last_turn_status,
        Some(SessionTurnStatus::Interrupted)
    );

    let failed = derive_summary_activity(&test_event(
        SessionEventType::TurnFinished,
        json!({"status": "failed"}),
    ))
    .expect("failed finish should publish summary activity");
    assert_eq!(failed.last_turn_status, Some(SessionTurnStatus::Failed));

    assert!(derive_summary_activity(&test_event(
        SessionEventType::TurnFinished,
        json!({"status": "running"}),
    ))
    .is_none());
    assert!(
        derive_summary_activity(&test_event(SessionEventType::TurnFinished, json!({}),)).is_none()
    );
}

#[test]
fn patch_turn_ignores_non_terminal_turn_finished_status() {
    let created_at = Utc::now();
    let mut turn = SessionTurn {
        turn_id: TurnId::new(),
        session_id: SessionId::new(),
        run_id: None,
        user_message_id: None,
        status: SessionTurnStatus::Running,
        start_seq: Some(1),
        end_seq: None,
        started_at: created_at,
        updated_at: created_at,
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
    let event = test_event(
        SessionEventType::TurnFinished,
        json!({"status": "running", "message": "not terminal"}),
    );

    patch_turn_from_event(&mut turn, &event);

    assert_eq!(turn.status, SessionTurnStatus::Running);
    assert_eq!(turn.end_seq, None);
    assert_eq!(turn.failure, None);
    assert_eq!(turn.updated_at, created_at);
}

#[test]
fn turn_interrupted_projects_immediate_terminal_activity_and_turn() {
    let created_at = Utc::now();
    let mut turn = SessionTurn {
        turn_id: TurnId::new(),
        session_id: SessionId::new(),
        run_id: None,
        user_message_id: None,
        status: SessionTurnStatus::Running,
        start_seq: Some(1),
        end_seq: None,
        started_at: created_at,
        updated_at: created_at,
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
    let event = test_event(SessionEventType::TurnInterrupted, json!({"reason": "user"}));

    patch_turn_from_event(&mut turn, &event);

    assert_eq!(turn.status, SessionTurnStatus::Interrupted);
    assert_eq!(turn.end_seq, Some(event.seq));
    assert_eq!(turn.failure, None);
    assert_eq!(turn.updated_at, event.created_at);

    let activity = derive_summary_activity(&event)
        .expect("interrupt should publish terminal summary activity");
    assert!(!activity.is_working);
    assert_eq!(
        activity.last_turn_status,
        Some(SessionTurnStatus::Interrupted)
    );
}

#[test]
fn raw_non_lifecycle_events_do_not_publish_terminal_summary_activity() {
    assert!(derive_summary_activity(&test_event(SessionEventType::Done, json!({}))).is_none());
    assert!(derive_summary_activity(&test_event(SessionEventType::Notice, json!({}))).is_none());
}

#[test]
fn emitted_session_summary_deltas_always_include_monotonic_versions() {
    let session = test_session();
    let now = Utc::now();

    let message_delta = build_session_summary_delta(
        &session,
        None,
        Some(now),
        Some("preview".to_string()),
        22,
        22,
        22,
    )
    .expect("message preview should emit a summary delta");
    assert_eq!(message_delta.last_event_seq, Some(22));
    assert_eq!(message_delta.projection_rev, Some(22));
    assert_eq!(message_delta.state_rev, Some(22));
}

#[test]
fn empty_session_summary_delta_is_not_emitted() {
    let session = test_session();
    assert!(
        build_session_summary_delta(&session, None, None, None, 5, 5, 5).is_none(),
        "empty updates should not publish summary deltas"
    );
}

#[test]
fn activity_only_session_summary_delta_is_emitted() {
    let session = test_session();
    let delta = build_session_summary_delta(
        &session,
        Some(SessionActivityState {
            is_working: true,
            last_turn_status: Some(SessionTurnStatus::Running),
        }),
        None,
        None,
        5,
        5,
        5,
    )
    .expect("activity updates should emit a summary delta");
    assert!(delta.activity.expect("activity delta").is_working);
    assert_eq!(delta.last_event_seq, Some(5));
}

#[tokio::test]
async fn stream_only_projection_rev_skips_lookup() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_lookup = Arc::clone(&calls);
    let projection_rev =
        resolve_projection_rev_for_stream_delta(true, 41, 23, move || async move {
            calls_for_lookup.fetch_add(1, Ordering::SeqCst);
            Some(99)
        })
        .await;

    assert_eq!(projection_rev, 23);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn non_stream_only_projection_rev_uses_lookup_when_available() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_lookup = Arc::clone(&calls);
    let projection_rev =
        resolve_projection_rev_for_stream_delta(false, 17, 7, move || async move {
            calls_for_lookup.fetch_add(1, Ordering::SeqCst);
            Some(23)
        })
        .await;

    assert_eq!(projection_rev, 23);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn detects_session_gap_notice() {
    let event = test_event(
        SessionEventType::Notice,
        json!({
            "kind": "session_gap",
            "reason": "data_plane_overflow",
        }),
    );
    assert!(is_session_gap_notice(&event));
}

#[test]
fn ignores_non_gap_notice() {
    let event = test_event(
        SessionEventType::Notice,
        json!({
            "kind": "context.compacted",
        }),
    );
    assert!(!is_session_gap_notice(&event));
}

#[test]
fn ignores_non_notice_events() {
    let event = test_event(
        SessionEventType::ToolResult,
        json!({
            "kind": "session_gap",
        }),
    );
    assert!(!is_session_gap_notice(&event));
}
