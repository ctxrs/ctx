use ctx_core::ids::WorkspaceId;

use crate::daemon::DaemonState;
use ctx_provider_install::install_state::InstallTarget;
use ctx_settings_model::ExecutionSettings;
use ctx_settings_service::{install_target_for_settings, EffectiveExecutionSettingsError};

pub async fn effective_execution_settings_classified(
    state: &DaemonState,
    workspace_id: WorkspaceId,
) -> Result<ExecutionSettings, EffectiveExecutionSettingsError> {
    let store = state
        .store_for_workspace(workspace_id)
        .await
        .map_err(EffectiveExecutionSettingsError::Internal)?;
    ctx_settings_service::effective_execution_settings_classified(state.global_store(), &store)
        .await
}

/// Compute effective execution settings for a workspace, combining daemon defaults with any
/// workspace runtime override.
pub async fn effective_execution_settings(
    state: &DaemonState,
    workspace_id: WorkspaceId,
) -> anyhow::Result<ExecutionSettings> {
    effective_execution_settings_classified(state, workspace_id)
        .await
        .map_err(EffectiveExecutionSettingsError::into_inner)
}

pub async fn effective_install_target(
    state: &DaemonState,
    workspace_id: WorkspaceId,
) -> anyhow::Result<InstallTarget> {
    let effective = effective_execution_settings(state, workspace_id).await?;
    Ok(install_target_for_settings(&effective))
}

pub async fn effective_execution_settings_for_environment(
    state: &DaemonState,
    workspace_id: WorkspaceId,
    execution_environment: ctx_core::models::ExecutionEnvironment,
) -> anyhow::Result<ExecutionSettings> {
    let store = state.store_for_workspace(workspace_id).await?;
    effective_execution_settings_for_environment_parts(
        state.global_store(),
        &store,
        execution_environment,
    )
    .await
}

pub async fn effective_execution_settings_for_environment_parts(
    global_store: &ctx_store::Store,
    workspace_store: &ctx_store::Store,
    execution_environment: ctx_core::models::ExecutionEnvironment,
) -> anyhow::Result<ExecutionSettings> {
    ctx_settings_service::effective_execution_settings_for_environment(
        global_store,
        workspace_store,
        execution_environment,
    )
    .await
}

pub async fn effective_install_target_for_environment(
    state: &DaemonState,
    workspace_id: WorkspaceId,
    execution_environment: ctx_core::models::ExecutionEnvironment,
) -> anyhow::Result<InstallTarget> {
    let store = state.store_for_workspace(workspace_id).await?;
    ctx_settings_service::effective_install_target_for_environment(
        state.global_store(),
        &store,
        execution_environment,
    )
    .await
}

#[cfg(test)]
mod execution_effective_test;
