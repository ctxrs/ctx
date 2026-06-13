use ctx_route_contracts::tasks::UpdateTaskTitleRouteRequest;

use crate::daemon::{TaskReadStateHandle, TaskSessionListingHandle, TaskTitleHandle};

use super::common::{classified_internal_route_error, TaskRouteError, TaskRouteParams};
use super::responses::{SessionRouteResponse, TaskRouteResponse};

impl TaskSessionListingHandle {
    pub async fn list_task_sessions_for_route(
        &self,
        params: TaskRouteParams,
    ) -> Result<Vec<SessionRouteResponse>, TaskRouteError> {
        let task_id = params.parse_task_id()?;
        let sessions = self
            .list_task_sessions(task_id)
            .await
            .map_err(|error| {
                let message = ctx_observability::logs::redact_sensitive(&error.to_string());
                classified_internal_route_error(&error, message)
            })?
            .ok_or_else(|| TaskRouteError::not_found("task not found"))?;
        Ok(sessions
            .into_iter()
            .map(SessionRouteResponse::from)
            .collect())
    }
}

impl TaskReadStateHandle {
    pub async fn mark_task_read_for_route(
        &self,
        params: TaskRouteParams,
    ) -> Result<TaskRouteResponse, TaskRouteError> {
        let task_id = params.parse_task_id()?;
        let task = self
            .mark_task_read(task_id)
            .await
            .map_err(|error| TaskRouteError::internal(error.to_string()))?
            .ok_or_else(|| TaskRouteError::not_found("task not found"))?;
        Ok(TaskRouteResponse::from(task))
    }

    pub async fn mark_task_unread_for_route(
        &self,
        params: TaskRouteParams,
    ) -> Result<TaskRouteResponse, TaskRouteError> {
        let task_id = params.parse_task_id()?;
        let task = self
            .mark_task_unread(task_id)
            .await
            .map_err(|error| TaskRouteError::internal(error.to_string()))?
            .ok_or_else(|| TaskRouteError::not_found("task not found"))?;
        Ok(TaskRouteResponse::from(task))
    }
}

impl TaskTitleHandle {
    pub async fn update_task_title_for_route(
        &self,
        params: TaskRouteParams,
        req: UpdateTaskTitleRouteRequest,
    ) -> Result<TaskRouteResponse, TaskRouteError> {
        let task_id = params.parse_task_id()?;
        let title = req.validated_title()?;
        let task = self
            .update_task_title(task_id, title)
            .await
            .map_err(|error| {
                let message = ctx_observability::logs::redact_sensitive(&error.to_string());
                classified_internal_route_error(&error, message)
            })?
            .ok_or_else(|| TaskRouteError::not_found("task not found"))?;
        Ok(TaskRouteResponse::from(task))
    }
}
