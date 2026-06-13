use anyhow::{anyhow, Result};
use async_trait::async_trait;
use std::path::{Path, PathBuf};

use ctx_core::ids::WorkspaceId;
use ctx_core::models::{SandboxBinding, Workspace, Worktree};
pub use ctx_sandbox_contract::{live_workspace_root_for_mode, live_worktree_root_for_mode};
use ctx_sandbox_contract::{
    map_host_or_live_path_to_live_roots, sandbox_execution_settings_from_binding,
    ContainerMountMode, ExecutionMode, ExecutionSettings, UbuntuSandboxSubstrate,
};
use ctx_store::Store;

#[derive(Debug, Clone)]
pub struct WorktreeDataPlane {
    pub binding: Option<SandboxBinding>,
    pub workspace: Workspace,
    pub execution_mode: ExecutionMode,
    pub live_workspace_root: PathBuf,
    pub live_worktree_root: PathBuf,
}

#[async_trait]
pub trait WorktreeDataPlaneHost: Send + Sync {
    async fn get_workspace(state: &Self, workspace_id: WorkspaceId) -> Result<Option<Workspace>>;
    async fn workspace_store(state: &Self, workspace_id: WorkspaceId) -> Result<Store>;
}

pub fn map_host_or_live_path_to_live_path(
    data_plane: &WorktreeDataPlane,
    host_workspace_root: &Path,
    host_worktree_root: Option<&Path>,
    requested: &Path,
) -> Option<PathBuf> {
    map_host_or_live_path_to_live_roots(
        &data_plane.live_workspace_root,
        &data_plane.live_worktree_root,
        host_workspace_root,
        host_worktree_root,
        requested,
    )
}

pub fn resolved_worktree_data_plane(
    workspace: &Workspace,
    worktree: &Worktree,
    binding: Option<SandboxBinding>,
) -> Result<WorktreeDataPlane> {
    if let Some(binding) = binding.as_ref() {
        ensure_supported_sandbox_instance_mapping(binding)?;
    }
    let execution_mode = if binding.is_some() {
        ExecutionMode::Sandbox
    } else {
        ExecutionMode::Host
    };
    let live_workspace_root = binding
        .as_ref()
        .map(|binding| PathBuf::from(&binding.live_workspace_root))
        .unwrap_or_else(|| PathBuf::from(&workspace.root_path));
    let live_worktree_root = binding
        .as_ref()
        .map(|binding| PathBuf::from(&binding.live_worktree_root))
        .unwrap_or_else(|| PathBuf::from(&worktree.root_path));
    Ok(WorktreeDataPlane {
        binding,
        workspace: workspace.clone(),
        execution_mode,
        live_workspace_root,
        live_worktree_root,
    })
}

pub async fn resolve_worktree_data_plane_with_host<H: WorktreeDataPlaneHost>(
    state: &H,
    worktree: &Worktree,
) -> Result<WorktreeDataPlane> {
    let workspace = H::get_workspace(state, worktree.workspace_id)
        .await?
        .ok_or_else(|| anyhow!("workspace not found for worktree"))?;
    let store = H::workspace_store(state, worktree.workspace_id).await?;
    let binding = store.get_sandbox_binding(worktree.id).await?;
    if binding.is_none() {
        let sessions = store.list_sessions_for_worktree(worktree.id).await?;
        if sessions.iter().any(|session| {
            matches!(
                session.execution_environment,
                ctx_core::models::ExecutionEnvironment::Sandbox
            )
        }) {
            return Err(anyhow!(
                "sandbox binding is missing for sandbox worktree {}",
                worktree.id.0
            ));
        }
    }
    resolved_worktree_data_plane(&workspace, worktree, binding)
}

pub fn workspace_data_plane(
    workspace: &Workspace,
    execution_mode: ExecutionMode,
) -> WorktreeDataPlane {
    let live_workspace_root = live_workspace_root_for_mode(workspace, execution_mode.clone());
    let live_worktree_root = live_workspace_root.clone();
    WorktreeDataPlane {
        binding: None,
        workspace: workspace.clone(),
        execution_mode,
        live_workspace_root,
        live_worktree_root,
    }
}

