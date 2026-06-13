use std::sync::Arc;

use tokio::sync::broadcast;

use ctx_core::ids::WorkspaceId;
use ctx_core::models::Workspace;
use ctx_execution_runtime::{
    route_contract::{
        LinuxSandboxActivationMode, LinuxSandboxRuntimeError, LinuxSandboxRuntimeOperation,
        LinuxSandboxRuntimePrepareResult, LinuxSandboxRuntimeStatus, StartExecutionLaunchError,
        StartExecutionLaunchRequest,
    },
    ExecutionLaunchSnapshot, ExecutionLaunchStreamEvent, ExecutionSetupCoordinator,
    ExecutionSetupJobKind, RuntimePrewarmScope,
};
use ctx_linux_sandbox_runtime::{
    linux_sandbox_runtime_status as runtime_linux_sandbox_runtime_status,
    prepare_linux_sandbox_runtime as runtime_prepare_linux_sandbox_runtime,
    stage_linux_sandbox_runtime_downloads as runtime_stage_linux_sandbox_runtime_downloads,
};
use ctx_observability::logs;
use ctx_settings_model::{ExecutionMode, ExecutionSettings};
use ctx_settings_service::EffectiveExecutionSettingsError;
use ctx_store::{Store, StoreManager};
use ctx_update_service::UpdateDrainCoordinator;

use crate::daemon::{maintenance, ExecutionLaunchHandle, LinuxSandboxRuntimeHandle};

fn classify_effective_execution_settings_error(
    error: EffectiveExecutionSettingsError,
) -> StartExecutionLaunchError {
    match error {
        EffectiveExecutionSettingsError::InvalidWorkspaceOverride(error) => {
            StartExecutionLaunchError::InvalidWorkspaceExecutionSettings {
                policy_denial: ctx_settings_service::is_execution_policy_denial(&error),
                message: logs::redact_sensitive(&error.to_string()),
            }
        }
        EffectiveExecutionSettingsError::Internal(error) => StartExecutionLaunchError::Internal {
            message: logs::redact_sensitive(&error.to_string()),
        },
    }
}

fn parse_workspace_id(raw_workspace_id: &str) -> Result<WorkspaceId, StartExecutionLaunchError> {
    uuid::Uuid::parse_str(raw_workspace_id.trim())
        .map(WorkspaceId)
        .map_err(|_| StartExecutionLaunchError::InvalidWorkspaceId)
}

fn linux_sandbox_runtime_error(
    operation: LinuxSandboxRuntimeOperation,
    error: anyhow::Error,
) -> LinuxSandboxRuntimeError {
    let operation_name = match operation {
        LinuxSandboxRuntimeOperation::Status => "linux_sandbox_runtime_status",
        LinuxSandboxRuntimeOperation::Stage => "linux_sandbox_runtime_stage",
        LinuxSandboxRuntimeOperation::Prepare => "linux_sandbox_runtime_prepare",
    };
    tracing::warn!(
        target: "linux_sandbox",
        error = %logs::redact_sensitive(&error.to_string()),
        "{operation_name} error"
    );
    LinuxSandboxRuntimeError::Runtime {
        operation,
        message: operation.user_message().to_string(),
    }
}

fn linux_sandbox_prepare_drain_error(
    error: maintenance::MaintenanceDrainError,
) -> LinuxSandboxRuntimeError {
    match error {
        maintenance::MaintenanceDrainError::AlreadyActive => {
            LinuxSandboxRuntimeError::PrepareAlreadyActive
        }
        maintenance::MaintenanceDrainError::ActivityUnavailable(error) => {
            tracing::warn!(
                target: "linux_sandbox",
                error = %logs::redact_sensitive(&error.to_string()),
                "linux_sandbox_runtime_prepare activity gate error"
            );
            LinuxSandboxRuntimeError::PrepareActivityUnavailable {
                message: LinuxSandboxRuntimeOperation::Prepare
                    .user_message()
                    .to_string(),
            }
        }
        maintenance::MaintenanceDrainError::SandboxWorkActive => {
            LinuxSandboxRuntimeError::PrepareSandboxWorkActive
        }
    }
}

