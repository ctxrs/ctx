use base64::Engine;
use ctx_core::ids::WorkspaceId;
use ctx_mobile_access_service::{
    prepare_mobile_secure_proxy_request,
    route_contract::{MobileAccessRouteError, MobileAccessRouteErrorKind},
    MobileAuthContext, MobileSecureProxyAdmission, MobileSecureProxyPayload,
    MobileSecureProxyResponsePayload,
};
use http::{header, Method, StatusCode};
use serde::Serialize;

use crate::daemon::MobileSecureProxyHandle;

const JSON_CONTENT_TYPE: &str = "application/json";

impl MobileSecureProxyHandle {
    pub async fn proxy_mobile_secure_request_for_route(
        &self,
        mobile_auth: Option<MobileAuthContext>,
        payload: MobileSecureProxyPayload,
        package_version: &'static str,
    ) -> Result<MobileSecureProxyResponsePayload, MobileAccessRouteError> {
        match prepare_mobile_secure_proxy_request(mobile_auth, payload)
            .map_err(MobileAccessRouteError::from)?
        {
            MobileSecureProxyAdmission::Admitted(admitted) => {
                dispatch_scoped_secure_proxy_request(
                    self,
                    &Method::GET,
                    &admitted.uri,
                    &admitted.headers,
                    package_version,
                )
                .await
            }
            MobileSecureProxyAdmission::Denied(reason) => secure_error_response(reason.message()),
        }
    }
}

async fn dispatch_scoped_secure_proxy_request(
    proxy: &MobileSecureProxyHandle,
    method: &Method,
    uri: &str,
    headers: &[(String, String)],
    package_version: &'static str,
) -> Result<MobileSecureProxyResponsePayload, MobileAccessRouteError> {
    let path = uri.split_once('?').map(|(path, _)| path).unwrap_or(uri);
    if method != Method::GET {
        return Ok(empty_response(StatusCode::METHOD_NOT_ALLOWED));
    }
    if path == "/api/health" {
        let include_sensitive = health_request_is_authorized(proxy, headers);
        let Ok(snapshot) = proxy
            .health()
            .health_snapshot(package_version, include_sensitive)
        else {
            return Ok(empty_response(StatusCode::INTERNAL_SERVER_ERROR));
        };
        return json_response(StatusCode::OK, &snapshot);
    }
    if path == "/api/workspaces" {
        let Ok(workspaces) = proxy.store().list_workspaces().await else {
            return Ok(empty_response(StatusCode::INTERNAL_SERVER_ERROR));
        };
        return json_response(StatusCode::OK, &workspaces);
    }
    if let Some(workspace_id) = path.strip_prefix("/api/workspaces/") {
        let Ok(workspace_uuid) = uuid::Uuid::parse_str(workspace_id) else {
            return Ok(empty_response(StatusCode::BAD_REQUEST));
        };
        let workspace_id = WorkspaceId(workspace_uuid);
        let Ok(workspace) = proxy.store().get_workspace(workspace_id).await else {
            return Ok(empty_response(StatusCode::INTERNAL_SERVER_ERROR));
        };
        if let Some(workspace) = workspace {
            proxy
                .telemetry()
                .emit(ctx_observability::telemetry::TelemetryEvent::workspace_opened())
                .await;
            return json_response(StatusCode::OK, &workspace);
        }
        return Ok(empty_response(StatusCode::NOT_FOUND));
    }
    Ok(empty_response(StatusCode::NOT_FOUND))
}

fn health_request_is_authorized(
    proxy: &MobileSecureProxyHandle,
    headers: &[(String, String)],
) -> bool {
    let Some(expected) = proxy.health().auth_token() else {
        return true;
    };
    headers.iter().any(|(name, value)| {
        if name.eq_ignore_ascii_case("host") || name.eq_ignore_ascii_case("content-length") {
            return false;
        }
        let Ok(header_name) = header::HeaderName::from_bytes(name.as_bytes()) else {
            return false;
        };
        let Ok(header_value) = header::HeaderValue::from_str(value) else {
            return false;
        };
        if header_name != header::AUTHORIZATION {
            return false;
        }
        header_value
            .to_str()
            .ok()
            .and_then(|value| value.strip_prefix("Bearer "))
            .is_some_and(|value| value == expected)
    })
}

fn secure_error_response(
    message: &str,
) -> Result<MobileSecureProxyResponsePayload, MobileAccessRouteError> {
    json_response(
        StatusCode::UNAUTHORIZED,
        &serde_json::json!({ "error": message }),
    )
}

fn json_response<T: Serialize>(
    status: StatusCode,
    value: &T,
) -> Result<MobileSecureProxyResponsePayload, MobileAccessRouteError> {
    let body = serde_json::to_vec(value).map_err(|_| {
        MobileAccessRouteError::new(
            MobileAccessRouteErrorKind::BadGateway,
            "failed to encode secure response",
        )
    })?;
    Ok(MobileSecureProxyResponsePayload {
        status: status.as_u16(),
        headers: vec![(
            header::CONTENT_TYPE.as_str().to_string(),
            JSON_CONTENT_TYPE.to_string(),
        )],
        body_b64: base64::engine::general_purpose::STANDARD.encode(body),
    })
}

fn empty_response(status: StatusCode) -> MobileSecureProxyResponsePayload {
    MobileSecureProxyResponsePayload {
        status: status.as_u16(),
        headers: Vec::new(),
        body_b64: String::new(),
    }
}
