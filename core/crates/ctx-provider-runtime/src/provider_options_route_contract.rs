use serde_json::Value;

#[derive(Debug, Clone)]
pub struct ProviderOptionsRouteRequest {
    workspace_id: String,
    provider_id: String,
}

impl ProviderOptionsRouteRequest {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderOptionsRouteErrorStatus {
    BadRequest,
    NotFound,
    InternalServerError,
}

#[derive(Debug, Clone)]
pub struct ProviderOptionsRouteError {
    status: ProviderOptionsRouteErrorStatus,
    body: Value,
}

impl ProviderOptionsRouteError {
    pub fn new(status: ProviderOptionsRouteErrorStatus, body: Value) -> Self {
        Self { status, body }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::message(ProviderOptionsRouteErrorStatus::BadRequest, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::message(ProviderOptionsRouteErrorStatus::NotFound, message)
    }

    pub fn internal_server_error(message: impl Into<String>) -> Self {
        Self::message(
            ProviderOptionsRouteErrorStatus::InternalServerError,
            message,
        )
    }

    pub fn status(&self) -> ProviderOptionsRouteErrorStatus {
        self.status
    }

    pub fn body(&self) -> &Value {
        &self.body
    }

    fn message(status: ProviderOptionsRouteErrorStatus, message: impl Into<String>) -> Self {
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
    fn request_preserves_path_parts() {
        let request =
            ProviderOptionsRouteRequest::new("workspace-1".to_string(), "qwen".to_string());

        assert_eq!(request.workspace_id(), "workspace-1");
        assert_eq!(request.provider_id(), "qwen");
        assert_eq!(
            request.into_parts(),
            ("workspace-1".to_string(), "qwen".to_string())
        );
    }

    #[test]
    fn error_preserves_status_and_json_message_body() {
        let error = ProviderOptionsRouteError::internal_server_error(
            "selected endpoint missing from provider configuration",
        );

        assert_eq!(
            error.status(),
            ProviderOptionsRouteErrorStatus::InternalServerError
        );
        assert_eq!(
            error.body()["error"].as_str(),
            Some("selected endpoint missing from provider configuration")
        );
    }
}
