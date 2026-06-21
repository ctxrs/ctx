use super::*;
use ctx_sandbox_container_runtime::CTX_HARNESS_SANDBOX_CLI_PATH_ENV;

#[cfg(unix)]
#[tokio::test]
async fn ready_runtime_sandbox_cli_short_circuits_network_cleanup_scripts_for_sh_c() {
    let _serial = env_var_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let _helper_path = write_avf_linux_lifecycle_helper(temp.path());
    let sandbox_cli_path = write_ready_runtime_sandbox_cli_shim(temp.path());
    let _sandbox_cli_available = EnvGuard::set("CTX_TEST_SANDBOX_CLI_AVAILABLE", "1");
    let _sandbox_cli_path = EnvGuard::set(
        CTX_HARNESS_SANDBOX_CLI_PATH_ENV,
        &sandbox_cli_path.to_string_lossy(),
    );

    let mut cmd = sandbox_container_command(temp.path()).expect("sandbox container command");
    cmd.arg("exec")
        .arg("--user")
        .arg("0")
        .arg("ctx-harness-network-cleanup")
        .arg("sh")
        .arg("-c")
        .arg("iptables -t nat -F OUTPUT");
    let output = command_output_with_timeout(cmd, Duration::from_secs(60))
        .await
        .expect("run fake network cleanup");
    assert!(
        output.status.success(),
        "fake AVF sandbox CLI should short-circuit host network cleanup scripts: {}",
        command_output_message(&output)
    );
}

#[tokio::test]
async fn sandbox_mode_errors_when_container_cli_is_unavailable() {
    let _serial = env_var_test_lock().lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let sandbox_cli_path = write_failing_sandbox_cli(tmp.path());
    let _sandbox_cli_path = EnvGuard::set(
        CTX_HARNESS_SANDBOX_CLI_PATH_ENV,
        &sandbox_cli_path.to_string_lossy(),
    );
    let manager = runtime_manager(&tmp).await;
    let workspace = sample_workspace(&tmp);
    let worktree = sample_worktree(&tmp, workspace.id);
    let settings = ExecutionSettings {
        mode: ExecutionMode::Sandbox,
        container: ContainerExecutionSettings {
            runtime: ctx_settings_model::ContainerRuntimeKind::NativeContainer,
            ..ContainerExecutionSettings::default()
        },
    };

    let err = manager
        .prepare(&workspace, &worktree, &settings, "http://127.0.0.1:9999")
        .await
        .unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("native sandbox container runtime")
            || message.contains("sandbox container CLI unavailable")
            || message.contains("container runtime failed"),
        "unexpected error message: {message}"
    );
}

fn write_failing_sandbox_cli(root: &std::path::Path) -> std::path::PathBuf {
    let path = root.join(if cfg!(windows) {
        "failing-sandbox-cli.cmd"
    } else {
        "failing-sandbox-cli.sh"
    });

    #[cfg(windows)]
    {
        std::fs::write(
            &path,
            "@echo off\r\necho sandbox runtime unavailable 1>&2\r\nexit /b 125\r\n",
        )
        .unwrap();
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::write(
            &path,
            "#!/bin/sh\necho 'sandbox runtime unavailable' >&2\nexit 125\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    path
}
