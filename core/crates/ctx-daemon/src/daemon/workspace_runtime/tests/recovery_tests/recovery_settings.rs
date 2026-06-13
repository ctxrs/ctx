use super::*;

#[cfg(target_os = "macos")]
#[tokio::test]
async fn ensure_sandbox_machine_running_uses_info_fast_path_before_recovery_settings_load() {
    use std::os::unix::fs::PermissionsExt;

    let _serial = env_var_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let log_path = temp.path().join("sandbox-cli-invocations.log");
    let sandbox_cli_path = temp.path().join("sandbox-cli.sh");
    std::fs::create_dir_all(temp.path().join("db").join("db.sqlite"))
        .expect("create invalid settings store path");

    std::fs::write(
        &sandbox_cli_path,
        format!(
            "#!/bin/sh\nLOG=\"{log}\"\nprintf '%s\\n' \"$*\" >> \"$LOG\"\nfor arg in \"$@\"; do\n  if [ \"$arg\" = \"info\" ]; then\n    printf '{{}}\\n'\n    exit 0\n  fi\ndone\necho \"unexpected sandbox CLI invocation: $*\" >&2\nexit 1\n",
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

    ensure_sandbox_machine_running_with_observer(temp.path(), None)
        .await
        .expect("reachable runtime should not load recovery settings before the info fast path");

    let log = std::fs::read_to_string(&log_path).unwrap_or_default();
    assert!(
        log.contains("info"),
        "expected sandbox CLI info fast path to run:\n{log}"
    );
    assert!(
        !log.contains("machine start "),
        "reachable runtime should not attempt machine recovery:\n{log}"
    );
    assert!(
        !log.contains("machine init "),
        "reachable runtime should not attempt machine initialization:\n{log}"
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn ensure_sandbox_machine_running_falls_back_to_default_memory_when_recovery_settings_are_corrupt(
) {
    use std::os::unix::fs::PermissionsExt;

    let _serial = env_var_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let machine_name = sandbox_machine_name(temp.path());
    let log_path = temp.path().join("sandbox-cli-invocations.log");
    let state_path = temp.path().join("sandbox-machine-ready");
    let init_state_path = temp.path().join("sandbox-machine-initialized");
    let sandbox_cli_path = temp.path().join("sandbox-cli.sh");
    write_invalid_test_execution_settings(temp.path(), "{").await;

    std::fs::write(
        &sandbox_cli_path,
        format!(
            "#!/bin/sh\nLOG=\"{log}\"\nSTATE=\"{state}\"\nINIT_STATE=\"{init_state}\"\nprintf '%s\\n' \"$*\" >> \"$LOG\"\nif [ \"$1\" = \"info\" ]; then\n  if [ -f \"$STATE\" ]; then\n    printf '{{}}\\n'\n    exit 0\n  fi\n  echo 'sandbox runtime unreachable' >&2\n  exit 125\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"start\" ]; then\n  if [ -f \"$INIT_STATE\" ]; then\n    touch \"$STATE\"\n    exit 0\n  fi\n  echo 'error: machine does not exist' >&2\n  exit 125\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"init\" ]; then\n  touch \"$INIT_STATE\"\n  exit 0\nfi\necho \"unexpected sandbox CLI invocation: $*\" >&2\nexit 1\n",
            log = log_path.display(),
            state = state_path.display(),
            init_state = init_state_path.display(),
        ),
    )
    .expect("write sandbox CLI shim");
    std::fs::set_permissions(&sandbox_cli_path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod sandbox CLI shim");
    let _guard = EnvGuard::set(
        "CTX_HARNESS_SANDBOX_CLI_PATH",
        &sandbox_cli_path.to_string_lossy(),
    );
    let _host_memory = EnvGuard::set("CTX_TEST_HOST_MEMORY_MB", "49152");
    let (_machine_cache_guard, machine_cache_server) =
        install_test_managed_machine_cache_source(b"machine-cache".to_vec()).await;

    ensure_sandbox_machine_running_with_observer(temp.path(), None)
        .await
        .expect("corrupt recovery settings should fall back to default machine memory");

    let log = std::fs::read_to_string(&log_path).expect("read invocation log");
    assert!(log.contains(&format!("machine start {machine_name}")));
    assert!(log.contains(&format!("machine init {machine_name}")));
    assert!(
        log.contains("--memory 6144"),
        "corrupt recovery settings should fall back to the default machine memory:\n{log}"
    );
    machine_cache_server.abort();
}
