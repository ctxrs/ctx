use ctx_harness_sources as harness_sources;
use serde::Deserialize;

pub type ProviderHarnessSourceConfig = harness_sources::HarnessProviderSourceConfig;

#[derive(Debug, Deserialize)]
pub struct UpsertProviderHarnessEndpointRouteRequest {
    #[serde(default)]
    endpoint_id: Option<String>,
    name: String,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    api_shape: Option<harness_sources::HarnessApiShape>,
    #[serde(default)]
    auth_type: Option<String>,
    #[serde(default)]
    model_override: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    service_account_json: Option<String>,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    manual_model_ids: Option<Vec<String>>,
}

impl UpsertProviderHarnessEndpointRouteRequest {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        endpoint_id: Option<String>,
        name: String,
        base_url: Option<String>,
        api_shape: Option<harness_sources::HarnessApiShape>,
        auth_type: Option<String>,
        model_override: Option<String>,
        api_key: Option<String>,
        service_account_json: Option<String>,
        project_id: Option<String>,
        location: Option<String>,
        manual_model_ids: Option<Vec<String>>,
    ) -> Self {
        Self {
            endpoint_id,
            name,
            base_url,
            api_shape,
            auth_type,
            model_override,
            api_key,
            service_account_json,
            project_id,
            location,
            manual_model_ids,
        }
    }

    #[allow(clippy::type_complexity)]
    pub fn into_parts(
        self,
    ) -> (
        Option<String>,
        String,
        Option<String>,
        Option<harness_sources::HarnessApiShape>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<Vec<String>>,
    ) {
        (
            self.endpoint_id,
            self.name,
            self.base_url,
            self.api_shape,
            self.auth_type,
            self.model_override,
            self.api_key,
            self.service_account_json,
            self.project_id,
            self.location,
            self.manual_model_ids,
        )
    }
}

#[derive(Debug, Deserialize)]
pub struct SetProviderHarnessEndpointManualModelsRouteRequest {
    #[serde(default)]
    model_ids: Vec<String>,
}

impl SetProviderHarnessEndpointManualModelsRouteRequest {
    pub fn new(model_ids: Vec<String>) -> Self {
        Self { model_ids }
    }

    pub fn into_model_ids(self) -> Vec<String> {
        self.model_ids
    }
}

#[derive(Debug, Deserialize)]
pub struct SelectProviderHarnessSourceRouteRequest {
    source_kind: harness_sources::HarnessSourceKind,
    #[serde(default)]
    endpoint_id: Option<String>,
}

impl SelectProviderHarnessSourceRouteRequest {
    pub fn new(
        source_kind: harness_sources::HarnessSourceKind,
        endpoint_id: Option<String>,
    ) -> Self {
        Self {
            source_kind,
            endpoint_id,
        }
    }

    pub fn into_parts(self) -> (harness_sources::HarnessSourceKind, Option<String>) {
        (self.source_kind, self.endpoint_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderHarnessConfigRouteError {
    message: String,
}

impl ProviderHarnessConfigRouteError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderHarnessEndpointRouteErrorKind {
    BadRequest,
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderHarnessEndpointRouteError {
    kind: ProviderHarnessEndpointRouteErrorKind,
    message: String,
}

impl ProviderHarnessEndpointRouteError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderHarnessEndpointRouteErrorKind::BadRequest,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderHarnessEndpointRouteErrorKind::NotFound,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> ProviderHarnessEndpointRouteErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn manual_models_request_defaults_to_empty_list() {
        let request: SetProviderHarnessEndpointManualModelsRouteRequest =
            serde_json::from_value(json!({})).unwrap();

        assert!(request.into_model_ids().is_empty());
    }

    #[test]
    fn select_source_request_preserves_endpoint_id_default() {
        let request: SelectProviderHarnessSourceRouteRequest = serde_json::from_value(json!({
            "source_kind": "subscription"
        }))
        .unwrap();

        let (source_kind, endpoint_id) = request.into_parts();
        assert_eq!(
            source_kind,
            harness_sources::HarnessSourceKind::Subscription
        );
        assert_eq!(endpoint_id, None);
    }

    #[test]
    fn endpoint_error_preserves_kind_and_message() {
        let error = ProviderHarnessEndpointRouteError::not_found("unknown endpoint");

        assert_eq!(
            error.kind(),
            ProviderHarnessEndpointRouteErrorKind::NotFound
        );
        assert_eq!(error.message(), "unknown endpoint");
    }
}
