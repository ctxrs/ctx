use super::*;
use ctx_daemon::daemon::UpdateActivityHandle;
use ctx_update_service::route_contract::UpdateActivitySnapshot;

pub(in crate::api) async fn update_activity(
    State(updates): State<UpdateActivityHandle>,
) -> Result<Json<UpdateActivitySnapshot>, (StatusCode, Json<ApiErrorResp>)> {
    updates
        .update_activity_snapshot()
        .await
        .map(Json)
        .map_err(update_route_error)
}
