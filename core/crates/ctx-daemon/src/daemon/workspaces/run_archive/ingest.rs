use ctx_core::ids::{RunId, WorkspaceId};
use ctx_core::models::{RunArchiveIngestBatch, RunArchiveIngestCursor};

use crate::daemon::{RunArchiveHandle, WorkspaceStoreAccessError};

#[derive(Debug)]
pub enum RunArchiveIngestError {
    WorkspaceNotFound,
    AcknowledgementConflict(&'static str),
    Internal(anyhow::Error),
}

impl RunArchiveHandle {
    pub async fn build_run_archive_ingest_batch(
        &self,
        workspace_id: WorkspaceId,
        run_id: RunId,
        max_items: u32,
    ) -> Result<Option<RunArchiveIngestBatch>, RunArchiveIngestError> {
        let store = self
            .existing_workspace_store(workspace_id)
            .await
            .map_err(run_archive_workspace_store_error)?;
        ctx_run_archive_service::build_run_archive_ingest_batch(
            &store,
            workspace_id,
            run_id,
            max_items,
        )
        .await
        .map_err(RunArchiveIngestError::from)
    }

    pub async fn acknowledge_run_archive_ingest_batch(
        &self,
        workspace_id: WorkspaceId,
        run_id: RunId,
        max_items: u32,
        batch: RunArchiveIngestBatch,
    ) -> Result<RunArchiveIngestCursor, RunArchiveIngestError> {
        let store = self
            .existing_workspace_store(workspace_id)
            .await
            .map_err(run_archive_workspace_store_error)?;
        ctx_run_archive_service::acknowledge_run_archive_ingest_batch(
            &store, run_id, max_items, batch,
        )
        .await
        .map_err(RunArchiveIngestError::from)
    }
}

impl From<ctx_run_archive_service::RunArchiveIngestError> for RunArchiveIngestError {
    fn from(error: ctx_run_archive_service::RunArchiveIngestError) -> Self {
        match error {
            ctx_run_archive_service::RunArchiveIngestError::AcknowledgementConflict(message) => {
                Self::AcknowledgementConflict(message)
            }
            ctx_run_archive_service::RunArchiveIngestError::Internal(error) => {
                Self::Internal(error)
            }
        }
    }
}

fn run_archive_workspace_store_error(error: WorkspaceStoreAccessError) -> RunArchiveIngestError {
    match error {
        WorkspaceStoreAccessError::NotFound => RunArchiveIngestError::WorkspaceNotFound,
        WorkspaceStoreAccessError::Unavailable(error) => RunArchiveIngestError::Internal(error),
    }
}
