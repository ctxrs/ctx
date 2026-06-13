use super::*;
use ctx_avf_linux_runtime::SharedSubstrateLifecycleManager;
use ctx_workspace_container::{
    EnsureWorkspaceContainerRequest, WorkspaceContainer, WorkspaceContainerReadiness,
};

impl HarnessRuntimeManager {
    pub(super) async fn ensure_container(
        &self,
        workspace: &Workspace,
        worktree: Option<&Worktree>,
        settings: &ContainerExecutionSettings,
        daemon_host: &str,
        daemon_port: u16,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<WorkspaceContainer> {
        self.ensure_container_machine_ready(settings, observer)
            .await?;
        let substrate = UbuntuSandboxSubstrate::from_runtime_kind(settings.runtime.clone());
        substrate.ensure_enabled()?;
        if substrate.is_shared_vm_backed() {
            let sandbox_instance_id =
                ctx_core::models::sandbox_instance_id_for_workspace(workspace.id);
            let record = SharedSubstrateLifecycleManager::new(&self.data_root)
                .ensure_workspace_runtime_ready(sandbox_instance_id, settings, observer)
                .await?;
            self.emit_substrate_lifecycle_ops_event(
                &record,
                "container_prepare",
                Some(workspace.id),
            );
        }
        self.ensure_container_after_machine_ready(EnsureWorkspaceContainerRequest {
            workspace,
            worktree,
            settings,
            daemon_host,
            daemon_port,
            observer,
            readiness: WorkspaceContainerReadiness::MachineReady,
        })
        .await
    }

    pub(super) async fn ensure_container_after_machine_ready(
        &self,
        request: EnsureWorkspaceContainerRequest<'_>,
    ) -> Result<WorkspaceContainer> {
        let mode = selected_sandbox_command_mode(&self.data_root)?;
        self.workspace_containers
            .ensure_after_machine_ready(&mode, request)
            .await
    }
}
