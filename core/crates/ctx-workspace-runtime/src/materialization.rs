use anyhow::Result;
use chrono::{DateTime, Utc};
use ctx_core::ids::SandboxInstanceId;
use ctx_core::models::{
    sandbox_instance_id_for_workspace, SandboxBinding, SandboxProfile, Workspace, Worktree,
};
use ctx_execution_runtime::{ExecutionMode, ExecutionSettings};
use ctx_sandbox_contract::{ContainerMountMode, CTX_CONTAINER_WORKSPACE_ROOT};
use ctx_sandbox_materialization::ensure_worktree_from_host_copy;
use std::path::{Path, PathBuf};

use super::{HarnessRuntimeManager, SharedVmLifecycleOrchestrator, UbuntuSandboxSubstrate};

#[derive(Debug, Clone)]
pub struct SandboxWorktreeMaterialization {
    pub sandbox_instance_id: SandboxInstanceId,
    pub substrate: UbuntuSandboxSubstrate,
    pub live_worktree_root: PathBuf,
    pub host_materialization_root: Option<PathBuf>,
}

#[derive(Clone, Copy)]
pub struct MaterializeSandboxBindingParams<'a> {
    pub data_root: &'a Path,
    pub daemon_url: &'a str,
    pub harness: &'a HarnessRuntimeManager,
    pub workspace: &'a Workspace,
    pub worktree: &'a Worktree,
    pub canonical_root: &'a Path,
    pub effective: &'a ExecutionSettings,
    pub created_at: DateTime<Utc>,
}

pub async fn materialize_sandbox_worktree(
    data_root: &Path,
    daemon_url: &str,
    harness: &HarnessRuntimeManager,
    workspace: &Workspace,
    worktree: &Worktree,
    canonical_root: &Path,
    effective: &ExecutionSettings,
) -> Result<Option<SandboxWorktreeMaterialization>> {
    if !matches!(effective.mode, ExecutionMode::Sandbox)
        || !matches!(
            effective.container.mount_mode,
            ContainerMountMode::DiskIsolated
        )
    {
        return Ok(None);
    }

    harness
        .ensure_workspace_container_after_machine_ready_with_observer(
            workspace, effective, daemon_url, None,
        )
        .await?;

    let substrate = UbuntuSandboxSubstrate::from_runtime_kind(effective.container.runtime.clone());
    substrate.ensure_enabled()?;

    let branch_name = worktree
        .git_branch
        .as_deref()
        .or(worktree.vcs_ref.as_deref())
        .ok_or_else(|| anyhow::anyhow!("managed sandbox worktree is missing branch metadata"))?;

    let host_materialization_root = if substrate.is_shared_vm_backed() {
        Some(
            SharedVmLifecycleOrchestrator::new(data_root)
                .ensure_host_materialization_root(
                    sandbox_instance_id_for_workspace(workspace.id),
                    worktree.id,
                    canonical_root,
                    &worktree.base_commit_sha,
                    branch_name,
                    None,
                )
                .await?,
        )
    } else {
        None
    };
    let host_source_root = host_materialization_root
        .as_deref()
        .unwrap_or(canonical_root);
    let sandbox_mode = super::selected_sandbox_command_mode(data_root)?;
    let live_worktree_root = ensure_worktree_from_host_copy(
        data_root,
        &sandbox_mode,
        workspace.id,
        worktree.id,
        host_source_root,
        &worktree.base_commit_sha,
        branch_name,
    )
    .await?;

    Ok(Some(SandboxWorktreeMaterialization {
        sandbox_instance_id: sandbox_instance_id_for_workspace(workspace.id),
        substrate,
        live_worktree_root,
        host_materialization_root,
    }))
}

