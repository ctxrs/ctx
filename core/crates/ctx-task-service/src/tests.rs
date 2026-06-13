use ctx_core::ids::TaskId;
use ctx_core::models::{ExecutionEnvironment, VcsKind};
use ctx_store::Store;

use crate::creation::{
    load_existing_task_record_for_request, persist_task_record_for_request,
    reload_or_retry_task_record_for_request, CreateTaskRecordInput, TaskRecordCreateError,
};
use crate::metadata::{set_task_read_state, update_task_title_record};

async fn setup_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.expect("open store");
    (dir, store)
}

#[tokio::test]
async fn task_record_creation_replays_same_requested_id() {
    let (_dir, store) = setup_store().await;
    let workspace = store
        .create_workspace("workspace".into(), "/tmp/workspace".into(), VcsKind::Git)
        .await
        .expect("workspace");
    let task_id = TaskId::new();
    let request = CreateTaskRecordInput {
        task_id: Some(task_id),
        title: "title".into(),
        description: Some("description".into()),
    };

    let existing = load_existing_task_record_for_request(&store, workspace.id, None, &request)
        .await
        .expect("existing lookup");
    assert!(existing.is_none());

    let persisted = persist_task_record_for_request(&store, workspace.id, existing, &request)
        .await
        .expect("persist task");
    assert!(persisted.created_in_this_request);

    let reloaded =
        reload_or_retry_task_record_for_request(&store, workspace.id, persisted, &request)
            .await
            .expect("reload task");
    assert_eq!(reloaded.task.id, task_id);

    let replay =
        load_existing_task_record_for_request(&store, workspace.id, Some(workspace.id), &request)
            .await
            .expect("replay lookup")
            .expect("existing task");
    assert_eq!(replay.id, task_id);
}

#[tokio::test]
async fn task_record_creation_rejects_conflicting_requested_id() {
    let (_dir, store) = setup_store().await;
    let workspace = store
        .create_workspace("workspace".into(), "/tmp/workspace".into(), VcsKind::Git)
        .await
        .expect("workspace");
    let task = store
        .create_task(workspace.id, "existing".into(), None)
        .await
        .expect("task");
    let request = CreateTaskRecordInput {
        task_id: Some(task.id),
        title: "different".into(),
        description: None,
    };

    let error =
        load_existing_task_record_for_request(&store, workspace.id, Some(workspace.id), &request)
            .await
            .expect_err("conflict");
    assert!(matches!(error, TaskRecordCreateError::Conflict(_)));
}

#[tokio::test]
async fn task_read_state_updates_return_current_task() {
    let (_dir, store) = setup_store().await;
    let workspace = store
        .create_workspace("workspace".into(), "/tmp/workspace".into(), VcsKind::Git)
        .await
        .expect("workspace");
    let task = store
        .create_task(workspace.id, "task".into(), None)
        .await
        .expect("task");

    let read = set_task_read_state(&store, task.id, true)
        .await
        .expect("mark read")
        .expect("updated task");
    assert!(read.assistant_seen_at.is_some());

    let unread = set_task_read_state(&store, task.id, false)
        .await
        .expect("mark unread")
        .expect("updated task");
    assert!(unread.assistant_seen_at.is_none());
}

#[tokio::test]
async fn title_update_returns_affected_session_and_worktree_ids() {
    let (_dir, store) = setup_store().await;
    let workspace = store
        .create_workspace("workspace".into(), "/tmp/workspace".into(), VcsKind::Git)
        .await
        .expect("workspace");
    let task = store
        .create_task(workspace.id, "old".into(), None)
        .await
        .expect("task");
    let worktree = store
        .create_worktree(workspace.id, "/tmp/worktree".into(), "abc123".into(), None)
        .await
        .expect("worktree");
    let session = store
        .create_session(
            task.id,
            workspace.id,
            worktree.id,
            ExecutionEnvironment::Host,
            "fake".into(),
            "model".into(),
            "implementer".into(),
            None,
            None,
            None,
        )
        .await
        .expect("session");
    store
        .set_task_primary_session(task.id, session.id, worktree.id)
        .await
        .expect("primary session");

    let outcome = update_task_title_record(&store, task.id, "new".into())
        .await
        .expect("update")
        .expect("outcome");
    assert_eq!(outcome.task.title, "new");
    assert!(outcome.session_ids.contains(&session.id));
    assert!(outcome.worktree_ids.contains(&worktree.id));
}