pub(in crate::daemon) async fn launch_status_parts(
    setup: &ExecutionSetupCoordinator,
    job_id: &str,
) -> Option<ExecutionLaunchSnapshot> {
    setup.launch_status(job_id).await
}

pub(in crate::daemon) async fn subscribe_launch_parts(
    setup: &ExecutionSetupCoordinator,
    job_id: &str,
) -> Option<(
    ExecutionLaunchSnapshot,
    broadcast::Receiver<ExecutionLaunchStreamEvent>,
)> {
    setup.subscribe_launch(job_id).await
}

pub(in crate::daemon) async fn start_workspace_launch_parts(
    setup: &Arc<ExecutionSetupCoordinator>,
    workspace: Workspace,
    execution_settings: ExecutionSettings,
    daemon_url: String,
) -> ExecutionLaunchSnapshot {
    setup
        .start_workspace_launch(workspace, execution_settings, daemon_url)
        .await
}

pub(in crate::daemon) async fn start_runtime_prewarm_parts(
    setup: &Arc<ExecutionSetupCoordinator>,
    execution_settings: ExecutionSettings,
    prewarm_scope: RuntimePrewarmScope,
) -> ExecutionLaunchSnapshot {
    setup
        .start_runtime_prewarm(execution_settings, prewarm_scope)
        .await
}

pub(in crate::daemon) async fn start_execution_launch_for_request_parts(
    global_store: &Store,
    stores: &StoreManager,
    update_drain: &UpdateDrainCoordinator,
    setup: &Arc<ExecutionSetupCoordinator>,
    daemon_url: &str,
    request: StartExecutionLaunchRequest,
) -> Result<ExecutionLaunchSnapshot, StartExecutionLaunchError> {
    maintenance::reject_new_execution_during_maintenance_parts(update_drain)
        .await
        .map_err(|error| StartExecutionLaunchError::MaintenanceActive {
            message: logs::redact_sensitive(&error.to_string()),
        })?;

    let kind = request
        .kind
        .unwrap_or(ExecutionSetupJobKind::WorkspaceLaunch);
    match kind {
        ExecutionSetupJobKind::WorkspaceLaunch => {
            let raw_workspace_id = request
                .workspace_id
                .as_deref()
                .ok_or(StartExecutionLaunchError::MissingWorkspaceId)?;
            let workspace_id = parse_workspace_id(raw_workspace_id)?;
            let workspace = global_store
                .get_workspace(workspace_id)
                .await
                .map_err(|error| StartExecutionLaunchError::Internal {
                    message: logs::redact_sensitive(&error.to_string()),
                })?
                .ok_or(StartExecutionLaunchError::WorkspaceNotFound)?;
            let workspace_store = stores.workspace(workspace_id).await.map_err(|error| {
                StartExecutionLaunchError::Internal {
                    message: logs::redact_sensitive(&error.to_string()),
                }
            })?;
            let execution_settings = ctx_settings_service::effective_execution_settings_classified(
                global_store,
                &workspace_store,
            )
            .await
            .map_err(classify_effective_execution_settings_error)?;
            Ok(start_workspace_launch_parts(
                setup,
                workspace,
                execution_settings,
                daemon_url.to_string(),
            )
            .await)
        }
        ExecutionSetupJobKind::StartupPrewarm => {
            let settings = ctx_settings_service::load_settings(global_store)
                .await
                .map_err(|error| StartExecutionLaunchError::Internal {
                    message: logs::redact_sensitive(&error.to_string()),
                })?;
            let mut execution_settings = settings.execution.unwrap_or_default();
            execution_settings.mode = ExecutionMode::Sandbox;
            Ok(start_runtime_prewarm_parts(setup, execution_settings, request.prewarm_scope).await)
        }
    }
}

pub(in crate::daemon) async fn linux_sandbox_runtime_status_parts(
    data_root: &std::path::Path,
) -> Result<LinuxSandboxRuntimeStatus, LinuxSandboxRuntimeError> {
    runtime_linux_sandbox_runtime_status(data_root)
        .await
        .map_err(|error| linux_sandbox_runtime_error(LinuxSandboxRuntimeOperation::Status, error))
}

