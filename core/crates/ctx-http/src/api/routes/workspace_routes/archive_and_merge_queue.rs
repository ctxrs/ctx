use super::*;

pub(super) fn workspace_archive_and_merge_queue_routes() -> axum::Router<RouteState> {
    axum::Router::new()
        .route(
            "/api/workspaces/:workspace_id/runs/:run_id/archive/ingest_batch",
            get(build_workspace_run_archive_ingest_batch),
        )
        .route(
            "/api/workspaces/:workspace_id/runs/:run_id/archive/ingest_ack",
            post(acknowledge_workspace_run_archive_ingest_batch),
        )
        .route(
            "/api/workspaces/:workspace_id/merge_queue/entries/:id/cancel",
            post(cancel_merge_queue_entry),
        )
        .route(
            "/api/workspaces/:workspace_id/merge_queue/entries/:id/retry",
            post(retry_merge_queue_entry),
        )
        .route(
            "/api/workspaces/:workspace_id/merge_queue/entries/:id/logs",
            get(get_merge_queue_entry_logs),
        )
}
