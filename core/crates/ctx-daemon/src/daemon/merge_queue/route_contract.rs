use std::path::{Path, PathBuf};

use ctx_core::ids::{MergeQueueEntryId, WorkspaceId};
use ctx_core::models::{MergeQueueRun, Workspace};
use ctx_route_contracts::downloads::TextRouteDownload;
use ctx_route_contracts::merge_queue::{
    ListMergeQueueEntriesRouteRequest, MergeQueueEntryRouteError, MergeQueueEntryRouteParams,
    MergeQueueEntryRouteResponse, MergeQueueLogDownloadRouteError,
};

use crate::daemon::route_files::{read_text_route_file, RouteFileDownloadError};
use crate::daemon::{MergeQueueApiHandle, WorkspaceStoreAccessError};

impl MergeQueueApiHandle {
    pub async fn list_merge_queue_entry_responses_for_route(
        &self,
        req: ListMergeQueueEntriesRouteRequest,
    ) -> Result<Vec<MergeQueueEntryRouteResponse>, MergeQueueEntryRouteError> {
        let workspace_id = req.parse_workspace_id()?;
        let store = self
            .existing_workspace_store(workspace_id)
            .await
            .map_err(list_store_error)?;
        let entries = store
            .list_merge_queue_entries(workspace_id, req.limit())
            .await
            .map_err(|error| MergeQueueEntryRouteError::internal(error.to_string()))?;
        Ok(entries.into_iter().map(Into::into).collect())
    }

    pub async fn cancel_merge_queue_entry_for_route(
        &self,
        params: MergeQueueEntryRouteParams,
    ) -> Result<MergeQueueEntryRouteResponse, MergeQueueEntryRouteError> {
        let (workspace_id, entry_id) = params.parse()?;
        self.cancel_merge_queue_entry(workspace_id, entry_id)
            .await
            .map(Into::into)
            .map_err(|error| MergeQueueEntryRouteError::bad_request(error.to_string()))
    }

    pub async fn retry_merge_queue_entry_for_route(
        &self,
        params: MergeQueueEntryRouteParams,
    ) -> Result<MergeQueueEntryRouteResponse, MergeQueueEntryRouteError> {
        let (workspace_id, entry_id) = params.parse()?;
        self.retry_merge_queue_entry(workspace_id, entry_id)
            .await
            .map(Into::into)
            .map_err(|error| MergeQueueEntryRouteError::bad_request(error.to_string()))
    }

    pub async fn download_merge_queue_entry_logs_for_route_params(
        &self,
        params: MergeQueueEntryRouteParams,
    ) -> Result<TextRouteDownload, MergeQueueLogDownloadRouteError> {
        let (workspace_id, entry_id) = params.parse_for_log_download()?;
        self.download_merge_queue_entry_logs_for_route(workspace_id, entry_id)
            .await
            .map_err(merge_queue_log_download_route_file_error)
    }

    pub async fn latest_merge_queue_run_for_route(
        &self,
        workspace_id: WorkspaceId,
        entry_id: MergeQueueEntryId,
    ) -> Result<Option<(Workspace, MergeQueueRun)>, WorkspaceStoreAccessError> {
        let store = self.existing_workspace_store(workspace_id).await?;
        let Some(workspace) = store
            .get_workspace(workspace_id)
            .await
            .map_err(WorkspaceStoreAccessError::Unavailable)?
        else {
            return Ok(None);
        };
        let run = store
            .get_latest_merge_queue_run(entry_id)
            .await
            .map_err(WorkspaceStoreAccessError::Unavailable)?;
        Ok(run.map(|run| (workspace, run)))
    }

    pub async fn download_merge_queue_entry_logs_for_route(
        &self,
        workspace_id: WorkspaceId,
        entry_id: MergeQueueEntryId,
    ) -> Result<TextRouteDownload, RouteFileDownloadError> {
        self.get_workspace_merge_queue_entry(workspace_id, entry_id)
            .await
            .map_err(|_| RouteFileDownloadError::NotFound)?;
        let (workspace, run) = self
            .latest_merge_queue_run_for_route(workspace_id, entry_id)
            .await
            .map_err(|_| RouteFileDownloadError::Internal)?
            .ok_or(RouteFileDownloadError::NotFound)?;
        let Some(path) = run.log_path.as_deref() else {
            return Err(RouteFileDownloadError::NotFound);
        };
        if path.trim().is_empty() {
            return Err(RouteFileDownloadError::NotFound);
        }
        let log_root = PathBuf::from(&workspace.root_path)
            .join(".ctx")
            .join("merge-queue")
            .join("logs");
        read_text_route_file(
            Path::new(path),
            &log_root,
            format!("merge-queue-{}.log", entry_id.0),
        )
        .await
    }
}

fn merge_queue_log_download_route_file_error(
    error: RouteFileDownloadError,
) -> MergeQueueLogDownloadRouteError {
    match error {
        RouteFileDownloadError::NotFound => MergeQueueLogDownloadRouteError::not_found(),
        RouteFileDownloadError::Internal => MergeQueueLogDownloadRouteError::internal(),
    }
}

fn list_store_error(error: WorkspaceStoreAccessError) -> MergeQueueEntryRouteError {
    match error {
        WorkspaceStoreAccessError::NotFound => {
            MergeQueueEntryRouteError::internal("workspace not found")
        }
        WorkspaceStoreAccessError::Unavailable(error) => {
            MergeQueueEntryRouteError::internal(error.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_route_contracts::merge_queue::MergeQueueLogDownloadRouteErrorKind;

    #[test]
    fn log_download_route_file_error_classification_is_transport_safe() {
        assert_eq!(
            merge_queue_log_download_route_file_error(RouteFileDownloadError::NotFound).kind(),
            MergeQueueLogDownloadRouteErrorKind::NotFound
        );
        assert_eq!(
            merge_queue_log_download_route_file_error(RouteFileDownloadError::Internal).kind(),
            MergeQueueLogDownloadRouteErrorKind::Internal
        );
    }
}
