use super::*;

#[cfg(unix)]
#[tokio::test]
async fn prepare_starts_existing_workspace_container_when_not_cached() {
    use std::os::unix::fs::PermissionsExt;

    let _serial = env_var_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let log_path = temp.path().join("sandbox-cli-invocations.log");
    let sandbox_cli_path = temp.path().join("sandbox-cli.sh");
    let manager = runtime_manager(&temp).await;
    let workspace = sample_workspace(&temp);
    let worktree = sample_worktree(&temp, workspace.id);
    let container_name = workspace_container_name(workspace.id);
    let volume_name = format!("ctx-ws-{}", workspace.id.0);
    let settings = ExecutionSettings {
        mode: ExecutionMode::Sandbox,
        container: ContainerExecutionSettings {
            network_mode: ContainerNetworkMode::All,
            allowlist: Vec::new(),
            runtime: ctx_settings_model::ContainerRuntimeKind::NativeContainer,
            ..Default::default()
        },
    };

    std::fs::write(
        &sandbox_cli_path,
        format!(
            "#!/bin/sh\nLOG=\"{log}\"\nprintf '%s\\n' \"$*\" >> \"$LOG\"\nif [ \"$1\" = \"info\" ]; then\n  printf '{{}}\\n'\n  exit 0\nfi\nif [ \"$1\" = \"image\" ] && [ \"$2\" = \"inspect\" ]; then\n  echo 'unexpected image check' >&2\n  exit 125\nfi\nif [ \"$1\" = \"volume\" ] && [ \"$2\" = \"inspect\" ]; then\n  exit 1\nfi\nif [ \"$1\" = \"volume\" ] && [ \"$2\" = \"create\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"container\" ] && [ \"$2\" = \"inspect\" ] && [ \"$3\" = \"{container}\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"container\" ] && [ \"$2\" = \"inspect\" ] && [ \"$5\" = \"{container}\" ]; then\n  printf 'false\\n'\n  exit 0\nfi\nif [ \"$1\" = \"inspect\" ] && [ \"$2\" = \"{container}\" ]; then\n  printf '[{{\"Mounts\":[{{\"Type\":\"volume\",\"Name\":\"{volume}\",\"Destination\":\"{workspace_root}\"}}]}}]\\n'\n  exit 0\nfi\nif [ \"$1\" = \"start\" ] && [ \"$2\" = \"{container}\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"exec\" ]; then\n  exit 0\nfi\necho \"unexpected sandbox CLI invocation: $*\" >&2\nexit 1\n",
            log = log_path.display(),
            container = container_name,
            volume = volume_name,
            workspace_root = CTX_CONTAINER_WORKSPACE_ROOT,
        ),
    )
    .expect("write sandbox CLI shim");
    std::fs::set_permissions(&sandbox_cli_path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod sandbox CLI shim");
    let _guard = EnvGuard::set(
        "CTX_HARNESS_SANDBOX_CLI_PATH",
        &sandbox_cli_path.to_string_lossy(),
    );

    let plan = manager
        .prepare(&workspace, &worktree, &settings, "http://127.0.0.1:4399")
        .await
        .expect("existing stopped workspace container should be started");

    match plan.runtime {
        HarnessRuntimeKind::NativeContainer { name } => assert_eq!(name, container_name),
        HarnessRuntimeKind::Host => panic!("expected container runtime"),
        HarnessRuntimeKind::SharedVmContainer => panic!("expected sandbox container runtime"),
    }

    let log = std::fs::read_to_string(&log_path).expect("read sandbox CLI invocation log");
    assert!(
        log.contains(&format!("container inspect {container_name}")),
        "expected container existence check in log:\n{log}"
    );
    assert!(
        log.contains(&format!(
            "container inspect --format {{{{.State.Running}}}} {container_name}"
        )),
        "expected stopped-container inspect in log:\n{log}"
    );
    assert!(
        log.contains(&format!("start {container_name}")),
        "expected stopped container to be started:\n{log}"
    );
    assert!(
        !log.contains("image inspect"),
        "starting a stopped container should not front-load image checks:\n{log}"
    );
    assert!(
        !log.contains("run -d --name"),
        "starting a stopped container should not recreate the container:\n{log}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn prepare_adopts_existing_workspace_container_when_run_reports_name_collision() {
    use std::os::unix::fs::PermissionsExt;

    let _serial = env_var_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let log_path = temp.path().join("sandbox-cli-invocations.log");
    let exists_state_path = temp.path().join("container-exists-count");
    let sandbox_cli_path = temp.path().join("sandbox-cli.sh");
    let manager = runtime_manager(&temp).await;
    let workspace = sample_workspace(&temp);
    let worktree = sample_worktree(&temp, workspace.id);
    let container_name = workspace_container_name(workspace.id);
    let volume_name = format!("ctx-ws-{}", workspace.id.0);
    let settings = ExecutionSettings {
        mode: ExecutionMode::Sandbox,
        container: ContainerExecutionSettings {
            network_mode: ContainerNetworkMode::All,
            allowlist: Vec::new(),
            runtime: ctx_settings_model::ContainerRuntimeKind::NativeContainer,
            ..Default::default()
        },
    };

    std::fs::write(
        &sandbox_cli_path,
        format!(
            "#!/bin/sh\nLOG=\"{log}\"\nSTATE=\"{state}\"\nprintf '%s\\n' \"$*\" >> \"$LOG\"\nif [ \"$1\" = \"info\" ]; then\n  printf '{{}}\\n'\n  exit 0\nfi\nif [ \"$1\" = \"image\" ] && [ \"$2\" = \"inspect\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"volume\" ] && [ \"$2\" = \"inspect\" ]; then\n  exit 1\nfi\nif [ \"$1\" = \"volume\" ] && [ \"$2\" = \"create\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"container\" ] && [ \"$2\" = \"inspect\" ] && [ \"$3\" = \"{container}\" ]; then\n  count=0\n  if [ -f \"$STATE\" ]; then\n    count=$(cat \"$STATE\")\n  fi\n  count=$((count + 1))\n  printf '%s' \"$count\" > \"$STATE\"\n  if [ \"$count\" -eq 1 ]; then\n    exit 1\n  fi\n  exit 0\nfi\nif [ \"$1\" = \"container\" ] && [ \"$2\" = \"inspect\" ] && [ \"$5\" = \"{container}\" ]; then\n  printf 'true\\n'\n  exit 0\nfi\nif [ \"$1\" = \"run\" ]; then\n  printf 'time=\"2026-03-26T00:00:00Z\" level=fatal msg=\"name-store error\\\\nname \\\\\\\"{container}\\\\\\\" is already used by ID \\\\\\\"abc123\\\\\\\"\"\\n' >&2\n  exit 1\nfi\nif [ \"$1\" = \"inspect\" ] && [ \"$2\" = \"{container}\" ]; then\n  printf '[{{\"Mounts\":[{{\"Type\":\"volume\",\"Name\":\"{volume}\",\"Destination\":\"{workspace_root}\"}}]}}]\\n'\n  exit 0\nfi\nif [ \"$1\" = \"exec\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"start\" ] && [ \"$2\" = \"{container}\" ]; then\n  exit 0\nfi\necho \"unexpected sandbox CLI invocation: $*\" >&2\nexit 1\n",
            log = log_path.display(),
            state = exists_state_path.display(),
            container = container_name,
            volume = volume_name,
            workspace_root = CTX_CONTAINER_WORKSPACE_ROOT,
        ),
    )
    .expect("write sandbox CLI shim");
    std::fs::set_permissions(&sandbox_cli_path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod sandbox CLI shim");
    let _guard = EnvGuard::set(
        "CTX_HARNESS_SANDBOX_CLI_PATH",
        &sandbox_cli_path.to_string_lossy(),
    );

    let plan = manager
        .prepare(&workspace, &worktree, &settings, "http://127.0.0.1:4399")
        .await
        .expect("name-collision create should adopt existing container");

    match plan.runtime {
        HarnessRuntimeKind::NativeContainer { name } => assert_eq!(name, container_name),
        HarnessRuntimeKind::Host => panic!("expected container runtime"),
        HarnessRuntimeKind::SharedVmContainer => panic!("expected sandbox container runtime"),
    }

    let log = std::fs::read_to_string(&log_path).expect("read sandbox CLI invocation log");
    assert!(
        log.contains(&format!("run -d --name {container_name}")),
        "expected attempted container create in log:\n{log}"
    );
    assert_eq!(
        log.match_indices(&format!("container inspect {container_name}"))
            .count(),
        2,
        "expected initial miss plus adopt-exists recheck in log:\n{log}"
    );
    assert!(
        log.contains(&format!(
            "container inspect --format {{{{.State.Running}}}} {container_name}"
        )),
        "expected adopted-container running check in log:\n{log}"
    );
}
