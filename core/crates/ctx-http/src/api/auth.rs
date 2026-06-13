use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::http::{Method, Request};
use axum::middleware::Next;
use axum::response::IntoResponse;
use ctx_http_auth::{
    browser_capability_query_token_is_valid, browser_query_secret_bearer_is_valid,
    browser_stream_query_token_is_valid, is_websocket_upgrade, scoped_mcp_route, ScopedMcpRoute,
};

use ctx_daemon::daemon::AuthHandle;
use ctx_mobile_access_service::MobileAuthContext;

mod mobile;

use mobile::verify_mobile_api_token;

pub(super) async fn auth_middleware(
    State(state): State<AuthHandle>,
    mut req: Request<Body>,
    next: Next,
) -> Result<impl IntoResponse, StatusCode> {
    if req.method() == Method::OPTIONS {
        return Ok(next.run(req).await);
    }
    let path = req.uri().path();
    if !path.starts_with("/api/") || path == "/api/health" {
        return Ok(next.run(req).await);
    }
    if path.starts_with("/api/mobile/secure") || path == "/api/mobile/pair" {
        return Ok(next.run(req).await);
    }
    if req.extensions().get::<MobileAuthContext>().is_some() {
        return Ok(next.run(req).await);
    }
    let header_token = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|v| v.to_string());
    let token = header_token;

    if path == "/api/mcp/context" {
        let Some(token_value) = token.as_deref() else {
            return Err(StatusCode::UNAUTHORIZED);
        };
        let Some(mcp_auth) = state.verify_mcp_auth_token(token_value).await else {
            return Err(StatusCode::UNAUTHORIZED);
        };
        req.extensions_mut().insert(mcp_auth);
        return Ok(next.run(req).await);
    }

    if !state.has_auth_token() {
        return Ok(next.run(req).await);
    }
    let is_terminal_stream = path.starts_with("/api/terminals/") && path.ends_with("/stream");
    let is_ws = is_terminal_stream && is_websocket_upgrade(req.headers());
    if is_ws {
        return Ok(next.run(req).await);
    }
    let is_mobile_token_route = path == "/api/mobile/register";

    if token.as_deref() == state.auth_token() {
        return Ok(next.run(req).await);
    }
    if let Some(auth_token) = state.auth_token() {
        if browser_query_secret_bearer_is_valid(path, token.as_deref(), auth_token) {
            return Ok(next.run(req).await);
        }
        if browser_stream_query_token_is_valid(req.method(), path, req.uri().query(), auth_token) {
            return Ok(next.run(req).await);
        }
        if browser_capability_query_token_is_valid(
            req.method(),
            path,
            req.uri().query(),
            auth_token,
        ) {
            return Ok(next.run(req).await);
        }
    }
    if let Some(token_value) = token.as_deref() {
        if let Some(route) = scoped_mcp_route(req.method(), path) {
            if let Some(mcp_auth) = state.verify_mcp_auth_token(token_value).await {
                let allowed = match route {
                    ScopedMcpRoute::SessionSubagents { session_id } => {
                        mcp_auth.allows_subagents(session_id)
                    }
                    ScopedMcpRoute::SessionArtifacts { session_id } => {
                        mcp_auth.allows_artifacts(session_id)
                    }
                    ScopedMcpRoute::MergeQueueSubmit => mcp_auth.capabilities.merge_queue_submit,
                };
                if allowed {
                    req.extensions_mut().insert(mcp_auth);
                    return Ok(next.run(req).await);
                }
                state.emit_mcp_token_denied(
                    mcp_auth,
                    req.method().as_str(),
                    path,
                    "scope_or_capability_mismatch",
                );
            }
        } else if let Some(mcp_auth) = state.verify_mcp_auth_token(token_value).await {
            state.emit_mcp_token_denied(mcp_auth, req.method().as_str(), path, "route_not_allowed");
        }
    }
    if is_mobile_token_route {
        let Some(token_value) = token else {
            return Err(StatusCode::UNAUTHORIZED);
        };
        if let Some(mobile_auth) = verify_mobile_api_token(&state, &token_value).await? {
            req.extensions_mut().insert(mobile_auth);
            return Ok(next.run(req).await);
        }
    }
    Err(StatusCode::UNAUTHORIZED)
}
