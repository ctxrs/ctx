use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider_auth_check::ProviderAuthCheckSnapshot;

#[derive(Debug, Default, Clone, Deserialize)]
pub struct AuthenticateProviderForWorkspaceRouteBody {
    #[serde(default)]
    method_id: Option<String>,
}

impl AuthenticateProviderForWorkspaceRouteBody {
    pub fn new(method_id: Option<String>) -> Self {
        Self { method_id }
    }

    pub fn method_id(&self) -> Option<&str> {
        self.method_id.as_deref()
    }

    pub fn into_method_id(self) -> Option<String> {
        self.method_id
    }
}

#[derive(Debug, Clone)]
pub struct AuthenticateProviderForWorkspaceRouteRequest {
    workspace_id: String,
    provider_id: String,
    method_id: Option<String>,
}

impl AuthenticateProviderForWorkspaceRouteRequest {
    pub fn new(workspace_id: String, provider_id: String, method_id: Option<String>) -> Self {
        Self {
            workspace_id,
            provider_id,
            method_id,
        }
    }

    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    pub fn method_id(&self) -> Option<&str> {
        self.method_id.as_deref()
    }

    pub fn into_parts(self) -> (String, String, Option<String>) {
        (self.workspace_id, self.provider_id, self.method_id)
    }
}

#[derive(Debug, Clone)]
pub struct VerifyProviderForWorkspaceRouteRequest {
    workspace_id: String,
    provider_id: String,
}

impl VerifyProviderForWorkspaceRouteRequest {
    pub fn new(workspace_id: String, provider_id: String) -> Self {
        Self {
            workspace_id,
            provider_id,
        }
    }

    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    pub fn into_parts(self) -> (String, String) {
        (self.workspace_id, self.provider_id)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderAuthCheckRouteResponse {
    provider_id: String,
    workspace_id: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    auth_required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    checked_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

impl ProviderAuthCheckRouteResponse {
    pub fn new(
        provider_id: String,
        workspace_id: String,
        status: String,
        auth_required: Option<bool>,
        checked_at: Option<String>,
        message: Option<String>,
    ) -> Self {
        Self {
            provider_id,
            workspace_id,
            status,
            auth_required,
            checked_at,
            message,
        }
    }
}

impl From<ProviderAuthCheckSnapshot> for ProviderAuthCheckRouteResponse {
    fn from(value: ProviderAuthCheckSnapshot) -> Self {
        Self::new(
            value.provider_id,
            value.workspace_id,
            value.status,
            value.auth_required,
            value.checked_at,
            value.message,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderAuthCheckRouteErrorStatus {
    BadRequest,
    NotFound,
    InternalServerError,
}

#[derive(Debug, Clone)]
pub struct ProviderAuthCheckRouteError {
    status: ProviderAuthCheckRouteErrorStatus,
    body: Value,
}

impl ProviderAuthCheckRouteError {
    pub fn new(status: ProviderAuthCheckRouteErrorStatus, body: Value) -> Self {
        Self { status, body }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::message(ProviderAuthCheckRouteErrorStatus::BadRequest, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::message(ProviderAuthCheckRouteErrorStatus::NotFound, message)
    }

    pub fn internal_server_error(message: impl Into<String>) -> Self {
        Self::message(
            ProviderAuthCheckRouteErrorStatus::InternalServerError,
            message,
        )
    }

    pub fn status(&self) -> ProviderAuthCheckRouteErrorStatus {
        self.status
    }

    pub fn body(&self) -> &Value {
        &self.body
    }

    fn message(status: ProviderAuthCheckRouteErrorStatus, message: impl Into<String>) -> Self {
        Self {
            status,
            body: serde_json::json!({
                "error": message.into(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_body_defaults_missing_method_id() {
        let body: AuthenticateProviderForWorkspaceRouteBody =
            serde_json::from_str("{}").expect("deserialize empty auth body");

        assert_eq!(body.method_id(), None);
        assert_eq!(
            AuthenticateProviderForWorkspaceRouteBody::new(Some("oauth".to_string())).method_id(),
            Some("oauth")
        );
    }

    #[test]
    fn auth_request_preserves_path_and_method_parts() {
        let request = AuthenticateProviderForWorkspaceRouteRequest::new(
            "workspace-1".to_string(),
            "qwen".to_string(),
            Some("browser".to_string()),
        );

        assert_eq!(request.workspace_id(), "workspace-1");
        assert_eq!(request.provider_id(), "qwen");
        assert_eq!(request.method_id(), Some("browser"));
        assert_eq!(
            request.into_parts(),
            (
                "workspace-1".to_string(),
                "qwen".to_string(),
                Some("browser".to_string())
            )
        );
    }

    #[test]
    fn verify_request_preserves_path_parts() {
        let request = VerifyProviderForWorkspaceRouteRequest::new(
            "workspace-1".to_string(),
            "qwen".to_string(),
        );

        assert_eq!(request.workspace_id(), "workspace-1");
        assert_eq!(request.provider_id(), "qwen");
        assert_eq!(
            request.into_parts(),
            ("workspace-1".to_string(), "qwen".to_string())
        );
    }

    #[test]
    fn response_skips_absent_optional_fields() {
        let response = ProviderAuthCheckRouteResponse::new(
            "qwen".to_string(),
            "workspace-1".to_string(),
            "ok".to_string(),
            None,
            None,
            None,
        );
        let payload = serde_json::to_value(response).expect("serialize auth check response");

        assert_eq!(payload["provider_id"].as_str(), Some("qwen"));
        assert_eq!(payload["workspace_id"].as_str(), Some("workspace-1"));
        assert_eq!(payload["status"].as_str(), Some("ok"));
        assert!(payload.get("auth_required").is_none());
        assert!(payload.get("checked_at").is_none());
        assert!(payload.get("message").is_none());
    }

    #[test]
    fn response_from_snapshot_preserves_wire_shape() {
        let response = ProviderAuthCheckRouteResponse::from(ProviderAuthCheckSnapshot {
            provider_id: "qwen".to_string(),
            workspace_id: "workspace-1".to_string(),
            status: "error".to_string(),
            auth_required: Some(true),
            checked_at: Some("2026-05-21T00:00:00Z".to_string()),
            message: Some("login required".to_string()),
        });
        let payload = serde_json::to_value(response).expect("serialize auth check response");

        assert_eq!(payload["provider_id"].as_str(), Some("qwen"));
        assert_eq!(payload["workspace_id"].as_str(), Some("workspace-1"));
        assert_eq!(payload["status"].as_str(), Some("error"));
        assert_eq!(payload["auth_required"].as_bool(), Some(true));
        assert_eq!(payload["checked_at"].as_str(), Some("2026-05-21T00:00:00Z"));
        assert_eq!(payload["message"].as_str(), Some("login required"));
    }

    #[test]
    fn error_preserves_status_and_json_message_body() {
        let error = ProviderAuthCheckRouteError::not_found("workspace not found");

        assert_eq!(error.status(), ProviderAuthCheckRouteErrorStatus::NotFound);
        assert_eq!(error.body()["error"].as_str(), Some("workspace not found"));
    }
}
