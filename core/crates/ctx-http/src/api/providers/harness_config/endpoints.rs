use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;

use super::provider_harness_endpoint_error;
use ctx_daemon::daemon::ProviderHarnessConfigHandle;
use ctx_provider_runtime::{
    ProviderHarnessSourceConfig, SetProviderHarnessEndpointManualModelsRouteRequest,
    UpsertProviderHarnessEndpointRouteRequest,
};

pub(crate) async fn upsert_provider_harness_endpoint(
    State(providers): State<ProviderHarnessConfigHandle>,
    Path(id): Path<String>,
    Json(req): Json<UpsertProviderHarnessEndpointRouteRequest>,
) -> Result<Json<ProviderHarnessSourceConfig>, (StatusCode, Json<serde_json::Value>)> {
    let config = providers
        .upsert_provider_harness_endpoint_for_route(&id, req)
        .await
        .map_err(provider_harness_endpoint_error)?;
    Ok(Json(config))
}

pub(crate) async fn refresh_provider_harness_endpoint_models(
    State(providers): State<ProviderHarnessConfigHandle>,
    Path((id, endpoint_id)): Path<(String, String)>,
) -> Result<Json<ProviderHarnessSourceConfig>, (StatusCode, Json<serde_json::Value>)> {
    let config = providers
        .refresh_provider_harness_endpoint_models_for_route(&id, &endpoint_id)
        .await
        .map_err(provider_harness_endpoint_error)?;
    Ok(Json(config))
}

pub(crate) async fn set_provider_harness_endpoint_manual_models(
    State(providers): State<ProviderHarnessConfigHandle>,
    Path((id, endpoint_id)): Path<(String, String)>,
    Json(req): Json<SetProviderHarnessEndpointManualModelsRouteRequest>,
) -> Result<Json<ProviderHarnessSourceConfig>, (StatusCode, Json<serde_json::Value>)> {
    let config = providers
        .set_provider_harness_endpoint_manual_models_for_route(&id, &endpoint_id, req)
        .await
        .map_err(provider_harness_endpoint_error)?;
    Ok(Json(config))
}

pub(crate) async fn delete_provider_harness_endpoint(
    State(providers): State<ProviderHarnessConfigHandle>,
    Path((id, endpoint_id)): Path<(String, String)>,
) -> Result<Json<ProviderHarnessSourceConfig>, (StatusCode, Json<serde_json::Value>)> {
    let config = providers
        .delete_provider_harness_endpoint_for_route(&id, &endpoint_id)
        .await
        .map_err(provider_harness_endpoint_error)?;
    Ok(Json(config))
}
