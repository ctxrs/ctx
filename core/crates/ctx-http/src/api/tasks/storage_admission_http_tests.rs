mod assertions;
mod fixtures;

use serde_json::json;

use ctx_core::models::VcsKind;
use ctx_settings_model::{
    ContainerExecutionSettings, ContainerMountMode, ContainerNetworkMode, ContainerRuntimeKind,
    ExecutionMode, ExecutionSettings,
};

use assertions::assert_storage_admission_rejected_before_copy;
use fixtures::{
    init_git_workspace, install_unreleased_host_reserve_storage_override, post_json,
    save_test_execution_settings, test_state, EnvVarGuard,
};

#[cfg(unix)]
#[tokio::test]
async fn create_task_rejects_before_disk_isolated_copy_when_host_reserve_is_unreleased() {
    let _process_env = crate::test_support::process_env_test_lock().lock().await;
    let _serial = crate::test_support::sandbox_cli_env_test_lock()
        .lock()
        .await;
    let temp = tempfile::tempdir().expect("tempdir");
    let data_root = temp.path().join("data");
    let repo_root = temp.path().join("repo");
    std::fs::create_dir_all(&data_root).expect("create data root");
    std::fs::create_dir_all(&repo_root).expect("create repo root");
    init_git_workspace(&repo_root);

    let fixture = test_state(&data_root).await;
    let state = fixture.daemon();
    let workspace = state
        .seed_workspace_for_test("ws", &repo_root, VcsKind::Git)
        .await
        .expect("create workspace");

    let log_path = temp.path().join("sandbox-cli.log");
    let container_name = ctx_workspace_container::workspace_container_name(workspace.id);
    let sandbox_cli_path = crate::test_support::write_running_container_sandbox_cli_shim(
        temp.path(),
        &log_path,
        &container_name,
    );
    let _sandbox_cli = EnvVarGuard::set(
        "CTX_HARNESS_SANDBOX_CLI_PATH",
        &sandbox_cli_path.to_string_lossy(),
    );
    let _sandbox_cli_available = EnvVarGuard::set("CTX_TEST_SANDBOX_CLI_AVAILABLE", "1");

    save_test_execution_settings(
        &fixture,
        ExecutionSettings {
            mode: ExecutionMode::Sandbox,
            container: ContainerExecutionSettings {
                runtime: ContainerRuntimeKind::NativeContainer,
                mount_mode: ContainerMountMode::DiskIsolated,
                network_mode: ContainerNetworkMode::All,
                image: Some("ctx/test-sandbox:latest".to_string()),
                ..ContainerExecutionSettings::default()
            },
        },
    )
    .await;

    let app = fixture.router();
    let _storage_override = install_unreleased_host_reserve_storage_override(workspace.id);

    let (status, body) = post_json(
        &app,
        format!("/api/workspaces/{}/tasks", workspace.id.0),
        json!({
            "title": "storage admission",
            "default_session": {
                "provider_id": "fake",
                "model_id": "fake-model",
                "execution_environment": "sandbox",
            },
        }),
    )
    .await;

    let log = std::fs::read_to_string(&log_path).unwrap_or_default();
    assert_storage_admission_rejected_before_copy(
        status,
        body,
        &log,
        workspace.id,
        &container_name,
        &data_root,
    );
}
