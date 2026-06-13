use std::path::PathBuf;

use ctx_repo_onboarding_service as service;
use ctx_route_contracts::repo_onboarding::{
    RepoCloneRouteRequest, RepoInitRouteRequest, RepoOnboardingRouteError,
    RepoOnboardingRouteErrorKind, RepoPathRouteResponse, RepoStatusRouteRequest,
    RepoStatusRouteResponse, RepoValidateDestinationRouteRequest,
};

use crate::daemon::RepoOnboardingHandle;

fn repo_onboarding_route_error(
    error: service::RepoOnboardingServiceError,
) -> RepoOnboardingRouteError {
    let kind = match error.kind() {
        service::RepoOnboardingServiceErrorKind::BadRequest => {
            RepoOnboardingRouteErrorKind::BadRequest
        }
        service::RepoOnboardingServiceErrorKind::Internal => RepoOnboardingRouteErrorKind::Internal,
    };
    RepoOnboardingRouteError::new(kind, error.message())
}

fn repo_path_route_response(path: PathBuf) -> RepoPathRouteResponse {
    RepoPathRouteResponse::new(path.to_string_lossy().to_string())
}

fn repo_status_route_response(status: service::RepoStatusCheck) -> RepoStatusRouteResponse {
    RepoStatusRouteResponse::new(
        status.canonical_path.to_string_lossy().to_string(),
        status.is_repo,
        status.error,
    )
}

impl RepoOnboardingHandle {
    pub async fn initialize_repo_for_route(
        &self,
        req: RepoInitRouteRequest,
    ) -> Result<RepoPathRouteResponse, RepoOnboardingRouteError> {
        service::initialize_repo_with_service_errors(service::RepoInitRequest {
            path: req.path(),
            allow_existing: req.allow_existing(),
            allow_non_empty: req.allow_non_empty(),
        })
        .await
        .map(repo_path_route_response)
        .map_err(repo_onboarding_route_error)
    }

    pub async fn clone_repo_for_route(
        &self,
        req: RepoCloneRouteRequest,
    ) -> Result<RepoPathRouteResponse, RepoOnboardingRouteError> {
        service::clone_repo_with_service_errors(service::RepoCloneRequest {
            repo_url: req.repo_url(),
            dest_parent: req.dest_parent(),
            branch: req.branch(),
            dest_name: req.dest_name(),
        })
        .await
        .map(repo_path_route_response)
        .map_err(repo_onboarding_route_error)
    }

    pub async fn validate_repo_destination_for_route(
        &self,
        req: RepoValidateDestinationRouteRequest,
    ) -> Result<RepoPathRouteResponse, RepoOnboardingRouteError> {
        service::validate_repo_destination_with_service_errors(
            service::RepoValidateDestinationRequest {
                path: req.path(),
                must_not_exist: req.must_not_exist(),
                require_empty_if_exists: req.require_empty_if_exists(),
            },
        )
        .await
        .map(repo_path_route_response)
        .map_err(repo_onboarding_route_error)
    }

    pub async fn create_repo_staging_path_for_route(
        &self,
    ) -> Result<RepoPathRouteResponse, RepoOnboardingRouteError> {
        service::create_repo_staging_path_with_service_errors(self.data_root())
            .await
            .map(repo_path_route_response)
            .map_err(repo_onboarding_route_error)
    }

    pub async fn inspect_repo_status_for_route(
        &self,
        req: RepoStatusRouteRequest,
    ) -> Result<RepoStatusRouteResponse, RepoOnboardingRouteError> {
        service::inspect_repo_status_with_service_errors(req.path())
            .await
            .map(repo_status_route_response)
            .map_err(repo_onboarding_route_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    use crate::test_support::TestDaemon;

    async fn test_repo_onboarding_handle() -> (tempfile::TempDir, RepoOnboardingHandle) {
        let data_root = tempdir().expect("data root");
        let daemon = TestDaemon::new_for_test(
            data_root.path().to_path_buf(),
            "http://127.0.0.1:4567".to_string(),
        )
        .await
        .expect("test daemon");
        (data_root, daemon.repo_onboarding_handle_for_test())
    }

    #[tokio::test]
    async fn create_repo_staging_path_uses_daemon_data_root() {
        let (data_root, repo_onboarding) = test_repo_onboarding_handle().await;

        let response = repo_onboarding
            .create_repo_staging_path_for_route()
            .await
            .expect("route staging path");
        let value = serde_json::to_value(response).expect("route response json");
        let route_path = PathBuf::from(
            value
                .get("path")
                .and_then(|path| path.as_str())
                .expect("path field"),
        );
        let expected_root = data_root
            .path()
            .canonicalize()
            .expect("canonical data root")
            .join("workspaces")
            .join("staging");
        assert!(route_path.exists());
        assert!(route_path.starts_with(expected_root));
    }

    #[tokio::test]
    async fn validate_repo_destination_preserves_path_error_behavior() {
        let (_data_root, repo_onboarding) = test_repo_onboarding_handle().await;

        let route_error = repo_onboarding
            .validate_repo_destination_for_route(RepoValidateDestinationRouteRequest::new(
                "   ", false, false,
            ))
            .await
            .expect_err("blank route path should fail");

        assert_eq!(route_error.kind(), RepoOnboardingRouteErrorKind::BadRequest);
        assert_eq!(route_error.message(), "path is required");
    }

    #[tokio::test]
    async fn initialize_repo_can_be_inspected_as_repo() {
        let (_data_root, repo_onboarding) = test_repo_onboarding_handle().await;
        let temp = tempdir().expect("repo parent");
        let repo_path = temp.path().join("repo");

        let response = repo_onboarding
            .initialize_repo_for_route(RepoInitRouteRequest::new(
                repo_path.to_string_lossy().to_string(),
                false,
                false,
            ))
            .await
            .expect("initialize repo");
        let value = serde_json::to_value(response).expect("route response json");
        let initialized = value
            .get("path")
            .and_then(|path| path.as_str())
            .expect("path field");
        let status = repo_onboarding
            .inspect_repo_status_for_route(RepoStatusRouteRequest::new(initialized))
            .await
            .expect("repo status");
        let status = serde_json::to_value(status).expect("status json");

        assert_eq!(
            status.get("is_repo").and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(status.get("error"), None);
    }

    #[tokio::test]
    async fn inspect_repo_status_missing_path_returns_bad_request() {
        let (_data_root, repo_onboarding) = test_repo_onboarding_handle().await;
        let temp = tempdir().expect("repo parent");
        let missing = temp.path().join("missing");

        let error = repo_onboarding
            .inspect_repo_status_for_route(RepoStatusRouteRequest::new(
                missing.to_string_lossy().to_string(),
            ))
            .await
            .expect_err("missing path should fail");

        assert_eq!(error.kind(), RepoOnboardingRouteErrorKind::BadRequest);
        assert!(error.message().starts_with("invalid path '"));
    }
}
