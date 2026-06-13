use ctx_route_contracts::workspaces::{WorkspaceFileCompletionsRouteQuery, WorkspaceRouteParams};

use super::route_contract::file_completions_route_error;
use super::WorkspaceRouteError;
use crate::daemon::WorkspaceFileCompletionsHandle;

impl WorkspaceFileCompletionsHandle {
    pub async fn workspace_file_completions_for_route(
        &self,
        params: WorkspaceRouteParams,
        query: WorkspaceFileCompletionsRouteQuery,
    ) -> Result<Vec<String>, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        let (query, limit) = query.into_parts();
        self.complete_files_for_workspace(workspace_id, query, limit)
            .await
            .map_err(file_completions_route_error)
    }
}
