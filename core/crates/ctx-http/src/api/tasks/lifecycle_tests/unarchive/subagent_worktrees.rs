use super::super::*;

#[tokio::test]
async fn unarchive_task_does_not_recreate_archived_subagent_worktrees() {
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
            execution_environment: ExecutionEnvironment::Host,
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
            execution_environment: ExecutionEnvironment::Host,
            title: "child".to_string(),
            parent_session_id: Some(parent_session.id),
            role: Some("sub_agent".to_string()),
        })
        .await
        .expect("create child session");
    state
        .archive_task_lifecycle_subagent_session_for_test(
            workspace.id,
            parent_session.id,
            child_session.id,
        )
        .await
        .expect("archive child session");

    git(
        &[
            "worktree",
            "remove",
            "--force",
            child_root.to_string_lossy().as_ref(),
        ],
        &repo_root,
    );
    git(
        &[
            "branch",
            "-D",
            child_worktree.git_branch.as_deref().expect("child branch"),
        ],
        &repo_root,
    );
    assert!(
        tokio::fs::metadata(&child_root).await.is_err(),
        "test setup should remove the archived child managed worktree root"
    );

    let tasks = task_api_lifecycle_state(&state);
    let Json(_) = archive_task(tasks, Path(task.id.0.to_string()))
        .await
        .expect("archive task");
    let tasks = task_api_lifecycle_state(&state);
    let Json(unarchived) = unarchive_task(tasks, Path(task.id.0.to_string()))
        .await
        .expect("unarchive task");

    assert!(unarchived.archived_at.is_none());
    assert!(
        tokio::fs::metadata(&parent_root).await.is_ok(),
        "unarchive should recreate the active parent managed worktree root"
    );
    assert!(
        tokio::fs::metadata(&child_root).await.is_err(),
        "unarchive should not recreate archived subagent managed worktrees"
    );
    assert!(
        branch_exists(
            &repo_root,
            parent_worktree
                .git_branch
                .as_deref()
                .expect("parent branch name"),
        )
        .await
        .expect("check parent branch"),
        "unarchive should restore the active parent branch"
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
        "unarchive should not restore archived subagent branches"
    );
}