pub fn apply_data_plane_to_execution_settings(
    base: &ExecutionSettings,
    data_plane: &WorktreeDataPlane,
) -> Result<ExecutionSettings> {
    let mut settings = base.clone();
    settings.mode = data_plane.execution_mode.clone();
    if let Some(binding) = data_plane.binding.as_ref() {
        ensure_supported_sandbox_instance_mapping(binding)?;
        let substrate = UbuntuSandboxSubstrate::from_binding(binding)?;
        if binding.execution_settings_json.is_some() {
            return sandbox_execution_settings_from_binding(binding).map_err(|err| {
                anyhow!(
                    "sandbox binding {} had invalid execution settings snapshot: {err:#}",
                    binding.worktree_id.0
                )
            });
        }
        settings.mode = ExecutionMode::Sandbox;
        settings.container.runtime = substrate.runtime_kind();
        settings.container.mount_mode = ContainerMountMode::DiskIsolated;
    } else if matches!(data_plane.execution_mode, ExecutionMode::Sandbox) {
        settings.container.mount_mode = ContainerMountMode::DiskIsolated;
    }
    Ok(settings)
}

pub fn ensure_supported_sandbox_instance_mapping(binding: &SandboxBinding) -> Result<()> {
    if binding.uses_workspace_mapped_sandbox_instance() {
        return Ok(());
    }

    let expected = binding.expected_sandbox_instance_id();
    Err(anyhow!(
        "sandbox binding {} maps workspace {} to unsupported sandbox_instance_id {}; expected {}",
        binding.worktree_id.0,
        binding.workspace_id.0,
        binding.sandbox_instance_id.0,
        expected.0
    ))
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use ctx_core::ids::{SandboxInstanceId, WorkspaceId, WorktreeId};
    use ctx_core::models::{SandboxGuestIdentity, SandboxSubstrate};
    use ctx_sandbox_contract::{
        ContainerExecutionSettings, ContainerNetworkMode, ContainerRuntimeKind,
    };
    use uuid::Uuid;

    use super::*;

    #[test]
    fn binding_snapshot_overrides_mutated_workspace_defaults() {
        let binding_workspace_id = WorkspaceId(Uuid::new_v4());
        let snapshot = ExecutionSettings {
            mode: ExecutionMode::Sandbox,
            container: ContainerExecutionSettings {
                runtime: ContainerRuntimeKind::SharedVmContainer,
                network_mode: ContainerNetworkMode::Allowlist,
                allowlist: vec!["github.com".to_string()],
                image: Some("registry.example/sandbox:v1".to_string()),
                ..ContainerExecutionSettings::default()
            },
        };
        let data_plane = WorktreeDataPlane {
            binding: Some(SandboxBinding {
                worktree_id: WorktreeId(Uuid::new_v4()),
                workspace_id: binding_workspace_id,
                sandbox_instance_id: ctx_core::models::sandbox_instance_id_for_workspace(
                    binding_workspace_id,
                ),
                substrate: SandboxSubstrate::SharedVmContainer,
                guest_identity: SandboxGuestIdentity::linux_container_ubuntu(),
                profile: ctx_core::models::SandboxProfile::Standard,
                live_workspace_root: "/ctx/ws".to_string(),
                live_worktree_root: "/ctx/wt".to_string(),
                execution_settings_json: Some(
                    serde_json::to_string(&snapshot).expect("serialize snapshot"),
                ),
                container_name: Some("ctx-harness-test".to_string()),
                host_materialization_root: None,
                created_at: Utc::now(),
            }),
            workspace: Workspace {
                id: WorkspaceId(Uuid::new_v4()),
                name: "ws".to_string(),
                root_path: "/host/ws".to_string(),
                created_at: Utc::now(),
                vcs_kind: None,
            },
            execution_mode: ExecutionMode::Sandbox,
            live_workspace_root: PathBuf::from("/ctx/ws"),
            live_worktree_root: PathBuf::from("/ctx/wt"),
        };

        let current = ExecutionSettings {
            mode: ExecutionMode::Sandbox,
            container: ContainerExecutionSettings {
                runtime: ContainerRuntimeKind::NativeContainer,
                network_mode: ContainerNetworkMode::All,
                allowlist: vec![],
                image: Some("registry.example/sandbox:v2".to_string()),
                ..ContainerExecutionSettings::default()
            },
        };

        let applied =
            apply_data_plane_to_execution_settings(&current, &data_plane).expect("apply settings");
        assert_eq!(
            applied.container.runtime,
            ContainerRuntimeKind::SharedVmContainer
        );
        assert_eq!(
            applied.container.network_mode,
            ContainerNetworkMode::Allowlist
        );
        assert_eq!(applied.container.allowlist, vec!["github.com".to_string()]);
        assert_eq!(
            applied.container.image,
            Some("registry.example/sandbox:v1".to_string())
        );
    }

    #[test]
    fn workspace_data_plane_uses_workspace_root_for_host_mode() {
        let workspace = Workspace {
            id: WorkspaceId(Uuid::new_v4()),
            name: "ws".to_string(),
            root_path: "/host/ws".to_string(),
            created_at: Utc::now(),
            vcs_kind: None,
        };

        let data_plane = workspace_data_plane(&workspace, ExecutionMode::Host);
        assert_eq!(data_plane.execution_mode, ExecutionMode::Host);
        assert_eq!(data_plane.live_workspace_root, PathBuf::from("/host/ws"));
        assert_eq!(data_plane.live_worktree_root, PathBuf::from("/host/ws"));
        assert!(data_plane.binding.is_none());
    }

    #[test]
    fn workspace_data_plane_uses_container_workspace_root_for_sandbox_mode() {
        let workspace = Workspace {
            id: WorkspaceId(Uuid::new_v4()),
            name: "ws".to_string(),
            root_path: "/host/ws".to_string(),
            created_at: Utc::now(),
            vcs_kind: None,
        };

        let data_plane = workspace_data_plane(&workspace, ExecutionMode::Sandbox);
        assert_eq!(data_plane.execution_mode, ExecutionMode::Sandbox);
        assert_eq!(data_plane.live_workspace_root, PathBuf::from("/ctx/ws"));
        assert_eq!(data_plane.live_worktree_root, PathBuf::from("/ctx/ws"));
        assert!(data_plane.binding.is_none());
    }

    #[test]
    fn synthetic_sandbox_data_plane_forces_disk_isolated_mount_mode() {
        let workspace = Workspace {
            id: WorkspaceId(Uuid::new_v4()),
            name: "ws".to_string(),
            root_path: "/host/ws".to_string(),
            created_at: Utc::now(),
            vcs_kind: None,
        };
        let data_plane = workspace_data_plane(&workspace, ExecutionMode::Sandbox);
        let base = ExecutionSettings {
            mode: ExecutionMode::Sandbox,
            container: ContainerExecutionSettings {
                mount_mode: ContainerMountMode::Legacy,
                ..ContainerExecutionSettings::default()
            },
        };

        let applied =
            apply_data_plane_to_execution_settings(&base, &data_plane).expect("apply settings");
        assert_eq!(applied.mode, ExecutionMode::Sandbox);
        assert_eq!(
            applied.container.mount_mode,
            ContainerMountMode::DiskIsolated
        );
    }

    #[test]
    fn binding_snapshot_with_unknown_schema_version_fails_closed() {
        let binding_workspace_id = WorkspaceId(Uuid::new_v4());
        let data_plane = WorktreeDataPlane {
            binding: Some(SandboxBinding {
                worktree_id: WorktreeId(Uuid::new_v4()),
                workspace_id: binding_workspace_id,
                sandbox_instance_id: ctx_core::models::sandbox_instance_id_for_workspace(
                    binding_workspace_id,
                ),
                substrate: SandboxSubstrate::SharedVmContainer,
                guest_identity: SandboxGuestIdentity::linux_container_ubuntu(),
                profile: ctx_core::models::SandboxProfile::Standard,
                live_workspace_root: "/ctx/ws".to_string(),
                live_worktree_root: "/ctx/wt".to_string(),
                execution_settings_json: Some(
                    serde_json::json!({
                        "schema_version": 99,
                        "execution_settings": {
                            "mode": "sandbox",
                            "container": {
                                "runtime": "shared_vm_container",
                                "mount_mode": "disk_isolated"
                            }
                        }
                    })
                    .to_string(),
                ),
                container_name: Some("ctx-harness-test".to_string()),
                host_materialization_root: None,
                created_at: Utc::now(),
            }),
            workspace: Workspace {
                id: WorkspaceId(Uuid::new_v4()),
                name: "ws".to_string(),
                root_path: "/host/ws".to_string(),
                created_at: Utc::now(),
                vcs_kind: None,
            },
            execution_mode: ExecutionMode::Sandbox,
            live_workspace_root: PathBuf::from("/ctx/ws"),
            live_worktree_root: PathBuf::from("/ctx/wt"),
        };

        let err =
            apply_data_plane_to_execution_settings(&ExecutionSettings::default(), &data_plane)
                .expect_err("unknown binding schema version should fail closed");

        assert!(format!("{err:#}")
            .contains("unsupported sandbox binding execution settings schema version 99"));
    }

    #[test]
    fn binding_snapshot_with_host_mode_fails_closed() {
        let binding_workspace_id = WorkspaceId(Uuid::new_v4());
        let data_plane = WorktreeDataPlane {
            binding: Some(SandboxBinding {
                worktree_id: WorktreeId(Uuid::new_v4()),
                workspace_id: binding_workspace_id,
                sandbox_instance_id: ctx_core::models::sandbox_instance_id_for_workspace(
                    binding_workspace_id,
                ),
                substrate: SandboxSubstrate::NativeContainer,
                guest_identity: SandboxGuestIdentity::linux_container_ubuntu(),
                profile: ctx_core::models::SandboxProfile::Standard,
                live_workspace_root: "/ctx/ws".to_string(),
                live_worktree_root: "/ctx/wt".to_string(),
                execution_settings_json: Some(
                    serde_json::json!({
                        "mode": "host",
                        "container": {
                            "runtime": "native_container",
                            "mount_mode": "disk_isolated",
                            "network_mode": "all",
                            "allowlist": [],
                            "image": null
                        }
                    })
                    .to_string(),
                ),
                container_name: Some("ctx-harness-test".to_string()),
                host_materialization_root: None,
                created_at: Utc::now(),
            }),
            workspace: Workspace {
                id: WorkspaceId(Uuid::new_v4()),
                name: "ws".to_string(),
                root_path: "/host/ws".to_string(),
                created_at: Utc::now(),
                vcs_kind: None,
            },
            execution_mode: ExecutionMode::Sandbox,
            live_workspace_root: PathBuf::from("/ctx/ws"),
            live_worktree_root: PathBuf::from("/ctx/wt"),
        };

        let err =
            apply_data_plane_to_execution_settings(&ExecutionSettings::default(), &data_plane)
                .expect_err("host-mode binding snapshot should fail closed");

        assert!(format!("{err:#}")
            .contains("sandbox binding execution settings snapshot must keep mode=sandbox"));
    }

    #[test]
    fn binding_snapshot_with_non_workspace_mapped_sandbox_instance_fails_closed() {
        let binding_workspace_id = WorkspaceId(Uuid::new_v4());
        let data_plane = WorktreeDataPlane {
            binding: Some(SandboxBinding {
                worktree_id: WorktreeId(Uuid::new_v4()),
                workspace_id: binding_workspace_id,
                sandbox_instance_id: SandboxInstanceId(Uuid::new_v4()),
                substrate: SandboxSubstrate::NativeContainer,
                guest_identity: SandboxGuestIdentity::linux_container_ubuntu(),
                profile: ctx_core::models::SandboxProfile::Standard,
                live_workspace_root: "/ctx/ws".to_string(),
                live_worktree_root: "/ctx/wt".to_string(),
                execution_settings_json: None,
                container_name: Some("ctx-harness-test".to_string()),
                host_materialization_root: None,
                created_at: Utc::now(),
            }),
            workspace: Workspace {
                id: WorkspaceId(Uuid::new_v4()),
                name: "ws".to_string(),
                root_path: "/host/ws".to_string(),
                created_at: Utc::now(),
                vcs_kind: None,
            },
            execution_mode: ExecutionMode::Sandbox,
            live_workspace_root: PathBuf::from("/ctx/ws"),
            live_worktree_root: PathBuf::from("/ctx/wt"),
        };

        let err =
            apply_data_plane_to_execution_settings(&ExecutionSettings::default(), &data_plane)
                .expect_err("non-workspace-mapped sandbox instance should fail closed");

        assert!(format!("{err:#}").contains("unsupported sandbox_instance_id"));
    }
}
