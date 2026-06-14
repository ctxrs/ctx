use super::*;

#[tokio::test]
async fn archive_task_reclaims_managed_worktree_but_preserves_rematerialization_state() {
    let _serial = crate::test_support::sandbox_cli_env_test_lock()
        .lock()
        .await;
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

    let host_materialization_root = temp.path().join("host-shadow");
    std::fs::create_dir_all(&host_materialization_root).expect("create host shadow root");
    state
        .seed_task_lifecycle_sandbox_binding_for_test(TaskLifecycleSandboxBindingSeed {
            worktree_id: worktree.id,
            workspace_id: workspace.id,
            substrate: SandboxSubstrate::SharedVmContainer,
            live_workspace_root: ctx_sandbox_contract::CTX_CONTAINER_WORKSPACE_ROOT.to_string(),
            live_worktree_root: ctx_sandbox_contract::container_worktree_root(worktree.id)
                .to_string_lossy()
                .to_string(),
            execution_settings_json: None,
            container_name: Some(ctx_workspace_container::workspace_container_name(
                workspace.id,
            )),
            host_materialization_root: Some(host_materialization_root.clone()),
        })
        .await
        .expect("insert sandbox binding");

    let log_path = temp.path().join("sandbox-cli.log");
    let sandbox_cli_path = crate::test_support::write_running_container_sandbox_cli_shim(
        temp.path(),
        &log_path,
        &ctx_workspace_container::workspace_container_name(workspace.id),
    );
    let _sandbox_cli = EnvVarGuard::set(
        "CTX_HARNESS_SANDBOX_CLI_PATH",
        &sandbox_cli_path.to_string_lossy(),
    );
    let _sandbox_cli_available = EnvVarGuard::set("CTX_TEST_SANDBOX_CLI_AVAILABLE", "1");

    let tasks = task_api_lifecycle_state(&state);
    let Json(_) = archive_task(tasks, Path(task.id.0.to_string()))
        .await
        .expect("archive task");
    let snapshot = task_lifecycle_snapshot(&state, workspace.id, task.id, worktree.id).await;
    let archived_task = snapshot.task.expect("archived task exists");
    assert!(
        archived_task.archived_at.is_some(),
        "task should be archived after archive_task"
    );
    assert!(
        tokio::fs::metadata(&managed_root).await.is_err(),
        "archive should reclaim the canonical managed worktree root"
    );
    assert!(
        !branch_exists(
            &repo_root,
            worktree.git_branch.as_deref().expect("branch name"),
        )
        .await
        .expect("check branch"),
        "archive should reclaim the task worktree branch"
    );
    assert!(
        snapshot.worktree.is_some(),
        "archive should preserve the worktree row"
    );
    assert!(
        snapshot.sandbox_binding.is_some(),
        "archive should preserve the sandbox binding row for rematerialization"
    );
    assert!(
        tokio::fs::metadata(&host_materialization_root)
            .await
            .is_err(),
        "archive should remove the sandbox host materialization root"
    );
}

#[tokio::test]
async fn archive_task_reports_cleanup_failure_when_branch_reclaim_fails() {
    let temp = tempfile::tempdir().expect("tempdir");
    let ManagedTaskFixture {
        repo_root,
        state,
        task,
        worktree,
        managed_root,
        ..
    } = create_managed_task_fixture(temp.path()).await;
    let state = state.daemon();
    let branch = worktree
        .git_branch
        .as_deref()
        .expect("branch name")
        .to_string();
    let _branch_lock = create_branch_lock(&repo_root, &branch);

    let tasks = task_api_lifecycle_state(&state);
    let Json(response) = archive_task(tasks, Path(task.id.0.to_string()))
        .await
        .expect("archive task");

    assert!(
        response.cleanup_failed,
        "archive should report cleanup failure when branch reclaim fails"
    );
    assert!(
        response.task.archived_at.is_some(),
        "task should still be archived after reporting cleanup failure"
    );
    assert!(
        tokio::fs::metadata(&managed_root).await.is_err(),
        "archive should still reclaim the managed worktree root"
    );
    assert!(
        branch_exists(&repo_root, &branch)
            .await
            .expect("check branch"),
        "archive should report failure if the ctx branch could not be reclaimed"
    );
}

