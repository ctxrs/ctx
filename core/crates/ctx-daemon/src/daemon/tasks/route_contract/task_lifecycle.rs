use crate::daemon::TaskLifecycleHandle;

use super::common::{task_route_error_from_task_lifecycle, TaskRouteError, TaskRouteParams};
use super::responses::{ArchiveTaskRouteResponse, TaskRouteResponse};

impl TaskLifecycleHandle {
    pub async fn archive_task_for_route(
        &self,
        params: TaskRouteParams,
    ) -> Result<ArchiveTaskRouteResponse, TaskRouteError> {
        let task_id = params.parse_task_id()?;
        self.archive_task(task_id)
            .await
            .map(ArchiveTaskRouteResponse::from)
            .map_err(task_route_error_from_task_lifecycle)
    }

    pub async fn unarchive_task_for_route(
        &self,
        params: TaskRouteParams,
    ) -> Result<TaskRouteResponse, TaskRouteError> {
        let task_id = params.parse_task_id()?;
        self.unarchive_task(task_id)
            .await
            .map(TaskRouteResponse::from)
            .map_err(task_route_error_from_task_lifecycle)
    }

    pub async fn delete_task_for_route(
        &self,
        params: TaskRouteParams,
    ) -> Result<(), TaskRouteError> {
        let task_id = params.parse_task_id()?;
        self.delete_task(task_id)
            .await
            .map_err(task_route_error_from_task_lifecycle)
    }
}
