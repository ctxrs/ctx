use super::*;

#[cfg(unix)]
#[tokio::test]
async fn prepare_reuses_running_workspace_container_without_front_loading_image_readiness() {
    let _serial = env_var_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let log_path = temp.path().join("sandbox-cli-invocations.log");
    let manager = runtime_manager(&temp).await;
    let workspace = sample_workspace(&temp);
    let worktree = sample_worktree(&temp, workspace.id);
    let container_name = workspace_container_name(workspace.id);
    let settings = ExecutionSettings {
        mode: ExecutionMode::Sandbox,
        container: ContainerExecutionSettings {
            network_mode: ContainerNetworkMode::All,
            runtime: ctx_settings_model::ContainerRuntimeKind::NativeContainer,
            ..Default::default()
        },
    };

    let sandbox_cli_path =
        write_running_container_sandbox_cli_shim(temp.path(), &log_path, &container_name);
    let _guard = EnvGuard::set(
        "CTX_HARNESS_SANDBOX_CLI_PATH",
        &sandbox_cli_path.to_string_lossy(),
    );
    let _available = EnvGuard::set("CTX_TEST_SANDBOX_CLI_AVAILABLE", "1");

    let plan = manager
        .prepare(&workspace, &worktree, &settings, "http://127.0.0.1:4399")
        .await
        .expect("running workspace container should be reused without image checks");

    match plan.runtime {
        HarnessRuntimeKind::NativeContainer { name } => assert_eq!(name, container_name),
        HarnessRuntimeKind::Host => panic!("expected container runtime"),
        HarnessRuntimeKind::SharedVmContainer => panic!("expected sandbox container runtime"),
    }

    let log = std::fs::read_to_string(&log_path).expect("read sandbox CLI invocation log");
    assert!(
        log.contains(&format!("container inspect {container_name}")),
        "expected running container existence check in log:\n{log}"
    );
    assert!(
        log.contains(&format!(
            "container inspect --format {{{{.State.Running}}}} {container_name}"
        )),
        "expected running-container inspect in log:\n{log}"
    );
    assert!(
        !log.contains("image inspect"),
        "running-container reuse should not front-load image checks:\n{log}"
    );
    assert!(
        !log.contains("run -d --name"),
        "running-container reuse should not recreate the container:\n{log}"
    );
}
