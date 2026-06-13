use super::*;

#[cfg(unix)]
#[tokio::test]
async fn stop_container_removes_avf_workspace_container() {
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
        .prepare(&workspace, &worktree, &settings, "http://192.168.64.1:4399")
        .await
        .expect("AVF prepare should start the workspace sandbox");

    assert!(manager
        .stop_container(workspace.id)
        .await
        .expect("stop AVF workspace container"));

    let status = manager
        .container_status(workspace.id)
        .await
        .expect("read stopped AVF workspace container status");
    assert!(
        status.as_ref().map(|value| !value.running).unwrap_or(true),
        "expected stopped or absent sandbox container status, got {status:?}"
    );

    for server in servers {
        server.abort();
    }
    image_server.abort();
}
