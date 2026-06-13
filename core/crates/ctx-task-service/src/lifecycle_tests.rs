use std::collections::HashSet;

use ctx_core::models::{ExecutionEnvironment, VcsKind};
use ctx_store::Store;

use crate::lifecycle::{
    archive_task_record, collect_archive_cleanup_targets,
    delete_unused_worktree_records_after_cleanup, load_archive_task_plan, load_delete_task_plan,
    load_unarchive_worktree_plan,
};

async fn setup_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.expect("open store");
    (dir, store)
}

#[tokio::test]
async fn archive_plan_aggregates_sessions_and_worktrees() {
    let (_dir, store) = setup_store().await;
    let workspace = store
        .create_workspace("workspace".into(), "/tmp/workspace".into(), VcsKind::Git)
        .await
        .expect("workspace");
    let task = store
        .create_task(workspace.id, "task".into(), None)
        .await
        .expect("task");
    let primary_worktree = store
        .create_worktree(workspace.id, "/tmp/primary".into(), "abc123".into(), None)
        .await
        .expect("primary worktree");
    let session_worktree = store
        .create_worktree(workspace.id, "/tmp/session".into(), "def456".into(), None)
        .await
        .expect("session worktree");
    let session = store
        .create_session(
            task.id,
            workspace.id,
            session_worktree.id,
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
        .set_task_primary_worktree(task.id, primary_worktree.id)
        .await
        .expect("primary worktree");
    let task = store.get_task(task.id).await.expect("task").expect("task");

    let plan = load_archive_task_plan(&store, &task)
        .await
        .expect("archive plan");
    assert_eq!(plan.session_ids, vec![session.id]);
    let worktree_ids = plan
        .worktrees
        .iter()
        .map(|worktree| worktree.id)
        .collect::<HashSet<_>>();
    assert_eq!(worktree_ids.len(), 2);
    assert!(worktree_ids.contains(&primary_worktree.id));
    assert!(worktree_ids.contains(&session_worktree.id));

    let archived = archive_task_record(&store, task.id)
        .await
        .expect("archive task");
    assert!(archived.archived_at.is_some());

    let cleanup_targets = collect_archive_cleanup_targets(&store, task.id, &plan.worktrees).await;
    assert_eq!(cleanup_targets.len(), 2);
    assert!(cleanup_targets
        .iter()
        .all(|target| target.destroy_worktree_on_cleanup));
}

#[tokio::test]
async fn unarchive_plan_uses_visible_sessions_and_primary_worktree() {
    let (_dir, store) = setup_store().await;
    let workspace = store
        .create_workspace("workspace".into(), "/tmp/workspace".into(), VcsKind::Git)
        .await
        .expect("workspace");
    let task = store
        .create_task(workspace.id, "task".into(), None)
        .await
        .expect("task");
    let parent_worktree = store
        .create_worktree(workspace.id, "/tmp/parent".into(), "abc123".into(), None)
        .await
        .expect("parent worktree");
    let archived_child_worktree = store
        .create_worktree(workspace.id, "/tmp/child".into(), "def456".into(), None)
        .await
        .expect("child worktree");
    let parent = store
        .create_session(
            task.id,
            workspace.id,
            parent_worktree.id,
            ExecutionEnvironment::Host,
            "fake".into(),
            "model".into(),
            "implementer".into(),
            None,
            None,
            None,
        )
        .await
        .expect("parent session");
    let child = store
        .create_session(
            task.id,
            workspace.id,
            archived_child_worktree.id,
            ExecutionEnvironment::Host,
            "fake".into(),
            "model".into(),
            "implementer".into(),
            Some(parent.id),
            Some("sub_agent".into()),
            None,
        )
        .await
        .expect("child session");
    store
        .archive_subagent_session(parent.id, child.id)
        .await
        .expect("archive child");
    store
        .set_task_primary_worktree(task.id, parent_worktree.id)
        .await
        .expect("primary worktree");
    let task = store.get_task(task.id).await.expect("task").expect("task");

    let plan = load_unarchive_worktree_plan(&store, &task)
        .await
        .expect("unarchive plan");
    assert_eq!(plan.session_ids, vec![parent.id]);
    let worktree_ids = plan
        .worktrees
        .iter()
        .map(|worktree| worktree.id)
        .collect::<HashSet<_>>();
    assert!(worktree_ids.contains(&parent_worktree.id));
    assert!(!worktree_ids.contains(&archived_child_worktree.id));
}

#[tokio::test]
async fn delete_plan_skips_active_shared_worktrees() {
    let (_dir, store) = setup_store().await;
    let workspace = store
        .create_workspace("workspace".into(), "/tmp/workspace".into(), VcsKind::Git)
        .await
        .expect("workspace");
    let task = store
        .create_task(workspace.id, "task".into(), None)
        .await
        .expect("task");
    let active_sibling = store
        .create_task(workspace.id, "active sibling".into(), None)
        .await
        .expect("active sibling");
    let worktree = store
        .create_worktree(workspace.id, "/tmp/shared".into(), "abc123".into(), None)
        .await
        .expect("worktree");
    store
        .set_task_primary_worktree(task.id, worktree.id)
        .await
        .expect("task primary");
    store
        .set_task_primary_worktree(active_sibling.id, worktree.id)
        .await
        .expect("sibling primary");
    let task = store.get_task(task.id).await.expect("task").expect("task");

    let plan = load_delete_task_plan(&store, &task)
        .await
        .expect("delete plan");
    assert!(plan.cleanup_targets.is_empty());
}

#[tokio::test]
async fn delete_plan_preserves_worktree_rows_shared_with_archived_tasks() {
    let (_dir, store) = setup_store().await;
    let workspace = store
        .create_workspace("workspace".into(), "/tmp/workspace".into(), VcsKind::Git)
        .await
        .expect("workspace");
    let task = store
        .create_task(workspace.id, "task".into(), None)
        .await
        .expect("task");
    let archived_sibling = store
        .create_task(workspace.id, "archived sibling".into(), None)
        .await
        .expect("archived sibling");
    let worktree = store
        .create_worktree(workspace.id, "/tmp/shared".into(), "abc123".into(), None)
        .await
        .expect("worktree");
    store
        .set_task_primary_worktree(task.id, worktree.id)
        .await
        .expect("task primary");
    store
        .set_task_primary_worktree(archived_sibling.id, worktree.id)
        .await
        .expect("sibling primary");
    store
        .archive_task(archived_sibling.id)
        .await
        .expect("archive sibling");
    let task = store.get_task(task.id).await.expect("task").expect("task");

    let plan = load_delete_task_plan(&store, &task)
        .await
        .expect("delete plan");
    assert_eq!(plan.cleanup_targets.len(), 1);
    assert!(!plan.cleanup_targets[0].destroy_worktree_on_cleanup);
}

#[tokio::test]
async fn worktree_rows_are_deleted_only_after_successful_cleanup() {
    let (_dir, store) = setup_store().await;
    let workspace = store
        .create_workspace("workspace".into(), "/tmp/workspace".into(), VcsKind::Git)
        .await
        .expect("workspace");
    let task = store
        .create_task(workspace.id, "task".into(), None)
        .await
        .expect("task");
    let worktree = store
        .create_worktree(workspace.id, "/tmp/worktree".into(), "abc123".into(), None)
        .await
        .expect("worktree");
    store
        .set_task_primary_worktree(task.id, worktree.id)
        .await
        .expect("primary worktree");
    let task = store.get_task(task.id).await.expect("task").expect("task");
    let plan = load_delete_task_plan(&store, &task)
        .await
        .expect("delete plan");
    assert_eq!(plan.cleanup_targets.len(), 1);
    assert!(plan.cleanup_targets[0].destroy_worktree_on_cleanup);

    let deleted =
        delete_unused_worktree_records_after_cleanup(&store, &task, &plan.cleanup_targets, false)
            .await;
    assert!(deleted.is_empty());
    assert!(store
        .get_worktree(worktree.id)
        .await
        .expect("worktree lookup")
        .is_some());

    let deleted =
        delete_unused_worktree_records_after_cleanup(&store, &task, &plan.cleanup_targets, true)
            .await;
    assert_eq!(deleted, vec![worktree.id]);
    assert!(store
        .get_worktree(worktree.id)
        .await
        .expect("worktree lookup")
        .is_none());
}
