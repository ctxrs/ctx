use super::*;

pub(super) fn core_routes() -> axum::Router<RouteState> {
    axum::Router::new()
        .route("/api/health", get(health))
        .route("/api/mcp/context", get(get_mcp_context))
        .route("/api/settings", get(get_settings).post(update_settings))
        .route("/api/plugins", get(list_plugins))
        .route("/api/plugins/extensions", get(list_plugin_extensions))
        .route("/api/plugins/reload", post(reload_plugins))
        .route(
            "/api/plugins/commands/execute",
            post(execute_plugin_command),
        )
        .route("/api/execution/launch/start", post(launch_start))
        .route("/api/execution/launch/status", get(launch_status))
        .route("/api/execution/launch/stream", get(launch_stream_ws))
        .route(
            "/api/execution/linux_sandbox_runtime/status",
            get(linux_sandbox_runtime_status_api),
        )
        .route(
            "/api/execution/linux_sandbox_runtime/stage",
            post(linux_sandbox_runtime_stage),
        )
        .route(
            "/api/execution/linux_sandbox_runtime/prepare",
            post(linux_sandbox_runtime_prepare),
        )
        .route(
            "/api/title_generation/local/status",
            get(get_title_generation_local_status),
        )
        .route(
            "/api/title_generation/local/install",
            post(install_title_generation_local),
        )
        .route("/api/repo/clone", post(repo_clone))
        .route("/api/repo/init", post(repo_init))
        .route("/api/repo/status", post(repo_status))
        .route(
            "/api/repo/validate_destination",
            get(repo_validate_destination_get).post(repo_validate_destination),
        )
        .route("/api/repo/staging_path", get(repo_staging_path))
        .route("/api/diagnostics", get(diagnostics))
        .route("/api/resource_utilization", get(resource_utilization))
        .route("/api/telemetry/summary", get(get_telemetry_summary))
        .route("/api/telemetry/export", get(export_telemetry))
        .route("/api/telemetry/client", post(post_client_telemetry))
        .route("/api/telemetry/events", post(post_semantic_telemetry))
        .route(
            "/api/blobs",
            post(upload_blob).layer(DefaultBodyLimit::max(MAX_BLOB_MULTIPART_BODY_BYTES)),
        )
        .route("/api/blobs/:id", get(get_blob))
        .route("/api/logs/open", post(open_logs_folder))
        .route("/api/desktop/log", post(append_desktop_log))
        .route("/api/daemon/shutdown", post(shutdown_daemon))
        .route("/api/updates/check", get(check_updates))
        .route("/api/updates/activity", get(update_activity))
        .route("/api/updates/drain/begin", post(begin_update_drain))
        .route("/api/updates/drain/release", post(release_update_drain))
        .route(
            "/api/updates/appimage/download",
            post(download_appimage_update),
        )
        .route("/api/updates/appimage/apply", post(apply_appimage_update))
        .route(
            "/api/merge-queue/entries",
            get(list_merge_queue_entries).post(submit_merge_queue_entry),
        )
        .route("/api/dev/providers/restart", post(dev_restart_providers))
        .route("/api/dev/clock", get(dev_clock))
        .route(
            "/api/dev/sessions/:id/seed_transcript",
            post(dev_seed_session_transcript),
        )
        .route(
            "/api/dictation/livekit/stream",
            get(dictation_livekit_stream_ws),
        )
}
