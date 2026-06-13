use super::*;

#[cfg(unix)]
#[tokio::test]
async fn prepare_in_host_mode_does_not_refresh_local_sandbox_activity() {
    use std::os::unix::fs::PermissionsExt;

    let _serial = env_var_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = runtime_manager(&temp).await;
    let workspace = sample_workspace(&temp);
    let worktree = sample_worktree(&temp, workspace.id);
    let log_path = temp.path().join("sandbox-cli-invocations.log");
    let sandbox_cli_path = temp.path().join("sandbox-cli.sh");
    std::fs::write(
        &sandbox_cli_path,
        format!(
            "#!/bin/sh\nLOG=\"{log}\"\nprintf '%s\\n' \"$*\" >> \"$LOG\"\necho \"unexpected sandbox CLI invocation: $*\" >&2\nexit 1\n",
            log = log_path.display(),
        ),
    )
    .expect("write sandbox CLI shim");
    std::fs::set_permissions(&sandbox_cli_path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod sandbox CLI shim");
    let _guard = EnvGuard::set(
        "CTX_HARNESS_SANDBOX_CLI_PATH",
        &sandbox_cli_path.to_string_lossy(),
    );
    let before_prepare = Instant::now() - Duration::from_secs(600);
    manager.set_last_activity_for_test(before_prepare);
    let settings = ExecutionSettings {
        mode: ExecutionMode::Host,
        container: ContainerExecutionSettings::default(),
    };

    let plan = manager
        .prepare(&workspace, &worktree, &settings, "http://127.0.0.1:4399")
        .await
        .expect("host prepare should succeed without touching the local sandbox");
    match plan.runtime {
        HarnessRuntimeKind::Host => {}
        HarnessRuntimeKind::NativeContainer { .. } => panic!("expected host runtime"),
        HarnessRuntimeKind::SharedVmContainer => panic!("expected host runtime"),
    }
    assert!(
        std::fs::read_to_string(&log_path)
            .unwrap_or_default()
            .is_empty(),
        "host prepare should not invoke sandbox CLI"
    );
    let idle_for = manager.runtime_idle_for();
    assert!(
        idle_for >= Duration::from_secs(540),
        "host prepare should not refresh local sandbox activity; idle_for={idle_for:?}"
    );
}
