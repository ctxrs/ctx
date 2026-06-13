use crate::daemon::RunArchiveHandle;

use super::ingest::RunArchiveIngestError;

use ctx_route_contracts::run_archive::{
    AcknowledgeRunArchiveIngestBatchRouteRequest, AcknowledgeRunArchiveIngestBatchRouteResponse,
    BuildRunArchiveIngestBatchRouteRequest, BuildRunArchiveIngestBatchRouteResponse,
    RunArchiveRouteError,
};

impl RunArchiveHandle {
    pub async fn build_run_archive_ingest_batch_for_route(
        &self,
        req: BuildRunArchiveIngestBatchRouteRequest,
    ) -> Result<BuildRunArchiveIngestBatchRouteResponse, RunArchiveRouteError> {
        let (workspace_id, run_id, max_items) = req.parse()?;
        self.build_run_archive_ingest_batch(workspace_id, run_id, max_items)
            .await
            .map(BuildRunArchiveIngestBatchRouteResponse)
            .map_err(|error| run_archive_route_error("build", error))
    }

    pub async fn acknowledge_run_archive_ingest_batch_for_route(
        &self,
        req: AcknowledgeRunArchiveIngestBatchRouteRequest,
    ) -> Result<AcknowledgeRunArchiveIngestBatchRouteResponse, RunArchiveRouteError> {
        let (workspace_id, run_id, max_items, batch) = req.into_parts()?;
        self.acknowledge_run_archive_ingest_batch(workspace_id, run_id, max_items, batch)
            .await
            .map(AcknowledgeRunArchiveIngestBatchRouteResponse)
            .map_err(|error| run_archive_route_error("acknowledge", error))
    }
}

fn run_archive_route_error(
    action: &'static str,
    error: RunArchiveIngestError,
) -> RunArchiveRouteError {
    match error {
        RunArchiveIngestError::WorkspaceNotFound => {
            RunArchiveRouteError::not_found("workspace not found for run archive ingest")
        }
        RunArchiveIngestError::AcknowledgementConflict(message) => {
            RunArchiveRouteError::conflict(message)
        }
        RunArchiveIngestError::Internal(error) => RunArchiveRouteError::internal(format!(
            "failed to {action} run archive ingest batch: {error:#}"
        )),
    }
}
