use super::super::*;

#[tokio::test]
async fn unarchive_task_fails_closed_for_corrupt_binding_snapshot() {
    let _serial = crate::test_support::sandbox_cli_env_test_lock()
        .lock()
        .await;
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
    state
        .seed_task_lifecycle_sandbox_binding_for_test(TaskLifecycleSandboxBindingSeed {
            worktree_id: worktree.id,
            workspace_id: workspace.id,
            substrate: SandboxSubstrate::NativeContainer,
            live_workspace_root: ctx_sandbox_contract::CTX_CONTAINER_WORKSPACE_ROOT.to_string(),
            live_worktree_root: ctx_sandbox_contract::container_worktree_root(worktree.id)
                .to_string_lossy()
                .to_string(),
            execution_settings_json: Some(
                serde_json::json!({
                    "mode": "host",
                    "container": {
                        "runtime": "native_container",
                        "mount_mode": "disk_isolated",
                        "network_mode": "all",
                        "allowlist": [],
                        "image": null
                    }
                })
                .to_string(),
            ),
            container_name: Some(ctx_workspace_container::workspace_container_name(
                workspace.id,
            )),
            host_materialization_root: None,
        })
        .await
        .expect("insert corrupt sandbox binding");

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

    let tasks = task_api_lifecycle_state(&state);
    let status = unarchive_task(tasks, Path(task.id.0.to_string()))
        .await
        .expect_err("corrupt binding snapshot should fail closed");
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let snapshot = task_lifecycle_snapshot(&state, workspace.id, task.id, worktree.id).await;
    assert!(
        snapshot
            .task
            .expect("task should still exist")
            .archived_at
            .is_some(),
        "failed unarchive should leave the task archived"
    );
    assert!(
        tokio::fs::metadata(&managed_root).await.is_ok(),
        "failed unarchive should not destroy the canonical managed worktree root"
    );
    assert!(
        snapshot.sandbox_binding.is_some(),
        "failed unarchive should preserve the persisted binding row for repair"
    );
}
