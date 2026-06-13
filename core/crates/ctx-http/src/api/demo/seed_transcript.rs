use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;

use super::dev_mode::dev_tools_enabled;
use crate::api::errors::ApiErrorResp;
use ctx_daemon::daemon::DemoSeedTranscriptHandle;
use ctx_route_contracts::sessions::{
    DemoSeedTranscriptRouteError, DemoSeedTranscriptRouteErrorKind, DemoSeedTranscriptRouteRequest,
    DemoSeedTranscriptRouteResponse, SessionRouteParams,
};

pub(crate) async fn dev_seed_session_transcript(
    State(demo_seed): State<DemoSeedTranscriptHandle>,
    Path(id): Path<String>,
    Json(req): Json<DemoSeedTranscriptRouteRequest>,
) -> Result<Json<DemoSeedTranscriptRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    if !dev_tools_enabled() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ApiErrorResp {
                error: "dev tools are disabled".to_string(),
            }),
        ));
    }

    let response = demo_seed
        .seed_demo_transcript_for_route(SessionRouteParams::new(id), req)
        .await
        .map_err(seed_transcript_error)?;
    Ok(Json(response))
}

fn seed_transcript_error(error: DemoSeedTranscriptRouteError) -> (StatusCode, Json<ApiErrorResp>) {
    let status = match error.kind() {
        DemoSeedTranscriptRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        DemoSeedTranscriptRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
        DemoSeedTranscriptRouteErrorKind::Conflict => StatusCode::CONFLICT,
        DemoSeedTranscriptRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(ApiErrorResp {
            error: error.message().to_string(),
        }),
    )
}
