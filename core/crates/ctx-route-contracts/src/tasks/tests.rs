use super::*;
use chrono::{DateTime, TimeZone, Utc};
use ctx_core::ids::{SessionId, TaskId, WorkspaceId, WorktreeId};
use ctx_core::models::{
    ExecutionEnvironment, Session, SessionStatus, SessionSummary, Task, TaskStatus,
    WorkspaceArchivedPage, WorkspaceIndexCursor, WorkspaceTaskSummary,
};
use serde_json::json;

fn now(offset: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, 17, 10, offset, 0).unwrap()
}

fn task_with_optional_fields(status: TaskStatus) -> Task {
    Task {
        id: TaskId::new(),
        workspace_id: WorkspaceId::new(),
        title: "task".to_string(),
        description: Some("description".to_string()),
        status,
        created_at: now(0),
        updated_at: now(1),
        exec_plan_id: Some("plan".to_string()),
        primary_session_id: Some(SessionId::new()),
        primary_worktree_id: Some(WorktreeId::new()),
        archived_at: Some(now(2)),
        assistant_seen_at: Some(now(3)),
        last_activity_at: Some(now(4)),
        last_assistant_message_at: Some(now(5)),
        has_active_session: true,
    }
}

fn session_with_optional_fields(status: SessionStatus) -> Session {
    Session {
        id: SessionId::new(),
        task_id: TaskId::new(),
        workspace_id: WorkspaceId::new(),
        worktree_id: WorktreeId::new(),
        execution_environment: ExecutionEnvironment::Sandbox,
        parent_session_id: Some(SessionId::new()),
        relationship: Some("follow_up".to_string()),
        provider_id: "fake".to_string(),
        model_id: "fake-model".to_string(),
        reasoning_effort: Some("high".to_string()),
        title: "session".to_string(),
        agent_role: "default".to_string(),
        status,
        provider_session_ref: Some("provider-ref".to_string()),
        created_at: now(0),
        updated_at: now(1),
    }
}

fn session_summary_with_optional_fields(status: SessionStatus) -> SessionSummary {
    SessionSummary {
        id: SessionId::new(),
        task_id: TaskId::new(),
        workspace_id: WorkspaceId::new(),
        execution_environment: ExecutionEnvironment::Sandbox,
        parent_session_id: Some(SessionId::new()),
        relationship: Some("follow_up".to_string()),
        provider_id: "fake".to_string(),
        model_id: "fake-model".to_string(),
        reasoning_effort: Some("medium".to_string()),
        title: "summary".to_string(),
        status,
        created_at: now(2),
        updated_at: now(3),
    }
}

#[test]
fn task_route_response_matches_raw_task_wire_shape_with_optional_fields() {
    let task = task_with_optional_fields(TaskStatus::Running);

    assert_eq!(
        serde_json::to_value(TaskRouteResponse::from(task.clone())).unwrap(),
        serde_json::to_value(task).unwrap()
    );
}

#[test]
fn task_route_response_matches_raw_task_wire_shape_without_optional_fields() {
    let mut task = task_with_optional_fields(TaskStatus::Completed);
    task.description = None;
    task.exec_plan_id = None;
    task.primary_session_id = None;
    task.primary_worktree_id = None;
    task.archived_at = None;
    task.assistant_seen_at = None;
    task.last_activity_at = None;
    task.last_assistant_message_at = None;
    task.has_active_session = false;

    assert_eq!(
        serde_json::to_value(TaskRouteResponse::from(task.clone())).unwrap(),
        serde_json::to_value(task).unwrap()
    );
}

#[test]
fn session_route_response_matches_raw_session_wire_shape() {
    let session = session_with_optional_fields(SessionStatus::Active);

    assert_eq!(
        serde_json::to_value(SessionRouteResponse::from(session.clone())).unwrap(),
        serde_json::to_value(session).unwrap()
    );
}

#[test]
fn session_route_response_matches_raw_session_wire_shape_without_optional_fields() {
    let mut session = session_with_optional_fields(SessionStatus::Completed);
    session.execution_environment = ExecutionEnvironment::Host;
    session.parent_session_id = None;
    session.relationship = None;
    session.reasoning_effort = None;
    session.provider_session_ref = None;

    assert_eq!(
        serde_json::to_value(SessionRouteResponse::from(session.clone())).unwrap(),
        serde_json::to_value(session).unwrap()
    );
}

