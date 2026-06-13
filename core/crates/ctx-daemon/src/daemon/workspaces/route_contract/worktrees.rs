use ctx_core::ids::WorktreeId;
use ctx_route_contracts::downloads::TextRouteDownload;
use ctx_route_contracts::workspaces::{WorktreeRouteParams, WorktreeRouteResponse};

use crate::daemon::{RouteFileDownloadError, WorkspaceWorktreeHandle};

use super::super::WorkspaceRouteError;
use super::common::route_file_download_error;

impl WorkspaceWorktreeHandle {
    pub async fn get_worktree_for_route_params(
        &self,
        params: WorktreeRouteParams,
    ) -> Result<WorktreeRouteResponse, WorkspaceRouteError> {
        let worktree_id = params.parse_worktree_id()?;
        self.get_worktree_for_route(worktree_id)
            .await?
            .ok_or_else(|| WorkspaceRouteError::not_found("worktree not found"))
    }

    pub async fn get_worktree_for_route(
        &self,
        worktree_id: WorktreeId,
    ) -> Result<Option<WorktreeRouteResponse>, WorkspaceRouteError> {
        get_worktree_with_live_root(self, worktree_id)
            .await
            .map(|worktree| worktree.map(Into::into))
            .map_err(WorkspaceRouteError::internal)
    }

    pub async fn download_worktree_bootstrap_logs_for_route_params(
        &self,
        params: WorktreeRouteParams,
    ) -> Result<TextRouteDownload, WorkspaceRouteError> {
        let worktree_id = params.parse_worktree_id()?;
        download_worktree_bootstrap_logs_for_route(self, worktree_id)
            .await
            .map_err(route_file_download_error)
    }
}

async fn get_worktree_with_live_root(
    handle: &WorkspaceWorktreeHandle,
    worktree_id: WorktreeId,
) -> anyhow::Result<Option<ctx_core::models::Worktree>> {
    handle.loaded_worktree_with_live_root(worktree_id).await
}

async fn get_worktree_bootstrap_log_path(
    handle: &WorkspaceWorktreeHandle,
    worktree_id: WorktreeId,
) -> anyhow::Result<Option<String>> {
    handle.worktree_bootstrap_log_path(worktree_id).await
}

async fn download_worktree_bootstrap_logs_for_route(
    handle: &WorkspaceWorktreeHandle,
    worktree_id: WorktreeId,
) -> Result<TextRouteDownload, RouteFileDownloadError> {
    let path = get_worktree_bootstrap_log_path(handle, worktree_id)
        .await
        .map_err(|_| RouteFileDownloadError::Internal)?
        .ok_or(RouteFileDownloadError::NotFound)?;
    if path.trim().is_empty() {
        return Err(RouteFileDownloadError::NotFound);
    }
    let log_root = ctx_observability::logs::logs_dir(handle.data_root()).join("worktree-bootstrap");
    crate::daemon::route_files::read_text_route_file(
        std::path::Path::new(&path),
        &log_root,
        format!("worktree-bootstrap-{}.log", worktree_id.0),
    )
    .await
}
