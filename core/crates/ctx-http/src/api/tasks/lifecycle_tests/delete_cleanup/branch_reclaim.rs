use super::*;

#[tokio::test]
async fn delete_task_removes_worktree_metadata_even_when_branch_reclaim_fails() {
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

    let _branch_lock = create_branch_lock(
        &repo_root,
        worktree.git_branch.as_deref().expect("branch name"),
    );

    let tasks = task_api_lifecycle_state(&state);
    let status = delete_task(tasks, Path(task.id.0.to_string()))
        .await
        .expect("delete task");
    assert_eq!(status, StatusCode::NO_CONTENT);
    let snapshot = task_lifecycle_snapshot(&state, workspace.id, task.id, worktree.id).await;
    assert!(
        snapshot.worktree.is_none(),
        "branch reclaim failure must not keep the worktree row behind"
    );
    assert!(
        snapshot.worktree_index_workspace_id.is_none(),
        "branch reclaim failure must not keep the worktree index behind"
    );
    assert!(
        tokio::fs::metadata(&managed_root).await.is_err(),
        "managed worktree root should still be removed before branch reclaim is attempted"
    );
    assert!(
        branch_exists(
            &repo_root,
            worktree.git_branch.as_deref().expect("branch name"),
        )
        .await
        .expect("check branch"),
        "branch lock should keep the branch behind so the test exercises best-effort branch cleanup"
    );
}
