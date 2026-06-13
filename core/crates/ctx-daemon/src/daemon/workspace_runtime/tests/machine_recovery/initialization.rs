use super::*;

#[tokio::test]
async fn initialize_sandbox_machine_uses_init_then_start_without_now() {
    use std::os::unix::fs::PermissionsExt;

    let _serial = env_var_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let log_path = temp.path().join("sandbox-cli-invocations.log");
    let sandbox_cli_path = temp.path().join("sandbox-cli.sh");
    std::fs::write(
        &sandbox_cli_path,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{}\"\nexit 0\n",
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
    let (_machine_cache_guard, machine_cache_server) =
        install_test_managed_machine_cache_source(b"machine-cache".to_vec()).await;

    let mut last_err = String::new();
    initialize_sandbox_machine(temp.path(), "ctx-test-machine", None, None, &mut last_err)
        .await
        .expect("initialize machine");

    let log = std::fs::read_to_string(&log_path).expect("read invocation log");
    assert!(log.contains("machine init ctx-test-machine"));
    assert!(log.contains("machine start ctx-test-machine"));
    assert!(!log.contains("--now"));
    assert!(last_err.is_empty());
    machine_cache_server.abort();
}

#[tokio::test]
async fn initialize_sandbox_machine_terminates_stuck_init_when_machine_is_present() {
    use std::os::unix::fs::PermissionsExt;

    let _serial = env_var_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let log_path = temp.path().join("sandbox-cli-invocations.log");
    let sandbox_cli_path = temp.path().join("sandbox-cli.sh");
    std::fs::write(
            &sandbox_cli_path,
            format!(
                "#!/bin/sh\nLOG=\"{}\"\nprintf '%s\\n' \"$*\" >> \"$LOG\"\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"init\" ]; then\n  exec sleep 30\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"inspect\" ]; then\n  printf '[]\\n'\n  exit 0\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"start\" ]; then\n  exit 0\nfi\nexit 0\n",
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
    let mut last_err = String::new();
    // Keep the outer test timeout comfortably above a single inspect timeout so the test
    // validates the kill-and-continue recovery path instead of host scheduling variance.
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        initialize_sandbox_machine_with_image(
            temp.path(),
            "ctx-test-machine",
            None,
            None,
            None,
            &mut last_err,
        ),
    )
    .await;
    let log = std::fs::read_to_string(&log_path).unwrap_or_default();
    let init_result = result
        .unwrap_or_else(|_| panic!("initialize_sandbox_machine timed out; invocation log:\n{log}"));
    init_result.expect("initialize machine");

    assert!(log.contains("machine init ctx-test-machine"));
    assert!(log.contains("machine inspect ctx-test-machine"));
    assert!(log.contains("machine start ctx-test-machine"));
    assert!(!log.contains("--now"));
}
