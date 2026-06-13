use super::*;
use ctx_sandbox_container_runtime::CTX_HARNESS_SANDBOX_CLI_PATH_ENV;

#[cfg(unix)]
#[tokio::test]
async fn ensure_workspace_container_after_runtime_ready_starts_avf_workspace_vm() {
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
    let image_bytes = b"ctx-harness-image".to_vec();
    let (image_url, image_server) =
        spawn_static_http_server_with_suffix(image_bytes.clone(), "ctx-harness.tar").await;
    let _image_guard = bundled_assets::override_managed_ctx_harness_image_source_for_test(
        bundled_assets::ManagedArtifactSource {
            uri: image_url,
            sha256: hex::encode(Sha256::digest(&image_bytes)),
        },
    );
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
        .ensure_container_machine_ready(&settings.container, None)
        .await
        .expect("AVF runtime artifacts should prewarm");
    manager
        .ensure_workspace_container_after_runtime_ready_with_observer(
            &workspace,
            &settings,
            "http://192.168.64.1:4399",
            None,
        )
        .await
        .expect("AVF workspace VM should start from runtime-ready path");

    let state = ctx_avf_linux_runtime::workspace_vm_state(temp.path(), workspace.id)
        .expect("workspace VM state");
    assert_eq!(
        state.state,
        ctx_avf_linux_runtime::AvfLinuxSharedVmLifecycleState::Running
    );
    let log_path = temp.path().join(format!(
        "managed/vms/avf-linux/{}/{}/shared/sandbox-cli-invocations.log",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    let log = std::fs::read_to_string(&log_path).expect("read AVF sandbox CLI invocation log");
    assert!(
        log.contains("load -i"),
        "runtime-ready path should still load the managed harness image before run: {log}"
    );

    for server in servers {
        server.abort();
    }
    image_server.abort();
}