#[tokio::test]
async fn archive_task_runs_cleanup_command_before_reclaiming_managed_worktree() {
    let temp = tempfile::tempdir().expect("tempdir");
    let ManagedTaskFixture {
        state,
        workspace,
        task,
        worktree,
        managed_root,
        ..
    } = create_managed_task_fixture(temp.path()).await;
    let state = state.daemon();
    let marker = temp.path().join("cleanup-marker.txt");
    seed_cleanup_command(
        state,
        workspace.id,
        format!(
            "printf '%s|%s|%s\\n' \"$CTX_TASK_ID\" \"$CTX_WORKTREE_ID\" \"$CTX_WORKTREE_ROOT\" > {}",
            shell_quote_path(&marker)
        ),
    )
    .await;

    let tasks = task_api_lifecycle_state(state);
    let Json(response) = archive_task(tasks, Path(task.id.0.to_string()))
        .await
        .expect("archive task");

    assert!(
        !response.cleanup_failed,
        "successful cleanup command should not fail archive cleanup"
    );
    assert!(
        tokio::fs::metadata(&managed_root).await.is_err(),
        "successful cleanup command should allow managed worktree reclaim"
    );
    let marker_contents =
        std::fs::read_to_string(&marker).expect("cleanup command should write marker");
    assert!(
        marker_contents.contains(&task.id.0.to_string()),
        "cleanup command should receive CTX_TASK_ID"
    );
    assert!(
        marker_contents.contains(&worktree.id.0.to_string()),
        "cleanup command should receive CTX_WORKTREE_ID"
    );
    assert!(
        marker_contents.contains(&managed_root.to_string_lossy().to_string()),
        "cleanup command should run with the managed worktree root"
    );
}

#[tokio::test]
async fn archive_task_preserves_managed_worktree_when_cleanup_command_fails() {
    let temp = tempfile::tempdir().expect("tempdir");
    let ManagedTaskFixture {
        state,
        workspace,
        task,
        worktree,
        managed_root,
        ..
    } = create_managed_task_fixture(temp.path()).await;
    let state = state.daemon();
    seed_cleanup_command(state, workspace.id, "exit 7".to_string()).await;

    let tasks = task_api_lifecycle_state(state);
    let Json(response) = archive_task(tasks, Path(task.id.0.to_string()))
        .await
        .expect("archive task");
    let snapshot = task_lifecycle_snapshot(state, workspace.id, task.id, worktree.id).await;

    assert!(
        response.cleanup_failed,
        "failing cleanup command should report archive cleanup failure"
    );
    assert!(
        response.task.archived_at.is_some(),
        "task should still be archived after cleanup command failure"
    );
    assert!(
        tokio::fs::metadata(&managed_root).await.is_ok(),
        "failing cleanup command should preserve the managed worktree root"
    );
    assert!(
        snapshot.worktree.is_some(),
        "failing cleanup command should preserve the worktree row"
    );
}

async fn seed_cleanup_command(
    state: &ctx_daemon::test_support::TestDaemon,
    workspace_id: WorkspaceId,
    cleanup_command: String,
) {
    let store = state
        .store_for_workspace(workspace_id)
        .await
        .expect("workspace store");
    ctx_workspace_config::update_worktree_bootstrap_config(
        &store,
        ctx_workspace_config::WorktreeBootstrapConfigUpdate {
            cleanup_command: Some(cleanup_command),
            cleanup_timeout_sec: Some(5),
            ..Default::default()
        },
    )
    .await
    .expect("seed cleanup command");
}

fn shell_quote_path(path: &StdPath) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}
