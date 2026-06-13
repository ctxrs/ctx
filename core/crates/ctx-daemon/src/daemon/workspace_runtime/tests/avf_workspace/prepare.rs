use super::*;
use ctx_sandbox_container_runtime::CTX_HARNESS_SANDBOX_CLI_PATH_ENV;

#[cfg(unix)]
#[tokio::test]
async fn prepare_returns_avf_linux_vm_plan_after_workspace_vm_and_container_ready() {
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

    let plan = manager
        .prepare(&workspace, &worktree, &settings, "http://192.168.64.1:4399")
        .await
        .expect("AVF prepare should now return a container-backed AVF plan");
    match &plan.runtime {
        HarnessRuntimeKind::SharedVmContainer => {}
        HarnessRuntimeKind::Host => panic!("expected AVF Linux VM runtime"),
        HarnessRuntimeKind::NativeContainer { .. } => panic!("expected AVF Linux VM runtime"),
    }
    assert_eq!(
        plan.env_overrides
            .get(ctx_harness_runtime::CTX_HARNESS_RUNTIME_KIND_ENV)
            .map(String::as_str),
        Some("shared_vm_container")
    );
    let workspace_id = workspace.id.0.to_string();
    let worktree_id = worktree.id.0.to_string();
    assert_eq!(
        plan.env_overrides
            .get("CTX_AVF_WORKSPACE_ID")
            .map(String::as_str),
        Some(workspace_id.as_str())
    );
    assert_eq!(
        plan.env_overrides
            .get("CTX_AVF_WORKTREE_ID")
            .map(String::as_str),
        Some(worktree_id.as_str())
    );
    assert_eq!(
        plan.env_overrides
            .get("CTX_AVF_HOST_WORKTREE_ROOT")
            .map(String::as_str),
        Some(worktree.root_path.as_str())
    );
    assert_eq!(
        plan.env_overrides.get("CTX_DAEMON_URL").map(String::as_str),
        Some("http://192.168.64.1:4399")
    );
    assert!(plan.env_overrides.contains_key(AVF_LINUX_HELPER_PATH_ENV));

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
