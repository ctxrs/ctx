use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, post, put};

use super::*;
use crate::api::artifacts::MAX_BLOB_MULTIPART_BODY_BYTES;
use crate::api::router::RouteState;

mod core_routes;
mod mobile_routes;
mod provider_routes;
mod session_routes;
mod workspace_routes;

use core_routes::core_routes;
use mobile_routes::mobile_routes;
use provider_routes::provider_routes;
use session_routes::session_routes;
use workspace_routes::workspace_routes;

pub(super) fn api_routes() -> axum::Router<RouteState> {
    core_routes()
        .merge(provider_routes())
        .merge(workspace_routes())
        .merge(mobile_routes())
        .merge(session_routes())
}
