use super::*;

#[cfg(target_os = "macos")]
#[tokio::test]
async fn ensure_sandbox_machine_running_waits_for_readiness_on_recoverable_start_error() {
    use std::os::unix::fs::PermissionsExt;

    let _serial = env_var_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let log_path = temp.path().join("sandbox-cli-invocations.log");
    let info_count_path = temp.path().join("sandbox-cli-info-count");
    let sandbox_cli_path = temp.path().join("sandbox-cli.sh");
    std::fs::write(
        &sandbox_cli_path,
        format!(
            "#!/bin/sh\nLOG=\"{log}\"\nINFO_COUNT=\"{info_count}\"\nprintf '%s\\n' \"$*\" >> \"$LOG\"\nif [ \"$1\" = \"info\" ]; then\n  count=0\n  if [ -f \"$INFO_COUNT\" ]; then\n    count=$(cat \"$INFO_COUNT\")\n  fi\n  count=$((count + 1))\n  printf '%s' \"$count\" > \"$INFO_COUNT\"\n  if [ \"$count\" -ge 2 ]; then\n    printf '{{}}\\n'\n    exit 0\n  fi\n  echo 'sandbox runtime unreachable' >&2\n  exit 125\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"start\" ]; then\n  echo 'error: operation timed out while waiting for vm startup' >&2\n  exit 125\nfi\necho \"unexpected sandbox CLI invocation: $*\" >&2\nexit 1\n",
            log = log_path.display(),
            info_count = info_count_path.display(),
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
        .expect("recoverable start error should resolve once runtime becomes reachable");

    let log = std::fs::read_to_string(&log_path).expect("read invocation log");
    assert!(log.contains("info"));
    assert!(log.contains("machine start "));
    assert!(
        !log.contains("machine stop "),
        "recoverable start error should not force stop:\n{log}"
    );
    assert!(
        !log.contains("machine rm -f "),
        "recoverable start error should not recreate:\n{log}"
    );
    assert!(
        !log.contains("machine init "),
        "recoverable start error should not init:\n{log}"
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn ensure_sandbox_machine_running_missing_machine_recovery_uses_configured_memory() {
    use std::os::unix::fs::PermissionsExt;

    let _serial = env_var_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let machine_name = sandbox_machine_name(temp.path());
    let log_path = temp.path().join("sandbox-cli-invocations.log");
    let state_path = temp.path().join("sandbox-machine-ready");
    let init_state_path = temp.path().join("sandbox-machine-initialized");
    let sandbox_cli_path = temp.path().join("sandbox-cli.sh");
    save_test_execution_settings(
        temp.path(),
        ExecutionSettings {
            mode: ExecutionMode::Sandbox,
            container: ContainerExecutionSettings {
                machine: ctx_settings_model::ContainerMachineSettings {
                    memory_profile: ctx_settings_model::ContainerMachineMemoryProfile::Custom,
                    custom_memory_mb: Some(6144),
                    ..ctx_settings_model::ContainerMachineSettings::default()
                },
                runtime: ctx_settings_model::ContainerRuntimeKind::NativeContainer,
                ..ContainerExecutionSettings::default()
            },
        },
    )
    .await;

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
    let (_machine_cache_guard, machine_cache_server) =
        install_test_managed_machine_cache_source(b"machine-cache".to_vec()).await;

    ensure_sandbox_machine_running_with_observer(temp.path(), None)
        .await
        .expect("missing machine recovery should materialize machine with configured memory");

    let log = std::fs::read_to_string(&log_path).expect("read invocation log");
    assert!(log.contains(&format!("machine start {machine_name}")));
    assert!(log.contains(&format!("machine init {machine_name}")));
    assert!(
        log.contains("--memory 6144"),
        "missing-machine recovery should preserve configured memory override:\n{log}"
    );
    machine_cache_server.abort();
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn ensure_sandbox_machine_running_recreate_recovery_uses_configured_memory() {
    use std::os::unix::fs::PermissionsExt;

    let _serial = env_var_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let machine_name = sandbox_machine_name(temp.path());
    let log_path = temp.path().join("sandbox-cli-invocations.log");
    let state_path = temp.path().join("sandbox-machine-ready");
    let start_count_path = temp.path().join("sandbox-machine-start-count");
    let sandbox_cli_path = temp.path().join("sandbox-cli.sh");
    save_test_execution_settings(
        temp.path(),
        ExecutionSettings {
            mode: ExecutionMode::Sandbox,
            container: ContainerExecutionSettings {
                machine: ctx_settings_model::ContainerMachineSettings {
                    memory_profile: ctx_settings_model::ContainerMachineMemoryProfile::Custom,
                    custom_memory_mb: Some(7168),
                    ..ctx_settings_model::ContainerMachineSettings::default()
                },
                runtime: ctx_settings_model::ContainerRuntimeKind::NativeContainer,
                ..ContainerExecutionSettings::default()
            },
        },
    )
    .await;

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
        .expect("recreate recovery should reinitialize machine with configured memory");

    let log = std::fs::read_to_string(&log_path).expect("read invocation log");
    assert!(log.contains(&format!("machine start {machine_name}")));
    assert!(log.contains(&format!("machine rm -f {machine_name}")));
    assert!(log.contains(&format!("machine init {machine_name}")));
    assert!(
        log.contains("--memory 7168"),
        "recreate recovery should preserve configured memory override:\n{log}"
    );
    machine_cache_server.abort();
}
