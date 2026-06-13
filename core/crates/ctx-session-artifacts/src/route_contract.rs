use ctx_core::models::Artifact;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct SessionArtifactInput {
    absolute_file_path: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    mime_type: Option<String>,
}

impl SessionArtifactInput {
    pub fn new(
        absolute_file_path: impl Into<String>,
        name: Option<String>,
        mime_type: Option<String>,
    ) -> Self {
        Self {
            absolute_file_path: absolute_file_path.into(),
            name,
            mime_type,
        }
    }

    pub fn into_domain(self) -> crate::SessionArtifactInput {
        crate::SessionArtifactInput {
            absolute_file_path: self.absolute_file_path,
            name: self.name,
            mime_type: self.mime_type,
        }
    }
}

impl From<SessionArtifactInput> for crate::SessionArtifactInput {
    fn from(input: SessionArtifactInput) -> Self {
        input.into_domain()
    }
}

#[derive(Debug, Deserialize)]
pub struct SetSessionArtifactsRouteRequest {
    #[serde(default)]
    artifacts: Vec<SessionArtifactInput>,
}

impl SetSessionArtifactsRouteRequest {
    pub fn new(artifacts: Vec<SessionArtifactInput>) -> Self {
        Self { artifacts }
    }

    pub fn into_artifacts(self) -> Vec<SessionArtifactInput> {
        self.artifacts
    }

    pub fn artifacts(&self) -> &[SessionArtifactInput] {
        &self.artifacts
    }
}

#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct SessionArtifactsRouteResponse(Vec<Artifact>);

impl SessionArtifactsRouteResponse {
    pub fn new(artifacts: Vec<Artifact>) -> Self {
        Self(artifacts)
    }

    pub fn into_artifacts(self) -> Vec<Artifact> {
        self.0
    }
}

impl From<Vec<Artifact>> for SessionArtifactsRouteResponse {
    fn from(artifacts: Vec<Artifact>) -> Self {
        Self::new(artifacts)
    }
}

#[derive(Debug, Clone)]
pub struct SessionArtifactDownloadRouteParams {
    session_id: String,
    artifact_id: String,
}

impl SessionArtifactDownloadRouteParams {
    pub fn new(session_id: impl Into<String>, artifact_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            artifact_id: artifact_id.into(),
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn artifact_id(&self) -> &str {
        &self.artifact_id
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum SessionArtifactRouteError {
    Unauthorized(String),
    NotFound,
    BadRequest(String),
    Internal(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn set_request_defaults_missing_artifacts_to_empty() {
        let request: SetSessionArtifactsRouteRequest =
            serde_json::from_value(json!({})).expect("deserialize request");

        assert!(request.artifacts().is_empty());
    }

    #[test]
    fn artifact_input_defaults_optional_fields() {
        let request: SetSessionArtifactsRouteRequest = serde_json::from_value(json!({
            "artifacts": [
                {
                    "absolute_file_path": "/tmp/output.txt"
                }
            ]
        }))
        .expect("deserialize request");

        let domain = request
            .into_artifacts()
            .into_iter()
            .next()
            .expect("artifact")
            .into_domain();
        assert_eq!(domain.absolute_file_path, "/tmp/output.txt");
        assert_eq!(domain.name, None);
        assert_eq!(domain.mime_type, None);
    }

    #[test]
    fn artifact_response_serializes_transparent_array() {
        let response = SessionArtifactsRouteResponse::new(Vec::new());

        assert_eq!(serde_json::to_value(response).unwrap(), json!([]));
    }

    #[test]
    fn download_params_preserve_raw_route_ids() {
        let params = SessionArtifactDownloadRouteParams::new("session-route", "artifact-route");

        assert_eq!(params.session_id(), "session-route");
        assert_eq!(params.artifact_id(), "artifact-route");
    }
}
