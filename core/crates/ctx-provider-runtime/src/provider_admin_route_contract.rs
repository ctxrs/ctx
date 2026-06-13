use ctx_providers::adapters::ProviderRestartMode;
use serde::{Deserialize, Serialize};

use crate::provider_workers::ProviderAdapterRestartResult;

#[derive(Debug, Clone, Serialize)]
pub struct ProviderMatrixRefreshRouteResponse {
    provider_count: usize,
    generated_at: Option<String>,
    source: String,
    degraded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
}

impl ProviderMatrixRefreshRouteResponse {
    pub fn new(
        provider_count: usize,
        generated_at: Option<String>,
        source: String,
        degraded: bool,
        last_error: Option<String>,
    ) -> Self {
        Self {
            provider_count,
            generated_at,
            source,
            degraded,
            last_error,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ProviderDevRestartRouteRequest {
    mode: String,
    #[serde(default)]
    reason: Option<String>,
}

impl ProviderDevRestartRouteRequest {
    pub fn new(mode: String, reason: Option<String>) -> Self {
        Self { mode, reason }
    }

    pub fn into_parts(self) -> (String, Option<String>) {
        (self.mode, self.reason)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderDevRestartRouteResponse {
    mode: String,
    results: Vec<ProviderDevRestartRouteResult>,
}

impl ProviderDevRestartRouteResponse {
    pub fn new(mode: ProviderRestartMode, results: Vec<ProviderAdapterRestartResult>) -> Self {
        Self {
            mode: mode.as_str().to_string(),
            results: results.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderDevRestartRouteResult {
    provider_id: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

impl From<ProviderAdapterRestartResult> for ProviderDevRestartRouteResult {
    fn from(result: ProviderAdapterRestartResult) -> Self {
        Self {
            provider_id: result.provider_id,
            status: result.status.as_str().to_string(),
            message: result.message,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderAdminRouteErrorKind {
    NotFound,
    BadRequest,
    Internal,
}

#[derive(Debug)]
pub struct ProviderAdminRouteError {
    kind: ProviderAdminRouteErrorKind,
    message: String,
}

impl ProviderAdminRouteError {
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderAdminRouteErrorKind::NotFound,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderAdminRouteErrorKind::BadRequest,
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderAdminRouteErrorKind::Internal,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> ProviderAdminRouteErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for ProviderAdminRouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

#[cfg(test)]
mod tests {
    use crate::provider_workers::ProviderAdapterRestartStatus;

    use super::*;

    #[test]
    fn admin_error_preserves_kind_and_message() {
        let error = ProviderAdminRouteError::bad_request("bad route");

        assert_eq!(error.kind(), ProviderAdminRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "bad route");
        assert_eq!(error.to_string(), "bad route");
    }

    #[test]
    fn dev_restart_response_preserves_json_shape_and_skips_empty_message() {
        let response = ProviderDevRestartRouteResponse::new(
            ProviderRestartMode::Immediate,
            vec![
                ProviderAdapterRestartResult {
                    provider_id: "ok-provider".to_string(),
                    status: ProviderAdapterRestartStatus::Ok,
                    message: None,
                },
                ProviderAdapterRestartResult {
                    provider_id: "bad-provider".to_string(),
                    status: ProviderAdapterRestartStatus::Error,
                    message: Some("restart failed".to_string()),
                },
            ],
        );
        let payload = serde_json::to_value(response).unwrap();

        assert_eq!(payload["mode"].as_str(), Some("immediate"));
        assert_eq!(
            payload["results"][0]["provider_id"].as_str(),
            Some("ok-provider")
        );
        assert_eq!(payload["results"][0]["status"].as_str(), Some("ok"));
        assert!(payload["results"][0].get("message").is_none());
        assert_eq!(
            payload["results"][1]["message"].as_str(),
            Some("restart failed")
        );
    }

    #[test]
    fn matrix_refresh_response_skips_absent_last_error() {
        let response =
            ProviderMatrixRefreshRouteResponse::new(3, None, "builtin".to_string(), false, None);
        let payload = serde_json::to_value(response).unwrap();

        assert_eq!(payload["provider_count"].as_u64(), Some(3));
        assert_eq!(payload["source"].as_str(), Some("builtin"));
        assert!(payload.get("last_error").is_none());
    }
}
