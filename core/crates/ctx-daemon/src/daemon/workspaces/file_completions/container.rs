use ctx_core::models::{ExecutionEnvironment, Worktree};
use ctx_settings_service::effective_execution_settings_for_environment;
use ctx_worktree_data_plane::apply_data_plane_to_execution_settings;
use ctx_worktree_data_plane::resolve_worktree_data_plane_with_host as resolve_worktree_data_plane;
use ctx_worktree_vcs_service::merge_and_sort_git_paths;

use crate::daemon::SessionFileCompletionsHandle;

use super::FileCompletionsError;

#[path = "container_git.rs"]
mod container_git;

pub(super) async fn list_container_worktree_files(
    handle: &SessionFileCompletionsHandle,
    worktree: &Worktree,
    execution_environment: ExecutionEnvironment,
) -> Result<Vec<String>, FileCompletionsError> {
    let workspace_id = worktree.workspace_id;
    let workspace = handle
        .global_store()
        .get_workspace(workspace_id)
        .await
        .map_err(|err| FileCompletionsError::internal(format!("loading workspace: {err}")))?
        .ok_or_else(|| FileCompletionsError::not_found("workspace not found"))?;
    let store = handle
        .store_for_workspace(workspace_id)
        .await
        .map_err(|err| FileCompletionsError::from_internal_error("loading workspace store", err))?;

    let settings = effective_execution_settings_for_environment(
        handle.global_store(),
        &store,
        execution_environment,
    )
    .await
    .map_err(|err| {
        FileCompletionsError::from_internal_error("resolving execution settings", err)
    })?;
    let data_plane = resolve_worktree_data_plane(handle, worktree)
        .await
        .map_err(|err| FileCompletionsError::internal(format!("resolving data plane: {err}")))?;
    let settings =
        apply_data_plane_to_execution_settings(&settings, &data_plane).map_err(|err| {
            FileCompletionsError::internal(format!(
                "applying data plane to execution settings: {err}"
            ))
        })?;
    handle
        .harness()
        .ensure_workspace_container_for_worktree(
            &workspace,
            worktree,
            &settings,
            handle.daemon_url(),
        )
        .await
        .map_err(|err| FileCompletionsError::internal(format!("ensuring container: {err}")))?;

    let workdir = data_plane.live_worktree_root.to_string_lossy().to_string();
    let tracked = container_git::container_git_ls_files(
        handle.data_root(),
        worktree,
        settings.container.runtime.clone(),
        &workdir,
        &["ls-files", "-z"],
    )
    .await?;
    let untracked = container_git::container_git_ls_files(
        handle.data_root(),
        worktree,
        settings.container.runtime,
        &workdir,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )
    .await?;

    Ok(merge_and_sort_git_paths(tracked, untracked))
}
