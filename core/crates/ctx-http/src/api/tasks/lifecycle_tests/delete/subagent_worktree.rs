use super::*;

#[tokio::test]
async fn delete_task_cleans_up_archived_subagent_worktree() {
    let temp = tempfile::tempdir().expect("tempdir");
    let ManagedTaskFixture {
        repo_root,
        state,
        workspace,
        task,
        worktree: parent_worktree,
        managed_root: parent_root,
    } = create_managed_task_fixture(temp.path()).await;
    let state = state.daemon();
    let base_commit = parent_worktree.base_commit_sha.clone();
    let (child_worktree, child_root) = insert_managed_worktree(
        &state,
        temp.path(),
        &workspace,
        task.id,
        &repo_root,
        &base_commit,
        false,
    )
    .await;
    let parent_session = state
        .seed_task_lifecycle_session_for_test(TaskLifecycleSessionSeed {
            task_id: task.id,
            workspace_id: workspace.id,
            worktree_id: parent_worktree.id,
            execution_environment: ExecutionEnvironment::Sandbox,
            title: "parent".to_string(),
            parent_session_id: None,
            role: None,
        })
        .await
        .expect("create parent session");
    let child_session = state
        .seed_task_lifecycle_session_for_test(TaskLifecycleSessionSeed {
            task_id: task.id,
            workspace_id: workspace.id,
            worktree_id: child_worktree.id,
            execution_environment: ExecutionEnvironment::Sandbox,
            title: "child".to_string(),
            parent_session_id: Some(parent_session.id),
            role: Some("sub_agent".to_string()),
        })
        .await
        .expect("create child session");
    assert!(state
        .archive_task_lifecycle_subagent_session_for_test(
            workspace.id,
            parent_session.id,
            child_session.id,
        )
        .await
        .expect("archive child session"));

    let tasks = task_api_lifecycle_state(&state);
    let status = delete_task(tasks, Path(task.id.0.to_string()))
        .await
        .expect("delete task");
    assert_eq!(status, StatusCode::NO_CONTENT);

    assert!(
        task_lifecycle_snapshot(&state, workspace.id, task.id, parent_worktree.id)
            .await
            .worktree
            .is_none(),
        "delete should remove the parent worktree row"
    );
    assert!(
        task_lifecycle_snapshot(&state, workspace.id, task.id, child_worktree.id)
            .await
            .worktree
            .is_none(),
        "delete should remove the archived child worktree row"
    );
    assert!(
        task_lifecycle_snapshot(&state, workspace.id, task.id, parent_worktree.id)
            .await
            .worktree_index_workspace_id
            .is_none(),
        "delete should remove the parent worktree index"
    );
    assert!(
        task_lifecycle_snapshot(&state, workspace.id, task.id, child_worktree.id)
            .await
            .worktree_index_workspace_id
            .is_none(),
        "delete should remove the archived child worktree index"
    );
    assert!(
        tokio::fs::metadata(&parent_root).await.is_err(),
        "delete should remove the parent managed worktree root"
    );
    assert!(
        tokio::fs::metadata(&child_root).await.is_err(),
        "delete should remove the archived child managed worktree root"
    );
    assert!(
        !branch_exists(
            &repo_root,
            parent_worktree
                .git_branch
                .as_deref()
                .expect("parent branch name"),
        )
        .await
        .expect("check parent branch"),
        "delete should remove the parent branch"
    );
    assert!(
        !branch_exists(
            &repo_root,
            child_worktree
                .git_branch
                .as_deref()
                .expect("child branch name"),
        )
        .await
        .expect("check child branch"),
        "delete should remove the archived child branch"
    );
}
