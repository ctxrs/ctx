use super::*;

#[tokio::test]
async fn delete_task_preserves_worktree_for_archived_sibling_session_reference() {
    let temp = tempfile::tempdir().expect("tempdir");
    let ManagedTaskFixture {
        repo_root,
        state,
        workspace,
        task: active_task,
        worktree,
        managed_root,
    } = create_managed_task_fixture(temp.path()).await;
    let state = state.daemon();
    let archived_task = state
        .seed_task_lifecycle_task_for_test(workspace.id, "archived")
        .await
        .expect("create archived task");
    state
        .seed_task_lifecycle_session_for_test(TaskLifecycleSessionSeed {
            task_id: archived_task.id,
            workspace_id: workspace.id,
            worktree_id: worktree.id,
            execution_environment: ExecutionEnvironment::Sandbox,
            title: "archived".to_string(),
            parent_session_id: None,
            role: None,
        })
        .await
        .expect("create archived task session");
    state
        .archive_task_lifecycle_row_for_test(workspace.id, archived_task.id)
        .await
        .expect("archive sibling task");

    let tasks = task_api_lifecycle_state(&state);
    let status = delete_task(tasks, Path(active_task.id.0.to_string()))
        .await
        .expect("delete active task");
    assert_eq!(status, StatusCode::NO_CONTENT);
    let snapshot =
        task_lifecycle_snapshot(&state, workspace.id, archived_task.id, worktree.id).await;
    assert!(
        snapshot
            .task
            .expect("archived sibling exists")
            .archived_at
            .is_some(),
        "archived sibling should remain archived after deleting the active task"
    );
    assert!(
        snapshot.worktree.is_some(),
        "delete should preserve the worktree row while an archived sibling still references it"
    );
    assert!(
        snapshot.worktree_index_workspace_id.is_some(),
        "delete should preserve the worktree index while an archived sibling still references it"
    );
    assert!(
        tokio::fs::metadata(&managed_root).await.is_ok(),
        "delete should preserve the managed worktree root for archived sibling history"
    );
    assert!(
        branch_exists(
            &repo_root,
            worktree.git_branch.as_deref().expect("branch name"),
        )
        .await
        .expect("check branch"),
        "delete should preserve the branch while an archived sibling still references the worktree"
    );
}
