use super::*;

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tokio::test]
async fn ensure_sandbox_machine_running_recreates_immediately_for_already_running_unreachable_machine(
) {
    use std::os::unix::fs::PermissionsExt;

    let _serial = env_var_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let log_path = temp.path().join("sandbox-cli-invocations.log");
    let state_path = temp.path().join("sandbox-machine-ready");
    let start_count_path = temp.path().join("sandbox-machine-start-count");
    let sandbox_cli_path = temp.path().join("sandbox-cli.sh");
    std::fs::write(
            &sandbox_cli_path,
            format!(
                "#!/bin/sh\nLOG=\"{log}\"\nSTATE=\"{state}\"\nSTART_COUNT=\"{start_count}\"\nprintf '%s\\n' \"$*\" >> \"$LOG\"\nif [ \"$1\" = \"info\" ]; then\n  if [ -f \"$STATE\" ]; then\n    printf '{{}}\\n'\n    exit 0\n  fi\n  echo 'sandbox runtime unreachable' >&2\n  exit 125\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"inspect\" ]; then\n  printf '[]\\n'\n  exit 0\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"rm\" ]; then\n  rm -f \"$STATE\"\n  exit 0\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"init\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"start\" ]; then\n  count=0\n  if [ -f \"$START_COUNT\" ]; then\n    count=$(cat \"$START_COUNT\")\n  fi\n  count=$((count + 1))\n  printf '%s' \"$count\" > \"$START_COUNT\"\n  if [ \"$count\" -eq 1 ]; then\n    echo 'Error: unable to start \"ctx\": already running' >&2\n    exit 125\n  fi\n  touch \"$STATE\"\n  exit 0\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"stop\" ]; then\n  rm -f \"$STATE\"\n  exit 0\nfi\nexit 0\n",
                log = log_path.display(),
                state = state_path.display(),
                start_count = start_count_path.display(),
            ),
        )
        .expect("write sandbox CLI shim");
    std::fs::set_permissions(&sandbox_cli_path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod sandbox CLI shim");
    let _guard = EnvGuard::set(
        "CTX_HARNESS_SANDBOX_CLI_PATH",
        &sandbox_cli_path.to_string_lossy(),
    );
    let (_machine_cache_guard, machine_cache_server) =
        install_test_managed_machine_cache_source(b"machine-cache".to_vec()).await;

    ensure_sandbox_machine_running_with_observer(temp.path(), None)
        .await
        .expect("already-running unreachable machine should recover");

    let log = std::fs::read_to_string(&log_path).expect("read invocation log");
    assert!(log.contains("info"));
    assert!(log.contains("machine start "));
    assert!(log.contains("machine inspect "));
    assert!(log.contains("machine rm -f "));
    assert!(log.contains("machine init "));
    assert!(!log.contains("machine stop "));
    machine_cache_server.abort();
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tokio::test]
async fn ensure_sandbox_machine_running_fails_fast_on_unknown_start_error() {
    use std::os::unix::fs::PermissionsExt;

    let _serial = env_var_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let log_path = temp.path().join("sandbox-cli-invocations.log");
    let sandbox_cli_path = temp.path().join("sandbox-cli.sh");
    std::fs::write(
            &sandbox_cli_path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{}\"\nif [ \"$1\" = \"info\" ]; then\n  echo 'sandbox runtime unreachable' >&2\n  exit 125\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"start\" ]; then\n  echo 'error: unknown vm provider configuration' >&2\n  exit 125\nfi\nexit 0\n",
                log_path.display()
            ),
        )
        .expect("write sandbox CLI shim");
    std::fs::set_permissions(&sandbox_cli_path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod sandbox CLI shim");
    let _guard = EnvGuard::set(
        "CTX_HARNESS_SANDBOX_CLI_PATH",
        &sandbox_cli_path.to_string_lossy(),
    );

    let err = ensure_sandbox_machine_running_with_observer(temp.path(), None)
        .await
        .expect_err("unknown start error should fail");
    let message = format!("{err:#}");
    assert!(message.contains("unknown vm provider configuration"));

    let log = std::fs::read_to_string(&log_path).expect("read invocation log");
    assert!(log.contains("machine start "));
    assert!(!log.contains("machine stop "));
    assert!(!log.contains("machine rm -f "));
}
