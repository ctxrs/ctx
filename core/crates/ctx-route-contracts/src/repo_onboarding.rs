use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Eq, PartialEq)]
pub struct RepoInitRouteRequest {
    path: String,
    #[serde(default)]
    allow_existing: bool,
    #[serde(default)]
    allow_non_empty: bool,
}

impl RepoInitRouteRequest {
    pub fn new(path: impl Into<String>, allow_existing: bool, allow_non_empty: bool) -> Self {
        Self {
            path: path.into(),
            allow_existing,
            allow_non_empty,
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn allow_existing(&self) -> bool {
        self.allow_existing
    }

    pub fn allow_non_empty(&self) -> bool {
        self.allow_non_empty
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq)]
pub struct RepoCloneRouteRequest {
    repo_url: String,
    dest_parent: String,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    dest_name: Option<String>,
}

impl RepoCloneRouteRequest {
    pub fn repo_url(&self) -> &str {
        &self.repo_url
    }

    pub fn dest_parent(&self) -> &str {
        &self.dest_parent
    }

    pub fn branch(&self) -> Option<&str> {
        self.branch.as_deref()
    }

    pub fn dest_name(&self) -> Option<&str> {
        self.dest_name.as_deref()
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq)]
pub struct RepoValidateDestinationRouteRequest {
    path: String,
    #[serde(default)]
    must_not_exist: bool,
    #[serde(default)]
    require_empty_if_exists: bool,
}

impl RepoValidateDestinationRouteRequest {
    pub fn new(
        path: impl Into<String>,
        must_not_exist: bool,
        require_empty_if_exists: bool,
    ) -> Self {
        Self {
            path: path.into(),
            must_not_exist,
            require_empty_if_exists,
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn must_not_exist(&self) -> bool {
        self.must_not_exist
    }

    pub fn require_empty_if_exists(&self) -> bool {
        self.require_empty_if_exists
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq)]
pub struct RepoStatusRouteRequest {
    path: String,
}

impl RepoStatusRouteRequest {
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &str {
        &self.path
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct RepoPathRouteResponse {
    path: String,
}

impl RepoPathRouteResponse {
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct RepoStatusRouteResponse {
    canonical_path: String,
    is_repo: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl RepoStatusRouteResponse {
    pub fn new(canonical_path: impl Into<String>, is_repo: bool, error: Option<String>) -> Self {
        Self {
            canonical_path: canonical_path.into(),
            is_repo,
            error,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RepoOnboardingRouteErrorKind {
    BadRequest,
    Internal,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RepoOnboardingRouteError {
    kind: RepoOnboardingRouteErrorKind,
    message: String,
}

impl RepoOnboardingRouteError {
    pub fn new(kind: RepoOnboardingRouteErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> RepoOnboardingRouteErrorKind {
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
    fn route_requests_preserve_serde_defaults() {
        let init: RepoInitRouteRequest = serde_json::from_value(json!({
            "path": "/tmp/repo",
        }))
        .expect("init route request");
        assert_eq!(init.path(), "/tmp/repo");
        assert!(!init.allow_existing());
        assert!(!init.allow_non_empty());

        let clone: RepoCloneRouteRequest = serde_json::from_value(json!({
            "repo_url": "https://example.invalid/repo.git",
            "dest_parent": "/tmp",
        }))
        .expect("clone route request");
        assert_eq!(clone.repo_url(), "https://example.invalid/repo.git");
        assert_eq!(clone.dest_parent(), "/tmp");
        assert_eq!(clone.branch(), None);
        assert_eq!(clone.dest_name(), None);

        let destination: RepoValidateDestinationRouteRequest = serde_json::from_value(json!({
            "path": "/tmp/repo",
        }))
        .expect("destination route request");
        assert_eq!(destination.path(), "/tmp/repo");
        assert!(!destination.must_not_exist());
        assert!(!destination.require_empty_if_exists());

        let status: RepoStatusRouteRequest = serde_json::from_value(json!({
            "path": "/tmp/repo",
        }))
        .expect("status route request");
        assert_eq!(status.path(), "/tmp/repo");
    }

    #[test]
    fn route_responses_preserve_wire_shapes() {
        let path_response = RepoPathRouteResponse::new("/tmp/repo");
        assert_eq!(
            serde_json::to_value(path_response).unwrap(),
            json!({
                "path": "/tmp/repo",
            })
        );

        let status_without_error = RepoStatusRouteResponse::new("/tmp/repo", true, None);
        assert_eq!(
            serde_json::to_value(status_without_error).unwrap(),
            json!({
                "canonical_path": "/tmp/repo",
                "is_repo": true,
            })
        );

        let status_with_error =
            RepoStatusRouteResponse::new("/tmp/repo", false, Some("not a repo".to_string()));
        assert_eq!(
            serde_json::to_value(status_with_error).unwrap(),
            json!({
                "canonical_path": "/tmp/repo",
                "is_repo": false,
                "error": "not a repo",
            })
        );
    }
}
