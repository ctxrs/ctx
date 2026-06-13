use super::*;
use ctx_daemon::daemon::UpdateReleaseHandle;
use ctx_update_service::route_contract::UpdateCheckSnapshot;

#[derive(Debug, Deserialize)]
pub(in crate::api) struct UpdateCheckQuery {
    #[serde(default)]
    channel: Option<String>,
}

pub(in crate::api) async fn check_updates(
    State(updates): State<UpdateReleaseHandle>,
    axum::extract::Query(q): axum::extract::Query<UpdateCheckQuery>,
) -> Result<Json<UpdateCheckSnapshot>, (StatusCode, Json<ApiErrorResp>)> {
    updates
        .check_updates(env!("CARGO_PKG_VERSION"), q.channel)
        .await
        .map(Json)
        .map_err(update_route_error)
}
