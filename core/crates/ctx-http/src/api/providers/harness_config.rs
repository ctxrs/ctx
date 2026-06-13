use super::*;
use ctx_provider_runtime::{
    ProviderHarnessConfigRouteError, ProviderHarnessEndpointRouteError,
    ProviderHarnessEndpointRouteErrorKind, ProviderHarnessSourceConfig,
};

mod endpoints;

pub(crate) use endpoints::{
    delete_provider_harness_endpoint, refresh_provider_harness_endpoint_models,
    set_provider_harness_endpoint_manual_models, upsert_provider_harness_endpoint,
};

fn provider_harness_config_error(
    error: ProviderHarnessConfigRouteError,
) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({
            "error": error.message(),
        })),
    )
}

fn provider_harness_endpoint_error(
    error: ProviderHarnessEndpointRouteError,
) -> (StatusCode, Json<serde_json::Value>) {
    let status = match error.kind() {
        ProviderHarnessEndpointRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        ProviderHarnessEndpointRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
    };
    (
        status,
        Json(serde_json::json!({
            "error": error.message(),
        })),
    )
}

pub(crate) async fn get_provider_harness_config(
    State(providers): State<ProviderHarnessConfigHandle>,
    Path(id): Path<String>,
) -> Result<Json<ProviderHarnessSourceConfig>, (StatusCode, Json<serde_json::Value>)> {
    let config = providers
        .get_provider_harness_config_for_route(&id)
        .await
        .map_err(provider_harness_config_error)?;
    Ok(Json(config))
}

pub(crate) async fn select_provider_harness_source(
    State(providers): State<ProviderHarnessConfigHandle>,
    Path(id): Path<String>,
    Json(req): Json<SelectProviderHarnessSourceRouteRequest>,
) -> Result<Json<ProviderHarnessSourceConfig>, (StatusCode, Json<serde_json::Value>)> {
    let config = providers
        .select_provider_harness_source_for_route(&id, req)
        .await
        .map_err(provider_harness_config_error)?;
    Ok(Json(config))
}
