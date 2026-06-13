use anyhow::{anyhow, Result};
use ctx_core::models::{ExecutionEnvironment, Session};
use ctx_settings_model::ExecutionSettings;

use crate::daemon::execution_effective;
use crate::daemon::scheduler::host::ProviderTurnLaunchHost;
use ctx_worktree_data_plane::apply_data_plane_to_execution_settings;
use ctx_worktree_data_plane::resolve_worktree_data_plane_with_host as resolve_worktree_data_plane;

pub(super) struct TurnExecutionPlan {
    pub(super) execution_settings: ExecutionSettings,
    pub(super) runtime_plan: ctx_harness_runtime::HarnessExecutionPlan,
}

pub(super) async fn prepare_turn_execution_plan(
    provider_launch: &ProviderTurnLaunchHost,
    store: &ctx_store::Store,
    session: &Session,
    execution_environment: ExecutionEnvironment,
) -> Result<TurnExecutionPlan> {
    let workspace = store
        .get_workspace(session.workspace_id)
        .await?
        .ok_or_else(|| anyhow!("workspace not found: {}", session.workspace_id.0))?;
    let worktree_for_runtime = store
        .get_worktree(session.worktree_id)
        .await?
        .ok_or_else(|| anyhow!("worktree not found: {}", session.worktree_id.0))?;
    let execution_settings =
        execution_effective::effective_execution_settings_for_environment_parts(
            provider_launch.global_store(),
            &provider_launch.store_for_workspace(workspace.id).await?,
            execution_environment,
        )
        .await?;
    let execution_settings =
        match resolve_worktree_data_plane(provider_launch, &worktree_for_runtime).await {
            Ok(data_plane) => {
                apply_data_plane_to_execution_settings(&execution_settings, &data_plane)?
            }
            Err(err) => return Err(err),
        };
    let runtime_plan = provider_launch
        .prepare_harness_runtime(&workspace, &worktree_for_runtime, &execution_settings)
        .await?;

    Ok(TurnExecutionPlan {
        execution_settings,
        runtime_plan,
    })
}
