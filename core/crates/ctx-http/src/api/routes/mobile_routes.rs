use super::*;

pub(super) fn mobile_routes() -> axum::Router<RouteState> {
    axum::Router::new()
        .route(
            "/api/mobile/connection_profiles",
            get(list_mobile_connection_profiles).post(create_mobile_connection_profile),
        )
        .route("/api/mobile/access/status", get(get_mobile_access_status))
        .route("/api/mobile/access/enable", post(enable_mobile_access))
        .route("/api/mobile/access/disable", post(disable_mobile_access))
        .route("/api/mobile/pair", post(pair_mobile_device))
        .route("/api/mobile/secure", post(handle_mobile_secure))
        .route(
            "/api/mobile/secure/workspaces/:id/stream",
            get(mobile_secure_workspace_stream_ws),
        )
        .route(
            "/api/mobile/connection_profiles/:id",
            delete(delete_mobile_connection_profile),
        )
        .route(
            "/api/mobile/connection_profiles/:id/devices",
            get(list_mobile_devices_for_profile),
        )
        .route("/api/mobile/register", post(register_mobile_device))
}
