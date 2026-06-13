use super::*;

#[cfg(unix)]
#[tokio::test]
async fn ensure_workspace_container_starts_avf_workspace_vm() {
    let _serial = env_var_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = runtime_manager(&temp).await;
    let helper_path = write_avf_linux_lifecycle_helper(temp.path());
    let sandbox_cli_path = write_ready_runtime_sandbox_cli_shim(temp.path());
    let _helper_guard = EnvGuard::set(AVF_LINUX_HELPER_PATH_ENV, &helper_path.to_string_lossy());
    let _sandbox_cli_available = EnvGuard::set("CTX_TEST_SANDBOX_CLI_AVAILABLE", "1");
    let _sandbox_cli_path = EnvGuard::set(
        CTX_HARNESS_SANDBOX_CLI_PATH_ENV,
        &sandbox_cli_path.to_string_lossy(),
    );
    let (_runtime_guard, servers) = install_test_managed_avf_linux_runtime_source().await;
    let (_image_guard, image_server) =
        install_test_managed_harness_image_source(b"ctx-harness-image".to_vec()).await;
    let workspace = sample_workspace(&temp);
    let settings = ExecutionSettings {
        mode: ExecutionMode::Sandbox,
        container: ContainerExecutionSettings {
            runtime: ctx_settings_model::ContainerRuntimeKind::SharedVmContainer,
            mount_mode: ContainerMountMode::DiskIsolated,
            network_mode: ContainerNetworkMode::All,
            ..ContainerExecutionSettings::default()
        },
    };

    manager
        .ensure_workspace_container(&workspace, &settings, "http://192.168.64.1:4399")
        .await
        .expect("AVF workspace VM should be started for workspace container callers");

    let state = ctx_avf_linux_runtime::workspace_vm_state(temp.path(), workspace.id)
        .expect("workspace VM state");
    assert_eq!(
        state.state,
        ctx_avf_linux_runtime::AvfLinuxSharedVmLifecycleState::Running
    );

    for server in servers {
        server.abort();
    }
    image_server.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn ensure_workspace_container_for_worktree_keeps_avf_workspace_container_ready() {
    let _serial = env_var_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = runtime_manager(&temp).await;
    let helper_path = write_avf_linux_lifecycle_helper(temp.path());
    let sandbox_cli_path = write_ready_runtime_sandbox_cli_shim(temp.path());
    let _helper_guard = EnvGuard::set(AVF_LINUX_HELPER_PATH_ENV, &helper_path.to_string_lossy());
    let _sandbox_cli_available = EnvGuard::set("CTX_TEST_SANDBOX_CLI_AVAILABLE", "1");
    let _sandbox_cli_path = EnvGuard::set(
        CTX_HARNESS_SANDBOX_CLI_PATH_ENV,
        &sandbox_cli_path.to_string_lossy(),
    );
    let (_runtime_guard, servers) = install_test_managed_avf_linux_runtime_source().await;
    let (_image_guard, image_server) =
        install_test_managed_harness_image_source(b"ctx-harness-image".to_vec()).await;
    let workspace = sample_workspace(&temp);
    let worktree = sample_worktree(&temp, workspace.id);
    let settings = ExecutionSettings {
        mode: ExecutionMode::Sandbox,
        container: ContainerExecutionSettings {
            runtime: ctx_settings_model::ContainerRuntimeKind::SharedVmContainer,
            mount_mode: ContainerMountMode::DiskIsolated,
            network_mode: ContainerNetworkMode::All,
            ..ContainerExecutionSettings::default()
        },
    };

    manager
        .ensure_workspace_container_for_worktree(
            &workspace,
            &worktree,
            &settings,
            "http://192.168.64.1:4399",
        )
        .await
        .expect("AVF worktree ensure should keep the workspace sandbox ready");

    let status = manager
        .container_status(workspace.id)
        .await
        .expect("AVF workspace status")
        .expect("AVF workspace container state should exist");
    assert!(status.running);

    for server in servers {
        server.abort();
    }
    image_server.abort();
}
