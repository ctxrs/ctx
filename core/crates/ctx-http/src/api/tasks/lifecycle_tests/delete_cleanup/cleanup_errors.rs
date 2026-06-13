use super::*;

#[tokio::test]
async fn delete_task_cleanup_errors_preserve_worktree_row_and_index() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_root = temp.path().join("repo");
    std::fs::create_dir_all(&repo_root).expect("create repo root");
    let base_commit = init_git_workspace(&repo_root);
    let fixture = test_state(temp.path()).await;
    let state = fixture.daemon();
    let workspace = state
        .seed_task_lifecycle_workspace_for_test("ws", &repo_root, VcsKind::Git)
        .await
        .expect("create workspace");
    let task = state
        .seed_task_lifecycle_task_for_test(workspace.id, "task")
        .await
        .expect("create task");
    let worktree_id = WorktreeId::new();
    let managed_root = managed_worktree_path(temp.path(), workspace.id, worktree_id);
    std::fs::create_dir_all(
        managed_root
            .parent()
            .expect("managed worktree parent exists"),
    )
    .expect("create managed worktree parent");
    std::fs::write(&managed_root, "not-a-directory").expect("create managed worktree file");
    let worktree = state
        .seed_task_lifecycle_worktree_for_test(TaskLifecycleWorktreeSeed {
            workspace_id: workspace.id,
            owner_task_id: task.id,
            worktree_id,
            root_path: managed_root.clone(),
            base_commit: base_commit.clone(),
            git_branch: format!("ctx/{}/{}", task.id.0, worktree_id.0),
            make_primary: true,
        })
        .await
        .expect("insert worktree");

    let tasks = task_api_lifecycle_state(&state);
    let status = delete_task(tasks, Path(task.id.0.to_string()))
        .await
        .expect("delete task");
    assert_eq!(status, StatusCode::NO_CONTENT);
    let snapshot = task_lifecycle_snapshot(&state, workspace.id, task.id, worktree.id).await;
    assert!(
        snapshot.worktree.is_some(),
        "delete cleanup errors must not drop the worktree row"
    );
    assert!(
        snapshot.worktree_index_workspace_id.is_some(),
        "delete cleanup errors must not drop the worktree index"
    );
    assert!(
        tokio::fs::metadata(&managed_root).await.is_ok(),
        "failed cleanup should leave the managed worktree root in place"
    );
}
