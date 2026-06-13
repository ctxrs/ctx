use super::*;
use target::resolve_default_session_target;

#[path = "default_session_plan/target.rs"]
mod target;

pub(super) type DefaultSessionPlan = (ExecutionEnvironment, String, String, Option<String>);

async fn validate_workspace_root_is_repo(workspace: &Workspace) -> Result<(), TaskCreateError> {
    let workspace_root = StdPath::new(&workspace.root_path);
    ctx_repo_onboarding_service::validate_workspace_root_repo(workspace_root)
        .await
        .map_err(|error| TaskCreateError::BadRequest(error.to_string()))?;
    Ok(())
}

pub(super) async fn preflight_default_session_creation(
    handles: &TaskCreationHandles,
    store: &Store,
    workspace: &Workspace,
) -> Result<DefaultSessionPlan, TaskCreateError> {
    validate_workspace_root_is_repo(workspace).await?;
    let effective = handles
        .session_admission
        .effective_execution_settings(workspace.id)
        .await
        .map_err(TaskCreateError::internal)?;
    let execution_environment = execution_environment_from_settings(&effective);
    let (provider_id, model_id, reasoning_effort) =
        resolve_default_session_target(handles, store, workspace, execution_environment).await?;
    Ok((
        execution_environment,
        provider_id,
        model_id,
        reasoning_effort,
    ))
}
