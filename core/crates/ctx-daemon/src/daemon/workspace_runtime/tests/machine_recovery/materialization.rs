use super::*;

#[tokio::test]
async fn ensure_sandbox_machine_materialized_recreates_machine_for_memory_profile_change_when_engine_is_down(
) {
    use std::os::unix::fs::PermissionsExt;

    let _serial = env_var_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = runtime_manager(&temp).await;
    let machine_name = sandbox_machine_name(temp.path());
    let log_path = temp.path().join("sandbox-cli-invocations.log");
    let sandbox_cli_path = temp.path().join("sandbox-cli.sh");
    std::fs::write(
        &sandbox_cli_path,
        format!(
            "#!/bin/sh\nLOG=\"{log}\"\nprintf '%s\\n' \"$*\" >> \"$LOG\"\nif [ \"$1\" = \"info\" ]; then\n  printf '{{}}\\n'\n  exit 0\nfi\nif [ \"$1\" = \"volume\" ] && [ \"$2\" = \"ls\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"ps\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"inspect\" ]; then\n  printf '[{{\"State\":\"stopped\",\"Resources\":{{\"Memory\":2048}}}}]\\n'\n  exit 0\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"stop\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"rm\" ] && [ \"$3\" = \"-f\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"init\" ]; then\n  exit 0\nfi\necho \"unexpected sandbox CLI invocation: $*\" >&2\nexit 1\n",
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
    let (_machine_cache_guard, machine_cache_server) =
        install_test_managed_machine_cache_source(b"machine-cache".to_vec()).await;
    let settings = ContainerExecutionSettings {
        mount_mode: ContainerMountMode::DiskIsolated,
        machine: ctx_settings_model::ContainerMachineSettings {
            memory_profile: ctx_settings_model::ContainerMachineMemoryProfile::Custom,
            custom_memory_mb: Some(12288),
            ..ctx_settings_model::ContainerMachineSettings::default()
        },
        runtime: ctx_settings_model::ContainerRuntimeKind::NativeContainer,
        ..ContainerExecutionSettings::default()
    };
    assert_eq!(container_machine_memory_mb(&settings), 12288);

    manager
        .ensure_sandbox_machine_materialized(&settings, None)
        .await
        .expect("machine should be recreated with desired memory");

    let log = std::fs::read_to_string(&log_path).expect("read invocation log");
    assert!(log.lines().any(|line| line == "info"));
    assert!(log.contains("volume ls --format {{.Name}}"));
    assert!(log.contains(&format!("machine inspect {machine_name}")));
    assert!(log.contains(&format!("machine stop {machine_name}")));
    assert!(log.contains(&format!("machine rm -f {machine_name}")));
    assert!(log.contains(&format!("machine init {machine_name}")));
    assert!(log.contains("--memory 12288"));
    machine_cache_server.abort();
}

#[tokio::test]
async fn ensure_sandbox_machine_materialized_defers_reconfiguration_when_machine_is_running_but_engine_unreachable(
) {
    use std::os::unix::fs::PermissionsExt;

    let _serial = env_var_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = runtime_manager(&temp).await;
    let machine_name = sandbox_machine_name(temp.path());
    let log_path = temp.path().join("sandbox-cli-invocations.log");
    let sandbox_cli_path = temp.path().join("sandbox-cli.sh");
    std::fs::write(
        &sandbox_cli_path,
        format!(
            "#!/bin/sh\nLOG=\"{log}\"\nprintf '%s\\n' \"$*\" >> \"$LOG\"\nif [ \"$1\" = \"info\" ]; then\n  echo 'unable to connect to \"gvproxy\" socket' >&2\n  exit 125\nfi\nif [ \"$1\" = \"ps\" ]; then\n  echo 'unable to connect to \"gvproxy\" socket' >&2\n  exit 125\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"inspect\" ]; then\n  printf '[{{\"State\":\"running\",\"Resources\":{{\"Memory\":2048}}}}]\\n'\n  exit 0\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"stop\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"rm\" ] && [ \"$3\" = \"-f\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"init\" ]; then\n  exit 0\nfi\necho \"unexpected sandbox CLI invocation: $*\" >&2\nexit 1\n",
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
    let _available = EnvGuard::set("CTX_TEST_SANDBOX_CLI_AVAILABLE", "1");
    let settings = ContainerExecutionSettings {
        machine: ctx_settings_model::ContainerMachineSettings {
            memory_profile: ctx_settings_model::ContainerMachineMemoryProfile::Balanced,
            ..ctx_settings_model::ContainerMachineSettings::default()
        },
        mount_mode: ContainerMountMode::Legacy,
        runtime: ctx_settings_model::ContainerRuntimeKind::NativeContainer,
        ..ContainerExecutionSettings::default()
    };

    manager
        .ensure_sandbox_machine_materialized(&settings, None)
        .await
        .expect("running-but-unreachable machine should defer destructive reconfiguration");

    let log = std::fs::read_to_string(&log_path).expect("read invocation log");
    assert!(log.lines().any(|line| line == "info"));
    assert!(log.contains(&format!("machine inspect {machine_name}")));
    assert!(!log.contains(&format!("machine stop {machine_name}")));
    assert!(!log.contains(&format!("machine rm -f {machine_name}")));
    assert!(!log.contains(&format!("machine init {machine_name}")));
}

#[tokio::test]
async fn ensure_sandbox_machine_materialized_defers_reconfiguration_when_machine_state_is_unknown_and_engine_unreachable(
) {
    use std::os::unix::fs::PermissionsExt;

    let _serial = env_var_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = runtime_manager(&temp).await;
    let machine_name = sandbox_machine_name(temp.path());
    let log_path = temp.path().join("sandbox-cli-invocations.log");
    let sandbox_cli_path = temp.path().join("sandbox-cli.sh");
    std::fs::write(
        &sandbox_cli_path,
        format!(
            "#!/bin/sh\nLOG=\"{log}\"\nprintf '%s\\n' \"$*\" >> \"$LOG\"\nif [ \"$1\" = \"info\" ]; then\n  echo 'unable to connect to \"gvproxy\" socket' >&2\n  exit 125\nfi\nif [ \"$1\" = \"ps\" ]; then\n  echo 'unable to connect to \"gvproxy\" socket' >&2\n  exit 125\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"inspect\" ]; then\n  printf '[]\\n'\n  exit 0\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"stop\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"rm\" ] && [ \"$3\" = \"-f\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"init\" ]; then\n  exit 0\nfi\necho \"unexpected sandbox CLI invocation: $*\" >&2\nexit 1\n",
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
    let _available = EnvGuard::set("CTX_TEST_SANDBOX_CLI_AVAILABLE", "1");
    let settings = ContainerExecutionSettings {
        machine: ctx_settings_model::ContainerMachineSettings {
            memory_profile: ctx_settings_model::ContainerMachineMemoryProfile::Balanced,
            ..ctx_settings_model::ContainerMachineSettings::default()
        },
        mount_mode: ContainerMountMode::Legacy,
        runtime: ctx_settings_model::ContainerRuntimeKind::NativeContainer,
        ..ContainerExecutionSettings::default()
    };

    manager
        .ensure_sandbox_machine_materialized(&settings, None)
        .await
        .expect("unknown runtime state should defer destructive reconfiguration");

    let log = std::fs::read_to_string(&log_path).expect("read invocation log");
    assert!(log.lines().any(|line| line == "info"));
    assert!(log.contains(&format!("machine inspect {machine_name}")));
    assert!(!log.contains(&format!("machine stop {machine_name}")));
    assert!(!log.contains(&format!("machine rm -f {machine_name}")));
    assert!(!log.contains(&format!("machine init {machine_name}")));
}
