use super::*;
use std::os::unix::fs::PermissionsExt;

#[tokio::test]
async fn maybe_reclaim_sandbox_machine_skips_running_container_terminals() {
    let _serial = env_var_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = runtime_manager(&temp).await;
    let machine_name = sandbox_machine_name(temp.path());
    let log_path = temp.path().join("sandbox-cli-invocations.log");
    let sandbox_cli_path = temp.path().join("sandbox-cli.sh");
    std::fs::write(
        &sandbox_cli_path,
        format!(
            "#!/bin/sh\nLOG=\"{log}\"\nprintf '%s\\n' \"$*\" >> \"$LOG\"\nif [ \"$1\" = \"exec\" ]; then\n  sleep 60\n  exit 0\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"inspect\" ]; then\n  printf '[]\\n'\n  exit 0\nfi\nif [ \"$1\" = \"machine\" ] && [ \"$2\" = \"stop\" ]; then\n  exit 0\nfi\necho \"unexpected sandbox CLI invocation: $*\" >&2\nexit 1\n",
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
    manager.set_last_activity_for_test(Instant::now() - Duration::from_secs(600));
    let stores = StoreManager::open(temp.path()).await.expect("open stores");
    let running_sessions = Arc::new(tokio::sync::Mutex::new(HashSet::new()));
    let terminals = Arc::new(ctx_transport_runtime::terminals::TerminalManager::default());
    let terminal = terminals
        .create(ctx_transport_runtime::terminals::TerminalCreateRequest {
            workspace_id: WorkspaceId::new(),
            task_id: Some(TaskId::new()),
            session_id: None,
            worktree_id: Some(WorktreeId::new()),
            cwd: temp.path().to_path_buf(),
            shell: "/bin/sh".to_string(),
            cols: None,
            rows: None,
            env: HashMap::new(),
            native_container: Some(
                ctx_transport_runtime::terminals::NativeContainerTerminalSpec {
                    cli_bin: sandbox_cli_path.clone(),
                    cli_env: HashMap::new(),
                    container_name: "ctx-harness-terminal".to_string(),
                    workdir: "/workspace".to_string(),
                    user: None,
                },
            ),
            shared_vm_container: None,
        })
        .await
        .expect("create terminal");
    let settings = ContainerExecutionSettings {
        machine: ctx_settings_model::ContainerMachineSettings {
            idle_shutdown_seconds: 60,
            ..ctx_settings_model::ContainerMachineSettings::default()
        },
        runtime: ctx_settings_model::ContainerRuntimeKind::NativeContainer,
        ..ContainerExecutionSettings::default()
    };
    let snapshot = SystemSnapshot {
        cpu_pct: 0.0,
        memory_total_bytes: 32 * 1024 * 1024 * 1024,
        memory_used_bytes: 8 * 1024 * 1024 * 1024,
        swap_total_bytes: 4 * 1024 * 1024 * 1024,
        swap_used_bytes: 0,
    };

    let stopped = manager
        .maybe_reclaim_sandbox_machine(
            &settings,
            &snapshot,
            None,
            &stores,
            &running_sessions,
            terminals.as_ref(),
        )
        .await
        .expect("reclaim check should succeed");

    assert!(!stopped);
    let log = std::fs::read_to_string(&log_path).unwrap_or_default();
    assert!(!log.contains(&format!("machine inspect {machine_name}")));
    assert!(!log.contains(&format!("machine stop {machine_name}")));

    terminal.kill().expect("kill terminal");
    tokio::time::sleep(Duration::from_millis(300)).await;
    manager.set_last_activity_for_test(Instant::now() - Duration::from_secs(600));

    let stopped = manager
        .maybe_reclaim_sandbox_machine(
            &settings,
            &snapshot,
            None,
            &stores,
            &running_sessions,
            terminals.as_ref(),
        )
        .await
        .expect("reclaim should resume once terminal exits");
    assert!(stopped);
}
