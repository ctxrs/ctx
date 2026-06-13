use super::super::load_workspace_snapshot_hydration_payload;
use super::fixtures::{test_head, test_summary, FakeHydrationStore};
use ctx_core::ids::{SessionId, TaskId, WorkspaceId};
use std::sync::Mutex;

#[tokio::test]
async fn workspace_hydration_payload_uses_canonical_page_and_preserves_snapshot_rev() {
    let workspace_id = WorkspaceId::new();
    let task_id = TaskId::new();
    let session_id = SessionId::new();
    let head = test_head(workspace_id, task_id, session_id);
    let mut summary = test_summary(workspace_id, task_id, session_id);
    summary.task.primary_worktree_id = Some(head.session.worktree_id);
    summary.primary_session.session.worktree_id = head.session.worktree_id;
    let store = FakeHydrationStore {
        snapshot_state: (17, 4),
        tasks: vec![summary],
        heads: vec![head.clone()],
        heads_error: None,
        calls: Mutex::new(Vec::new()),
    };

    let payload = load_workspace_snapshot_hydration_payload(&store, workspace_id)
        .await
        .expect("expected hydration payload");
    let calls = store.calls.lock().unwrap().clone();
    assert_eq!(calls, vec!["snapshot_state", "active_page", "active_heads"]);
    assert_eq!(payload.snapshot_rev, 17);
    assert_eq!(payload.archived_rev, 4);
    assert_eq!(payload.tasks.len(), 1);
    assert_eq!(
        payload.tasks[0]
            .primary_session
            .last_message_preview
            .as_deref(),
        Some("canonical-summary")
    );
    assert_eq!(payload.tasks[0].primary_session.last_event_seq, Some(44));
    assert_eq!(payload.heads.len(), 1);
    assert_eq!(payload.heads[0].session.id, head.session.id);
    assert_eq!(payload.heads[0].last_event_seq, head.last_event_seq);
}

#[tokio::test]
async fn workspace_hydration_payload_propagates_active_head_batch_errors() {
    let workspace_id = WorkspaceId::new();
    let task_id = TaskId::new();
    let store = FakeHydrationStore {
        snapshot_state: (19, 5),
        tasks: vec![test_summary(workspace_id, task_id, SessionId::new())],
        heads: Vec::new(),
        heads_error: Some("head decode failed"),
        calls: Mutex::new(Vec::new()),
    };

    let err = load_workspace_snapshot_hydration_payload(&store, workspace_id)
        .await
        .expect_err("expected hydration to fail");
    assert!(err.to_string().contains("head decode failed"));
}
