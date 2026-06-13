use serde::{Deserialize, Serialize};

use super::{ProviderAuthImportCandidate, ProviderAuthImportResult, ProviderImportedAuthProfile};

#[derive(Debug, Clone, Serialize)]
pub struct ProviderAuthImportCandidatesRouteResponse {
    candidates: Vec<ProviderAuthImportCandidate>,
}

impl ProviderAuthImportCandidatesRouteResponse {
    pub fn new(candidates: Vec<ProviderAuthImportCandidate>) -> Self {
        Self { candidates }
    }

    pub fn candidates(&self) -> &[ProviderAuthImportCandidate] {
        &self.candidates
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderAuthImportProfilesRouteResponse {
    profiles: Vec<ProviderImportedAuthProfile>,
}

impl ProviderAuthImportProfilesRouteResponse {
    pub fn new(profiles: Vec<ProviderImportedAuthProfile>) -> Self {
        Self { profiles }
    }

    pub fn profiles(&self) -> &[ProviderImportedAuthProfile] {
        &self.profiles
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderAuthImportRouteRequest {
    candidate_ids: Vec<String>,
}

impl ProviderAuthImportRouteRequest {
    pub fn new(candidate_ids: Vec<String>) -> Self {
        Self { candidate_ids }
    }

    pub fn candidate_ids(&self) -> &[String] {
        &self.candidate_ids
    }

    pub fn into_candidate_ids(self) -> Vec<String> {
        self.candidate_ids
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderAuthImportRouteResponse {
    results: Vec<ProviderAuthImportResult>,
}

impl ProviderAuthImportRouteResponse {
    pub fn new(results: Vec<ProviderAuthImportResult>) -> Self {
        Self { results }
    }

    pub fn results(&self) -> &[ProviderAuthImportResult] {
        &self.results
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ProviderAuthImportRouteError {
    message: String,
}

impl ProviderAuthImportRouteError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
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
    fn provider_auth_import_route_request_preserves_candidate_ids_shape() {
        let request: ProviderAuthImportRouteRequest =
            serde_json::from_value(json!({ "candidate_ids": ["one", "two"] }))
                .expect("deserialize request");

        assert_eq!(
            request.candidate_ids(),
            &["one".to_string(), "two".to_string()]
        );
        assert_eq!(
            request.into_candidate_ids(),
            vec!["one".to_string(), "two".to_string()]
        );
    }

    #[test]
    fn provider_auth_import_route_responses_preserve_wire_shapes() {
        assert_eq!(
            serde_json::to_value(ProviderAuthImportCandidatesRouteResponse::new(Vec::new()))
                .expect("serialize candidates"),
            json!({ "candidates": [] })
        );
        assert_eq!(
            serde_json::to_value(ProviderAuthImportProfilesRouteResponse::new(Vec::new()))
                .expect("serialize profiles"),
            json!({ "profiles": [] })
        );
        assert_eq!(
            serde_json::to_value(ProviderAuthImportRouteResponse::new(Vec::new()))
                .expect("serialize results"),
            json!({ "results": [] })
        );
    }

    #[test]
    fn provider_auth_import_route_error_exposes_message_only() {
        let error = ProviderAuthImportRouteError::new("auth import failed");

        assert_eq!(error.message(), "auth import failed");
    }
}