pub(in crate::daemon) async fn stage_linux_sandbox_runtime_parts(
    data_root: &std::path::Path,
) -> Result<LinuxSandboxRuntimeStatus, LinuxSandboxRuntimeError> {
    runtime_stage_linux_sandbox_runtime_downloads(data_root, None)
        .await
        .map_err(|error| linux_sandbox_runtime_error(LinuxSandboxRuntimeOperation::Stage, error))
}

pub(in crate::daemon) async fn prepare_linux_sandbox_runtime_parts(
    data_root: &std::path::Path,
    drain_permit: maintenance::MaintenanceDrainPermit,
    activation_mode: Option<LinuxSandboxActivationMode>,
    sudo_password: Option<&str>,
) -> Result<LinuxSandboxRuntimePrepareResult, LinuxSandboxRuntimeError> {
    let result = match runtime_prepare_linux_sandbox_runtime(
        data_root,
        activation_mode.unwrap_or(LinuxSandboxActivationMode::Local),
        sudo_password,
        None,
    )
    .await
    {
        Ok(result) => result,
        Err(error) => {
            let _ = drain_permit.release().await;
            return Err(linux_sandbox_runtime_error(
                LinuxSandboxRuntimeOperation::Prepare,
                error,
            ));
        }
    };
    let _ = drain_permit.release().await;
    Ok(result)
}

impl ExecutionLaunchHandle {
    pub async fn launch_status(&self, job_id: &str) -> Option<ExecutionLaunchSnapshot> {
        launch_status_parts(self.execution_setup().as_ref(), job_id).await
    }

    pub async fn subscribe_launch(
        &self,
        job_id: &str,
    ) -> Option<(
        ExecutionLaunchSnapshot,
        broadcast::Receiver<ExecutionLaunchStreamEvent>,
    )> {
        subscribe_launch_parts(self.execution_setup().as_ref(), job_id).await
    }

    pub async fn start_workspace_launch(
        &self,
        workspace: Workspace,
        execution_settings: ExecutionSettings,
    ) -> ExecutionLaunchSnapshot {
        start_workspace_launch_parts(
            self.execution_setup(),
            workspace,
            execution_settings,
            self.daemon_url().to_string(),
        )
        .await
    }

    pub async fn start_runtime_prewarm(
        &self,
        execution_settings: ExecutionSettings,
        prewarm_scope: RuntimePrewarmScope,
    ) -> ExecutionLaunchSnapshot {
        start_runtime_prewarm_parts(self.execution_setup(), execution_settings, prewarm_scope).await
    }

    pub async fn start_execution_launch_for_request(
        &self,
        request: StartExecutionLaunchRequest,
    ) -> Result<ExecutionLaunchSnapshot, StartExecutionLaunchError> {
        start_execution_launch_for_request_parts(
            self.global_store(),
            self.stores(),
            self.update_drain(),
            self.execution_setup(),
            self.daemon_url(),
            request,
        )
        .await
    }
}

impl LinuxSandboxRuntimeHandle {
    pub async fn linux_sandbox_runtime_status(
        &self,
    ) -> Result<LinuxSandboxRuntimeStatus, LinuxSandboxRuntimeError> {
        linux_sandbox_runtime_status_parts(self.data_root()).await
    }

    pub async fn stage_linux_sandbox_runtime(
        &self,
    ) -> Result<LinuxSandboxRuntimeStatus, LinuxSandboxRuntimeError> {
        stage_linux_sandbox_runtime_parts(self.data_root()).await
    }

    pub async fn prepare_linux_sandbox_runtime(
        &self,
        activation_mode: Option<LinuxSandboxActivationMode>,
        sudo_password: Option<&str>,
    ) -> Result<LinuxSandboxRuntimePrepareResult, LinuxSandboxRuntimeError> {
        let drain_permit = self
            .acquire_linux_sandbox_prepare_drain()
            .await
            .map_err(linux_sandbox_prepare_drain_error)?;
        prepare_linux_sandbox_runtime_parts(
            self.data_root(),
            drain_permit,
            activation_mode,
            sudo_password,
        )
        .await
    }
}
