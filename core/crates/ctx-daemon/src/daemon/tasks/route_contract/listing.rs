use ctx_route_contracts::tasks::{
    ListWorkspaceArchivedTasksRouteParams, ListWorkspaceTasksRouteParams,
};

use crate::daemon::TaskListingHandle;

use super::common::{task_route_error_from_workspace_store, TaskRouteError};
use super::responses::{TaskRouteResponse, WorkspaceArchivedPageRouteResponse};

impl TaskListingHandle {
    pub async fn list_workspace_tasks_for_route(
        &self,
        params: ListWorkspaceTasksRouteParams,
    ) -> Result<Vec<TaskRouteResponse>, TaskRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        self.list_workspace_tasks(workspace_id)
            .await
            .map(|tasks| tasks.into_iter().map(TaskRouteResponse::from).collect())
            .map_err(task_route_error_from_workspace_store)
    }

    pub async fn list_workspace_archived_page_for_route(
        &self,
        params: ListWorkspaceArchivedTasksRouteParams,
    ) -> Result<WorkspaceArchivedPageRouteResponse, TaskRouteError> {
        let (workspace_id, cursor, limit) = params.parse()?;
        self.list_workspace_archived_page(workspace_id, cursor, limit)
            .await
            .map(WorkspaceArchivedPageRouteResponse::from)
            .map_err(task_route_error_from_workspace_store)
    }
}
