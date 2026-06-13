use super::*;

use crate::daemon::DaemonState;
use ctx_core::models::{ExecutionEnvironment, SandboxGuestIdentity, SandboxProfile, VcsKind};
use ctx_settings_model::{ContainerMountMode, ContainerNetworkMode, ContainerRuntimeKind};
use ctx_store::StoreManager;
use ctx_workspace_config::{self, ExecutionConfigUpdate};
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::test]
async fn resolve_existing_worktree_execution_uses_binding_snapshot_after_workspace_defaults_change()
{
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_root = temp.path().join("repo");
    std::fs::create_dir_all(&repo_root).expect("create repo root");
    let state = Arc::new(DaemonState::new(
        temp.path().to_path_buf(),
        StoreManager::open(temp.path()).await.expect("open stores"),
        HashMap::new(),
        "http://127.0.0.1:4310".to_string(),
        None,
    ));
    let workspace = state
        .global_store()
        .create_workspace(
            "ws".to_string(),
            repo_root.to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .expect("create workspace");
    let store = state
        .store_for_workspace(workspace.id)
        .await
        .expect("workspace store");
    ctx_workspace_config::update_execution_config(
        &store,
        ExecutionConfigUpdate {
            environment: ExecutionEnvironment::Sandbox,
            network_mode: Some(ContainerNetworkMode::All),
            allowlist: Some(vec!["api.example.com".to_string()]),
            image: Some("registry.example/current:v2".to_string()),
        },
    )
    .await
    .expect("update workspace execution config");

    let managed_root = temp.path().join("managed-worktree");
    std::fs::create_dir_all(&managed_root).expect("create managed root");
    let worktree = store
        .insert_worktree(Worktree {
            id: WorktreeId(Uuid::new_v4()),
            workspace_id: workspace.id,
            root_path: managed_root.to_string_lossy().to_string(),
            base_commit_sha: "abc123".to_string(),
            git_branch: Some("ctx/test".to_string()),
            vcs_kind: Some(VcsKind::Git),
            base_revision: Some("abc123".to_string()),
            vcs_ref: Some("ctx/test".to_string()),
            created_at: Utc::now(),
            bootstrap_status: None,
            bootstrap_started_at: None,
            bootstrap_finished_at: None,
            bootstrap_exit_code: None,
            bootstrap_timeout_sec: None,
            bootstrap_error: None,
            bootstrap_log_path: None,
            bootstrap_log_truncated: None,
            bootstrap_command: None,
            bootstrap_script_path: None,
        })
        .await
        .expect("insert worktree");
    let binding_snapshot = serde_json::json!({
        "mode": "sandbox",
        "container": {
            "runtime": "shared_vm_container",
            "mount_mode": "disk_isolated",
            "network_mode": "allowlist",
            "allowlist": ["github.com"],
            "image": "registry.example/snapshot:v1"
        }
    });
    store
        .upsert_sandbox_binding(SandboxBinding {
            worktree_id: worktree.id,
            workspace_id: workspace.id,
            sandbox_instance_id: ctx_core::models::sandbox_instance_id_for_workspace(workspace.id),
            substrate: SandboxSubstrate::SharedVmContainer,
            guest_identity: SandboxGuestIdentity::linux_container_ubuntu(),
            profile: SandboxProfile::Standard,
            live_workspace_root: "/ctx/ws".to_string(),
            live_worktree_root: "/ctx/ws/worktrees/test".to_string(),
            execution_settings_json: Some(binding_snapshot.to_string()),
            container_name: Some("ctx-harness-test".to_string()),
            host_materialization_root: Some("/tmp/shadow".to_string()),
            created_at: Utc::now(),
        })
        .await
        .expect("insert sandbox binding");

    let resolved = resolve_existing_worktree_execution(&state, &store, &workspace, worktree.id)
        .await
        .expect("resolve worktree execution");

    assert_eq!(
        resolved.execution_environment(),
        ExecutionEnvironment::Sandbox
    );
    assert_eq!(
        resolved.effective.container.runtime,
        ContainerRuntimeKind::SharedVmContainer
    );
    assert_eq!(
        resolved.effective.container.mount_mode,
        ContainerMountMode::DiskIsolated
    );
    assert_eq!(
        resolved.effective.container.network_mode,
        ContainerNetworkMode::Allowlist
    );
    assert_eq!(
        resolved.effective.container.allowlist,
        vec!["github.com".to_string()]
    );
    assert_eq!(
        resolved.effective.container.image,
        Some("registry.example/snapshot:v1".to_string())
    );
}
