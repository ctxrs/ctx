use std::time::Duration;

mod errors;
mod handlers;

use errors::provider_install_error_response;
pub(in crate::api) use handlers::*;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::Json;
use futures::{Stream, StreamExt};
use serde::Deserialize;

use ctx_daemon::daemon::{
    ProviderInstallHandle, ProviderOptionsHandle, ProviderWorkspaceAuthHandle,
};
use ctx_provider_install::{
    ProviderInstallInfo, ProviderInstallProgressEvent, ProviderInstallStartRouteResponse,
    ProviderInstallStatusOnlyRouteError, ProviderInstallStatusesRouteRequest,
    ProviderInstallStatusesRouteResponse,
};
use ctx_provider_runtime::{
    AuthenticateProviderForWorkspaceRouteBody, AuthenticateProviderForWorkspaceRouteRequest,
    ProviderAuthCheckRouteError, ProviderAuthCheckRouteErrorStatus, ProviderAuthCheckRouteResponse,
    ProviderOptionsRouteError, ProviderOptionsRouteErrorStatus, ProviderOptionsRouteRequest,
    VerifyProviderForWorkspaceRouteRequest,
};

#[derive(Debug, Deserialize)]
pub(super) struct RawInstallTargetQuery {
    target: Option<String>,
}