pub async fn materialize_sandbox_binding(
    params: MaterializeSandboxBindingParams<'_>,
) -> Result<Option<SandboxBinding>> {
    let MaterializeSandboxBindingParams {
        data_root,
        daemon_url,
        harness,
        workspace,
        worktree,
        canonical_root,
        effective,
        created_at,
    } = params;
    let Some(materialization) = materialize_sandbox_worktree(
        data_root,
        daemon_url,
        harness,
        workspace,
        worktree,
        canonical_root,
        effective,
    )
    .await?
    else {
        return Ok(None);
    };
    Ok(Some(sandbox_binding_from_materialization(
        workspace,
        worktree,
        effective,
        materialization,
        created_at,
    )?))
}

pub fn sandbox_binding_from_materialization(
    workspace: &Workspace,
    worktree: &Worktree,
    effective: &ExecutionSettings,
    materialization: SandboxWorktreeMaterialization,
    created_at: DateTime<Utc>,
) -> Result<SandboxBinding> {
    Ok(SandboxBinding {
        worktree_id: worktree.id,
        workspace_id: workspace.id,
        sandbox_instance_id: materialization.sandbox_instance_id,
        substrate: materialization.substrate.substrate,
        guest_identity: materialization.substrate.guest_identity,
        profile: SandboxProfile::Standard,
        live_workspace_root: CTX_CONTAINER_WORKSPACE_ROOT.to_string(),
        live_worktree_root: materialization
            .live_worktree_root
            .to_string_lossy()
            .to_string(),
        execution_settings_json: Some(serde_json::to_string(effective)?),
        container_name: Some(ctx_workspace_container::workspace_container_name(
            workspace.id,
        )),
        host_materialization_root: materialization
            .host_materialization_root
            .map(|path| path.to_string_lossy().to_string()),
        created_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContainerRuntimeKind;
    use ctx_core::ids::{WorkspaceId, WorktreeId};

    #[test]
    fn sandbox_binding_from_materialization_preserves_runtime_fields() {
        let workspace = Workspace {
            id: WorkspaceId::new(),
            root_path: "/host/workspace".to_string(),
            name: "workspace".to_string(),
            created_at: Utc::now(),
            vcs_kind: None,
        };
        let worktree = Worktree {
            id: WorktreeId::new(),
            workspace_id: workspace.id,
            root_path: "/host/worktree".to_string(),
            base_commit_sha: "abc123".to_string(),
            git_branch: Some("ctx/task".to_string()),
            vcs_kind: None,
            base_revision: Some("abc123".to_string()),
            vcs_ref: Some("ctx/task".to_string()),
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
        };
        let created_at = DateTime::parse_from_rfc3339("2026-05-12T00:00:00Z")
            .expect("timestamp")
            .with_timezone(&Utc);
        let materialization = SandboxWorktreeMaterialization {
            sandbox_instance_id: sandbox_instance_id_for_workspace(workspace.id),
            substrate: UbuntuSandboxSubstrate::from_runtime_kind(
                ContainerRuntimeKind::NativeContainer,
            ),
            live_worktree_root: PathBuf::from("/workspace/worktrees/abc"),
            host_materialization_root: Some(PathBuf::from("/host/materialized")),
        };

        let binding = sandbox_binding_from_materialization(
            &workspace,
            &worktree,
            &ExecutionSettings::default(),
            materialization,
            created_at,
        )
        .expect("binding");

        assert_eq!(binding.worktree_id, worktree.id);
        assert_eq!(binding.workspace_id, workspace.id);
        assert_eq!(binding.profile, SandboxProfile::Standard);
        assert_eq!(binding.live_workspace_root, CTX_CONTAINER_WORKSPACE_ROOT);
        assert_eq!(binding.live_worktree_root, "/workspace/worktrees/abc");
        assert_eq!(
            binding.host_materialization_root.as_deref(),
            Some("/host/materialized")
        );
        let expected_container_name =
            ctx_workspace_container::workspace_container_name(workspace.id);
        assert_eq!(
            binding.container_name.as_deref(),
            Some(expected_container_name.as_str())
        );
        assert_eq!(binding.created_at, created_at);
        assert!(binding.execution_settings_json.is_some());
    }
}