#[test]
fn archived_page_response_matches_raw_page_wire_shape() {
    let task = task_with_optional_fields(TaskStatus::Pending);
    let summary = WorkspaceTaskSummary {
        task,
        provider_ids: vec!["fake".to_string()],
        sessions: vec![session_summary_with_optional_fields(SessionStatus::Active)],
        sort_at: now(6),
    };
    let page = WorkspaceArchivedPage {
        workspace_id: WorkspaceId::new(),
        archived_rev: 42,
        tasks: vec![summary],
        next_cursor: Some(WorkspaceIndexCursor {
            sort_at: now(7),
            task_id: TaskId::new(),
        }),
        total_archived: 3,
    };

    assert_eq!(
        serde_json::to_value(WorkspaceArchivedPageRouteResponse::from(page.clone())).unwrap(),
        serde_json::to_value(page).unwrap()
    );
}

#[test]
fn archived_page_response_matches_raw_page_wire_shape_without_cursor_or_nested_lists() {
    let page = WorkspaceArchivedPage {
        workspace_id: WorkspaceId::new(),
        archived_rev: 0,
        tasks: vec![WorkspaceTaskSummary {
            task: task_with_optional_fields(TaskStatus::Failed),
            provider_ids: Vec::new(),
            sessions: Vec::new(),
            sort_at: now(8),
        }],
        next_cursor: None,
        total_archived: 1,
    };

    assert_eq!(
        serde_json::to_value(WorkspaceArchivedPageRouteResponse::from(page.clone())).unwrap(),
        serde_json::to_value(page).unwrap()
    );
}

#[test]
fn archive_response_preserves_flattened_task_shape() {
    let task = task_with_optional_fields(TaskStatus::Cancelled);
    let response = ArchiveTaskRouteResponse::from_task(task.clone(), true);
    let mut expected = serde_json::to_value(task).unwrap();
    expected["cleanup_failed"] = json!(true);

    assert_eq!(serde_json::to_value(response).unwrap(), expected);
}

#[test]
fn create_session_request_parses_and_normalizes_fields() {
    let req: CreateTaskSessionRouteRequest = serde_json::from_value(json!({
        "id": "session-id",
        "provider_id": " fake ",
        "model_id": " fake-model ",
        "reasoning_effort": "extra high",
        "remember_model_preference": true,
        "parent_session_id": "parent",
        "relationship": "follow_up",
        "initial_prompt": "hello",
        "initial_message_id": "message",
        "initial_turn_id": "turn",
        "worktree_id": "worktree",
        "execution_environment": "container_default"
    }))
    .unwrap();

    let spec = req.into_spec(Some("run".to_string()));
    assert_eq!(spec.id.as_deref(), Some("session-id"));
    assert_eq!(spec.provider_id, "fake");
    assert_eq!(spec.model_id, "fake-model");
    assert_eq!(spec.reasoning_effort.as_deref(), Some("xhigh"));
    assert!(spec.remember_model_preference);
    assert_eq!(spec.parent_session_id.as_deref(), Some("parent"));
    assert_eq!(spec.relationship.as_deref(), Some("follow_up"));
    assert_eq!(spec.initial_prompt.as_deref(), Some("hello"));
    assert_eq!(spec.initial_message_id.as_deref(), Some("message"));
    assert_eq!(spec.initial_turn_id.as_deref(), Some("turn"));
    assert_eq!(spec.worktree_id.as_deref(), Some("worktree"));
    assert_eq!(
        spec.execution_environment,
        Some(ExecutionEnvironment::Sandbox)
    );
    assert_eq!(spec.run_id_header.as_deref(), Some("run"));
}

#[test]
fn create_session_request_drops_blank_reasoning_effort() {
    let req: CreateTaskSessionRouteRequest = serde_json::from_value(json!({
        "provider_id": "fake",
        "model_id": "fake-model",
        "reasoning_effort": "   "
    }))
    .unwrap();

    assert_eq!(req.into_spec(None).reasoning_effort, None);
}

#[test]
fn create_session_request_rejects_unknown_reasoning_effort() {
    let error = serde_json::from_value::<CreateTaskSessionRouteRequest>(json!({
        "provider_id": "fake",
        "model_id": "fake-model",
        "reasoning_effort": "huge"
    }))
    .unwrap_err();

    assert!(error
        .to_string()
        .contains("invalid reasoning_effort 'huge'"));
}

#[test]
fn create_session_request_rejects_legacy_env_target_alias() {
    let error = serde_json::from_value::<CreateTaskSessionRouteRequest>(json!({
        "provider_id": "fake",
        "model_id": "fake-model",
        "env_target": "local"
    }))
    .unwrap_err();

    assert!(error.to_string().contains("unknown field"));
}

#[test]
fn create_session_request_rejects_invalid_execution_environment() {
    let error = serde_json::from_value::<CreateTaskSessionRouteRequest>(json!({
        "provider_id": "fake",
        "model_id": "fake-model",
        "execution_environment": "worktree"
    }))
    .unwrap_err();

    assert!(error.to_string().contains("unknown execution environment"));
}

