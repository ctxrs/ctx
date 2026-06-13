use super::*;
use ctx_sandbox_container_runtime::CTX_HARNESS_SANDBOX_CLI_PATH_ENV;

#[cfg(unix)]
#[tokio::test]
async fn ensure_container_machine_ready_prefetches_avf_runtime_without_starting_a_global_vm() {
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
    let settings = ContainerExecutionSettings {
        runtime: ctx_settings_model::ContainerRuntimeKind::SharedVmContainer,
        ..ContainerExecutionSettings::default()
    };

    manager
        .ensure_container_machine_ready(&settings, None)
        .await
        .expect("AVF runtime prefetch should succeed");

    let runtime_state = super::selected_runtime_state(temp.path(), &settings)
        .await
        .expect("read AVF runtime state");
    assert_eq!(runtime_state, (true, true));

    for server in servers {
        server.abort();
    }
}
