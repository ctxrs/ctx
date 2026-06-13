use super::*;

#[tokio::test]
async fn delete_task_removes_standalone_managed_worktree_when_workspace_root_is_missing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let ManagedTaskFixture {
        repo_root,
        state,
        workspace,
        task,
        worktree,
        managed_root,
    } = create_managed_task_fixture(temp.path()).await;
    let state = state.daemon();
    standaloneize_worktree_git_dir(&managed_root)
        .await
        .expect("standaloneize managed worktree");
    tokio::fs::remove_dir_all(&repo_root)
        .await
        .expect("remove source workspace root");

    let tasks = task_api_lifecycle_state(&state);
    let status = delete_task(tasks, Path(task.id.0.to_string()))
        .await
        .expect("delete task");
    assert_eq!(status, StatusCode::NO_CONTENT);
    let snapshot = task_lifecycle_snapshot(&state, workspace.id, task.id, worktree.id).await;
    assert!(
        snapshot.worktree.is_none(),
        "unused worktree row should be removed on task delete"
    );
    assert!(
        snapshot.worktree_index_workspace_id.is_none(),
        "unused worktree index should be removed on task delete"
    );
    assert!(
        tokio::fs::metadata(&managed_root).await.is_err(),
        "standalone managed worktree root should be removed on task delete"
    );
}
