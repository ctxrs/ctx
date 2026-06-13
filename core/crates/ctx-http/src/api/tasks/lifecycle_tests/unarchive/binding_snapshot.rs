use super::super::*;

#[tokio::test]
async fn unarchive_task_recreates_managed_root_and_keeps_binding_snapshot_runtime() {
    let _serial = crate::test_support::sandbox_cli_env_test_lock()
        .lock()
        .await;
    let temp = tempfile::tempdir().expect("tempdir");
    let ManagedTaskFixture {
        repo_root,
        state,
        workspace,
        task,
        worktree,
        managed_root,
    } = create_managed_task_fixture(temp.path()).await;
    let state = state.daemon();

    let persisted_snapshot = ctx_settings_model::ExecutionSettings {
        mode: ctx_settings_model::ExecutionMode::Sandbox,
        container: ctx_settings_model::ContainerExecutionSettings {
            runtime: ctx_settings_model::ContainerRuntimeKind::NativeContainer,
            network_mode: ctx_settings_model::ContainerNetworkMode::Allowlist,
            allowlist: vec!["github.com".to_string()],
            image: Some("registry.example/sandbox:v1".to_string()),
            ..ctx_settings_model::ContainerExecutionSettings::default()
        },
    };
    state
        .seed_task_lifecycle_sandbox_binding_for_test(TaskLifecycleSandboxBindingSeed {
            worktree_id: worktree.id,
            workspace_id: workspace.id,
            substrate: SandboxSubstrate::NativeContainer,
            live_workspace_root: ctx_sandbox_contract::CTX_CONTAINER_WORKSPACE_ROOT.to_string(),
            live_worktree_root: ctx_sandbox_contract::container_worktree_root(worktree.id)
                .to_string_lossy()
                .to_string(),
            execution_settings_json: Some(
                serde_json::to_string(&persisted_snapshot).expect("serialize binding snapshot"),
            ),
            container_name: Some(ctx_workspace_container::workspace_container_name(
                workspace.id,
            )),
            host_materialization_root: None,
        })
        .await
        .expect("insert sandbox binding");

    let log_path = temp.path().join("sandbox-cli.log");
    let sandbox_cli_path = crate::test_support::write_running_container_sandbox_cli_shim(
        temp.path(),
        &log_path,
        &ctx_workspace_container::workspace_container_name(workspace.id),
    );
    let _sandbox_cli = EnvVarGuard::set(
        "CTX_HARNESS_SANDBOX_CLI_PATH",
        &sandbox_cli_path.to_string_lossy(),
    );
    let _sandbox_cli_available = EnvVarGuard::set("CTX_TEST_SANDBOX_CLI_AVAILABLE", "1");
    let workspace_id = workspace.id;
    let _storage_override =
        ctx_sandbox_materialization::set_test_preflight_storage_samples_override(Arc::new(
            move |data_root,
                  _mode,
                  container_id,
                  _estimated_copy_bytes,
                  destination_probe_root,
                  operation,
                  required_bytes| {
                assert_eq!(
                    container_id,
                    ctx_workspace_container::workspace_container_name(workspace_id)
                );
                assert_eq!(
                    operation,
                    ctx_storage_admission::StorageAdmissionOperation::DiskIsolatedWorktreeMaterialization
                );
                assert_eq!(
                    destination_probe_root,
                    std::path::Path::new(ctx_sandbox_contract::CTX_CONTAINER_WORKSPACE_ROOT)
                );
                let total_bytes =
                    required_bytes.saturating_add(2 * ctx_storage_admission::STORAGE_BYTES_GIB);
                Ok((
                    ctx_storage_admission::StorageAdmissionSample {
                        label: "CTX data root".to_string(),
                        path: data_root.to_string_lossy().to_string(),
                        mount_point: "/".to_string(),
                        free_bytes: required_bytes
                            .saturating_add(ctx_storage_admission::STORAGE_BYTES_GIB),
                        total_bytes,
                    },
                    ctx_storage_admission::StorageAdmissionSample {
                        label: "sandbox workspace volume".to_string(),
                        path: destination_probe_root.to_string_lossy().to_string(),
                        mount_point: ctx_sandbox_contract::CTX_CONTAINER_WORKSPACE_ROOT.to_string(),
                        free_bytes: required_bytes
                            .saturating_add(ctx_storage_admission::STORAGE_BYTES_GIB),
                        total_bytes,
                    },
                ))
            },
        ));

    let tasks = task_api_lifecycle_state(&state);
    let Json(_) = archive_task(tasks, Path(task.id.0.to_string()))
        .await
        .expect("archive task");

    save_test_execution_settings(
        &state,
        ctx_settings_model::ExecutionSettings {
            mode: ctx_settings_model::ExecutionMode::Sandbox,
            container: ctx_settings_model::ContainerExecutionSettings {
                runtime: ctx_settings_model::ContainerRuntimeKind::SharedVmContainer,
                network_mode: ctx_settings_model::ContainerNetworkMode::All,
                allowlist: Vec::new(),
                image: Some("registry.example/sandbox:v2".to_string()),
                ..ctx_settings_model::ContainerExecutionSettings::default()
            },
        },
    )
    .await;

    let tasks = task_api_lifecycle_state(&state);
    let Json(unarchived_task) = unarchive_task(tasks, Path(task.id.0.to_string()))
        .await
        .unwrap_or_else(|status| {
            let sandbox_log = std::fs::read_to_string(&log_path)
                .unwrap_or_else(|err| format!("failed to read sandbox CLI log: {err}"));
            panic!("unarchive task: {status}; sandbox log:\n{sandbox_log}");
        });

    assert!(
        unarchived_task.archived_at.is_none(),
        "task should no longer be archived after unarchive_task"
    );
    assert!(
        tokio::fs::metadata(&managed_root).await.is_ok(),
        "unarchive should recreate the canonical managed worktree root"
    );
    assert!(
        branch_exists(
            &repo_root,
            worktree.git_branch.as_deref().expect("branch name"),
        )
        .await
        .expect("check branch"),
        "unarchive should keep the existing managed worktree branch attached"
    );

    let current_effective = state
        .task_lifecycle_effective_execution_settings_for_test(workspace.id)
        .await
        .expect("load current effective settings");
    assert_eq!(
        current_effective.container.runtime,
        ctx_settings_model::ContainerRuntimeKind::SharedVmContainer,
        "workspace defaults should now point at the new runtime"
    );

    let binding = task_lifecycle_snapshot(&state, workspace.id, task.id, worktree.id)
        .await
        .sandbox_binding
        .expect("binding should remain present after unarchive");
    assert_eq!(binding.substrate, SandboxSubstrate::NativeContainer);
    assert_eq!(
        binding.sandbox_instance_id,
        ctx_core::models::sandbox_instance_id_for_workspace(workspace.id)
    );
    let parsed = ctx_sandbox_contract::sandbox_execution_settings_from_binding(&binding)
        .expect("parse rematerialized binding snapshot");
    assert_eq!(
        parsed.container.runtime,
        ctx_settings_model::ContainerRuntimeKind::NativeContainer,
        "rematerialized binding must preserve the original runtime snapshot"
    );
    assert_eq!(
        parsed.container.network_mode,
        ctx_settings_model::ContainerNetworkMode::Allowlist
    );
    assert_eq!(parsed.container.allowlist, vec!["github.com".to_string()]);
    assert_eq!(
        parsed.container.image,
        Some("registry.example/sandbox:v1".to_string())
    );
}
