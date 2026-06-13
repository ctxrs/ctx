use std::path::{Path, PathBuf};

use anyhow::Result;
use ctx_core::ids::{SandboxInstanceId, WorkspaceId, WorktreeId};
use ctx_sandbox_container_runtime::{
    container_image_present, prefetch_container_image_with_observer, resolve_container_image,
    SandboxCommandMode,
};

use crate::avf_linux_vm::{
    ensure_guest_worktree_from_host_copy as ensure_avf_linux_guest_worktree_from_host_copy,
    helper_path, shared_vm_is_launch_ready, stop_shared_vm,
    workspace_vm_state as avf_linux_workspace_vm_state, AvfLinuxSharedVmState,
};
use crate::{
    ensure_shared_vm_ready_with_observer, ensure_workspace_vm_ready_with_observer,
    prefetch_runtime_with_observer, runtime_state as avf_linux_runtime_state,
    ContainerExecutionSettings, HarnessSetupObserver,
};

pub struct SharedVmLifecycleOrchestrator<'a> {
    data_root: &'a Path,
}

impl<'a> SharedVmLifecycleOrchestrator<'a> {
    pub fn new(data_root: &'a Path) -> Self {
        Self { data_root }
    }

    pub async fn prefetch_runtime(
        &self,
        settings: &ContainerExecutionSettings,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<()> {
        prefetch_runtime_with_observer(self.data_root, settings, observer).await
    }

    pub async fn ensure_shared_runtime_ready(
        &self,
        settings: &ContainerExecutionSettings,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<AvfLinuxSharedVmState> {
        let state =
            ensure_shared_vm_ready_with_observer(self.data_root, settings, observer).await?;
        let image = resolve_container_image(settings.image.as_deref());
        prefetch_container_image_with_observer(
            self.data_root,
            &SandboxCommandMode::SharedVm {
                helper_path: helper_path()?,
            },
            &image,
            observer,
        )
        .await?;
        Ok(state)
    }

    pub async fn ensure_workspace_runtime_ready(
        &self,
        sandbox_instance_id: SandboxInstanceId,
        settings: &ContainerExecutionSettings,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<AvfLinuxSharedVmState> {
        ensure_workspace_vm_ready_with_observer(
            self.data_root,
            WorkspaceId(sandbox_instance_id.0),
            settings,
            observer,
        )
        .await
    }

    pub async fn ensure_host_materialization_root(
        &self,
        sandbox_instance_id: SandboxInstanceId,
        worktree_id: WorktreeId,
        host_workspace_root: &Path,
        base_commit_sha: &str,
        branch_name: &str,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<PathBuf> {
        let guest_worktree = ensure_avf_linux_guest_worktree_from_host_copy(
            self.data_root,
            WorkspaceId(sandbox_instance_id.0),
            worktree_id,
            host_workspace_root,
            base_commit_sha,
            branch_name,
            observer,
        )
        .await?;
        Ok(guest_worktree.host_shadow_root)
    }

    pub fn workspace_runtime_state(
        &self,
        sandbox_instance_id: SandboxInstanceId,
    ) -> Result<AvfLinuxSharedVmState> {
        avf_linux_workspace_vm_state(self.data_root, WorkspaceId(sandbox_instance_id.0))
    }

    pub fn save_or_stop_shared_runtime(&self) -> Result<AvfLinuxSharedVmState> {
        stop_shared_vm(self.data_root)
    }

    pub async fn launch_readiness_state(
        &self,
        settings: &ContainerExecutionSettings,
    ) -> Result<(bool, bool)> {
        let (helper_ready, runtime_ready) = avf_linux_runtime_state(self.data_root)?;
        if !helper_ready || !runtime_ready {
            return Ok((false, false));
        }
        let shared_vm_state =
            avf_linux_workspace_vm_state(self.data_root, WorkspaceId(uuid::Uuid::nil()))?;
        let substrate_ready = shared_vm_is_launch_ready(&shared_vm_state);
        let image_ready = if substrate_ready {
            container_image_present(
                self.data_root,
                &SandboxCommandMode::SharedVm {
                    helper_path: helper_path()?,
                },
                &resolve_container_image(settings.image.as_deref()),
            )
            .await?
        } else {
            false
        };
        Ok((substrate_ready, image_ready))
    }
}