#[test]
fn create_session_request_rejects_empty_model_id() {
    let error = serde_json::from_value::<CreateTaskSessionRouteRequest>(json!({
        "provider_id": "fake",
        "model_id": "   "
    }))
    .unwrap_err();

    assert!(error.to_string().contains("model_id must not be empty"));
}

#[test]
fn create_session_request_rejects_default_placeholder_model_id() {
    let error = serde_json::from_value::<CreateTaskSessionRouteRequest>(json!({
        "provider_id": "fake",
        "model_id": "default"
    }))
    .unwrap_err();

    assert!(error
        .to_string()
        .contains("model_id must be a concrete model id"));
}

#[test]
fn create_session_request_rejects_empty_provider_id() {
    let error = serde_json::from_value::<CreateTaskSessionRouteRequest>(json!({
        "provider_id": "   ",
        "model_id": "fake-model"
    }))
    .unwrap_err();

    assert!(error.to_string().contains("provider_id must not be empty"));
}

#[test]
fn create_task_request_rejects_legacy_default_session_flag() {
    let error = serde_json::from_value::<CreateTaskRouteRequest>(json!({
        "title": "task",
        "create_default_session": false
    }))
    .unwrap_err();

    assert!(error.to_string().contains("unknown field"));
}

#[test]
fn create_task_request_accepts_default_session_options() {
    let task_id = TaskId::new();
    let req: CreateTaskRouteRequest = serde_json::from_value(json!({
        "id": format!(" {} ", task_id.0),
        "title": "task",
        "description": "description",
        "default_session": {
            "provider_id": "fake",
            "model_id": "fake-model",
            "execution_environment": "host"
        }
    }))
    .unwrap();

    let spec = req.into_spec().unwrap();
    assert_eq!(spec.task_id, Some(task_id));
    assert_eq!(spec.title, "task");
    assert_eq!(spec.description.as_deref(), Some("description"));
    assert!(spec.default_session.is_some());
}

#[test]
fn create_task_request_rejects_invalid_task_id() {
    let req: CreateTaskRouteRequest = serde_json::from_value(json!({
        "id": "not-a-task",
        "title": "task"
    }))
    .unwrap();
    let error = req.into_spec().unwrap_err();

    assert_eq!(error.kind(), TaskRouteErrorKind::BadRequest);
    assert_eq!(error.message(), "invalid task id");
}

#[test]
fn archived_cursor_parses_complete_cursor() {
    let task_id = TaskId::new();
    let sort_at = now(9);
    let cursor = parse_archived_cursor(Some(&sort_at.to_rfc3339()), Some(&task_id.0.to_string()))
        .unwrap()
        .unwrap();

    assert_eq!(cursor.sort_at, sort_at);
    assert_eq!(cursor.task_id, task_id);
}

#[test]
fn archived_cursor_rejects_half_cursor() {
    let error = parse_archived_cursor(Some(&now(9).to_rfc3339()), None).unwrap_err();

    assert_eq!(error.kind(), TaskRouteErrorKind::BadRequest);
    assert_eq!(error.message(), "invalid cursor");
}

#[test]
fn archived_cursor_rejects_bad_timestamp() {
    let error = parse_archived_cursor(Some("not-a-date"), Some("not-a-task")).unwrap_err();

    assert_eq!(error.kind(), TaskRouteErrorKind::BadRequest);
    assert_eq!(error.message(), "invalid cursor");
}

#[test]
fn archived_cursor_rejects_bad_task_id() {
    let error = parse_archived_cursor(Some(&now(9).to_rfc3339()), Some("not-a-task")).unwrap_err();

    assert_eq!(error.kind(), TaskRouteErrorKind::BadRequest);
    assert_eq!(error.message(), "invalid task id");
}

#[test]
fn title_request_trims_and_validates() {
    let title: UpdateTaskTitleRouteRequest =
        serde_json::from_value(json!({"title": "  New title  "})).unwrap();
    assert_eq!(title.validated_title().unwrap(), "New title");

    let empty: UpdateTaskTitleRouteRequest =
        serde_json::from_value(json!({"title": "  "})).unwrap();
    let empty = empty.validated_title().unwrap_err();
    assert_eq!(empty.kind(), TaskRouteErrorKind::BadRequest);
    assert_eq!(empty.message(), "title is required");

    let too_long: UpdateTaskTitleRouteRequest =
        serde_json::from_value(json!({"title": "x".repeat(121)})).unwrap();
    let too_long = too_long.validated_title().unwrap_err();
    assert_eq!(too_long.kind(), TaskRouteErrorKind::BadRequest);
    assert_eq!(too_long.message(), "title is too long");
}
