pub(super) use ctx_route_contracts::tasks::{
    ArchiveTaskRouteResponse, SessionRouteResponse, TaskRouteResponse,
    WorkspaceArchivedPageRouteResponse,
};

use super::super::ArchiveTaskOutcome;

impl From<ArchiveTaskOutcome> for ArchiveTaskRouteResponse {
    fn from(outcome: ArchiveTaskOutcome) -> Self {
        Self::from_task(outcome.task, outcome.cleanup_failed)
    }
}
