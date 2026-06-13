use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;

use crate::api::workspaces::{workspace_route_status, WorkspaceRouteParams};
use ctx_daemon::daemon::WorkspaceFileCompletionsHandle;
use ctx_route_contracts::workspaces::WorkspaceFileCompletionsRouteQuery;

pub(in crate::api) async fn workspace_file_completions(
    State(file_completions): State<WorkspaceFileCompletionsHandle>,
    Path(id): Path<String>,
    Query(q): Query<WorkspaceFileCompletionsRouteQuery>,
) -> Result<Json<Vec<String>>, StatusCode> {
    file_completions
        .workspace_file_completions_for_route(WorkspaceRouteParams::new(id), q)
        .await
        .map(Json)
        .map_err(|error| workspace_route_status(&error))
}
