use super::*;
use ctx_daemon::daemon::ResourceUtilizationHandle;
use ctx_resource_utilization::route_contract::{
    ResourceUtilizationRouteError, ResourceUtilizationRouteErrorKind,
    ResourceUtilizationRouteQuery, ResourceUtilizationRouteResponse,
};

pub(in crate::api) async fn resource_utilization(
    State(state): State<ResourceUtilizationHandle>,
    Query(query): Query<ResourceUtilizationRouteQuery>,
) -> Result<Json<ResourceUtilizationRouteResponse>, StatusCode> {
    state
        .workspace_resource_utilization_snapshot_for_route(query)
        .await
        .map(Json)
        .map_err(resource_utilization_status)
}

fn resource_utilization_status(error: ResourceUtilizationRouteError) -> StatusCode {
    match error.kind() {
        ResourceUtilizationRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        ResourceUtilizationRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
        ResourceUtilizationRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
