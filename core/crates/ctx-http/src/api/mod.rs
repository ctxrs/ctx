use std::collections::HashMap;
use std::time::Instant;

use axum::body::{Body, Bytes};
use axum::extract::{Extension, MatchedPath, Path, Query, State};
use axum::http::header;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use opentelemetry::trace::SpanKind;
use opentelemetry::KeyValue;
use serde::{Deserialize, Serialize};

pub(crate) mod artifacts;
mod auth;
mod demo;
mod diagnostics;
pub(crate) mod errors;
mod execution;
mod health;
mod logs_api;
mod mcp_context;
mod merge_queue_api;
mod mobile_access;
mod perf;
mod plugins;
mod provider_launch;
pub(crate) mod providers;
mod repo;
mod request_base;
mod resource_utilization;
mod router;
mod routes;
mod run_archive;
pub(crate) mod sessions;
mod settings;
pub(crate) mod tasks;
mod telemetry;
mod terminals;
mod title_generation;
mod updates;
mod web_sessions;
mod workspaces;
mod ws;

use artifacts::*;
use diagnostics::*;
use execution::*;
use health::*;
use logs_api::*;
use mcp_context::*;
use merge_queue_api::*;
use mobile_access::*;
use plugins::*;
use providers::*;
use repo::*;
use resource_utilization::*;
use run_archive::*;
use sessions::*;
use settings::*;
use tasks::*;
use telemetry::*;
use terminals::*;
use title_generation::*;
use updates::*;
use web_sessions::*;
use workspaces::*;

use request_base::{public_route_url, public_websocket_url, resolve_request_base_url};
pub use router::{router, RouteHandles};

use demo::*;
use errors::ApiErrorResp;
use ws::{
    dictation_livekit_stream_ws, mobile_secure_workspace_stream_ws, terminal_stream_ws,
    web_session_signal, workspace_active_snapshot_stream_ws, workspace_vcs_stream_ws,
};

use ctx_core::models::*;
use ctx_mobile_access_service::MobileAuthContext;
use ctx_observability::perf_telemetry::{PerfMetric, PerfMetricKind};
use ctx_provider_install::install_state::InstallId;
use ctx_transport_runtime::web_sessions::{
    render_web_session_view, WebSessionInfo, WebSessionRunResponse,
};
