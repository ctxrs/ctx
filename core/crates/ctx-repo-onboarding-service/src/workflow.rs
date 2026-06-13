use std::path::PathBuf;

use super::destination::{
    prepare_clone_destination, prepare_repo_init_path, validate_repo_destination,
    RepoCloneDestinationRequest, RepoInitPathRequest, RepoOnboardingPathError,
};
use super::git::{
    canonical_clone_dest, ensure_git_usable, init_git_repo_with_initial_commit, run_git_clone,
    RepoGitCommandError,
};
use super::staging::create_repo_staging_path;
use super::status::{repo_status, RepoStatusCheck};
use super::RepoValidateDestinationRequest;

pub struct RepoInitRequest<'a> {
    pub path: &'a str,
    pub allow_existing: bool,
    pub allow_non_empty: bool,
}

pub struct RepoCloneRequest<'a> {
    pub repo_url: &'a str,
    pub dest_parent: &'a str,
    pub branch: Option<&'a str>,
    pub dest_name: Option<&'a str>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RepoOnboardingWorkflowError {
    GitPreflight(String),
    GitCommand(RepoGitCommandError),
    Path(RepoOnboardingPathError),
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RepoOnboardingServiceErrorKind {
    BadRequest,
    Internal,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RepoOnboardingServiceError {
    kind: RepoOnboardingServiceErrorKind,
    message: String,
}

impl RepoOnboardingServiceError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            kind: RepoOnboardingServiceErrorKind::BadRequest,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: RepoOnboardingServiceErrorKind::Internal,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> RepoOnboardingServiceErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl From<RepoGitCommandError> for RepoOnboardingWorkflowError {
    fn from(value: RepoGitCommandError) -> Self {
        Self::GitCommand(value)
    }
}

impl From<RepoOnboardingPathError> for RepoOnboardingWorkflowError {
    fn from(value: RepoOnboardingPathError) -> Self {
        Self::Path(value)
    }
}

pub async fn initialize_repo(
    req: RepoInitRequest<'_>,
) -> Result<PathBuf, RepoOnboardingWorkflowError> {
    ensure_git_usable()
        .await
        .map_err(RepoOnboardingWorkflowError::GitPreflight)?;

    let path = prepare_repo_init_path(RepoInitPathRequest {
        path: req.path,
        allow_existing: req.allow_existing,
        allow_non_empty: req.allow_non_empty,
    })
    .await?;

    // Worktrees require a base commit to diff against. `git init` alone yields a repo with no
    // commits, which breaks the out-of-the-box wizard path ("New repo").
    //
    // Use inline identity overrides so this workflow does not depend on global git config.
    init_git_repo_with_initial_commit(&path).await?;

    Ok(tokio::fs::canonicalize(&path).await.unwrap_or(path))
}

pub async fn clone_repo(req: RepoCloneRequest<'_>) -> Result<PathBuf, RepoOnboardingWorkflowError> {
    ensure_git_usable()
        .await
        .map_err(RepoOnboardingWorkflowError::GitPreflight)?;

    let repo_url = req.repo_url.trim();
    if repo_url.is_empty() {
        return Err(RepoOnboardingPathError::new("repo_url is required").into());
    }

    let dest = prepare_clone_destination(RepoCloneDestinationRequest {
        repo_url,
        dest_parent: req.dest_parent,
        dest_name: req.dest_name,
    })
    .await?;

    run_git_clone(
        repo_url,
        req.branch.map(str::trim).filter(|value| !value.is_empty()),
        &dest,
    )
    .await?;

    Ok(canonical_clone_dest(dest).await)
}

pub async fn inspect_repo_status(
    path: &str,
) -> Result<RepoStatusCheck, RepoOnboardingWorkflowError> {
    ensure_git_usable()
        .await
        .map_err(RepoOnboardingWorkflowError::GitPreflight)?;
    Ok(repo_status(path).await?)
}

pub async fn initialize_repo_with_service_errors(
    req: RepoInitRequest<'_>,
) -> Result<PathBuf, RepoOnboardingServiceError> {
    initialize_repo(req).await.map_err(repo_workflow_error)
}

pub async fn clone_repo_with_service_errors(
    req: RepoCloneRequest<'_>,
) -> Result<PathBuf, RepoOnboardingServiceError> {
    clone_repo(req).await.map_err(repo_workflow_error)
}

pub async fn validate_repo_destination_with_service_errors(
    req: RepoValidateDestinationRequest<'_>,
) -> Result<PathBuf, RepoOnboardingServiceError> {
    validate_repo_destination(req)
        .await
        .map_err(repo_path_error)
}

pub async fn create_repo_staging_path_with_service_errors(
    data_root: &std::path::Path,
) -> Result<PathBuf, RepoOnboardingServiceError> {
    create_repo_staging_path(data_root)
        .await
        .map_err(repo_staging_path_error)
}

pub async fn inspect_repo_status_with_service_errors(
    path: &str,
) -> Result<RepoStatusCheck, RepoOnboardingServiceError> {
    let mut status = inspect_repo_status(path)
        .await
        .map_err(repo_workflow_error)?;
    status.error = status
        .error
        .map(|error| ctx_core::redaction::redact_sensitive(&error));
    Ok(status)
}

fn repo_git_command_error(error: RepoGitCommandError) -> RepoOnboardingServiceError {
    if let Some(message) = error.spawn_message() {
        return RepoOnboardingServiceError::internal(format!("failed to spawn git: {message}"));
    }
    RepoOnboardingServiceError::bad_request(ctx_core::redaction::redact_sensitive(
        &error
            .failed_message()
            .unwrap_or_else(|| "git command failed".to_string()),
    ))
}

fn repo_path_error(error: RepoOnboardingPathError) -> RepoOnboardingServiceError {
    RepoOnboardingServiceError::bad_request(error.message().to_string())
}

fn repo_staging_path_error(error: RepoOnboardingPathError) -> RepoOnboardingServiceError {
    RepoOnboardingServiceError::internal(error.message().to_string())
}

fn repo_workflow_error(error: RepoOnboardingWorkflowError) -> RepoOnboardingServiceError {
    match error {
        RepoOnboardingWorkflowError::GitPreflight(error) => {
            RepoOnboardingServiceError::bad_request(error)
        }
        RepoOnboardingWorkflowError::GitCommand(error) => repo_git_command_error(error),
        RepoOnboardingWorkflowError::Path(error) => repo_path_error(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn initialize_repo_creates_repo_with_initial_commit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("new-repo");

        let initialized = initialize_repo(RepoInitRequest {
            path: path.to_str().expect("utf8 path"),
            allow_existing: false,
            allow_non_empty: false,
        })
        .await
        .expect("initialize repo");

        assert_eq!(initialized, path);
        let head = tokio::process::Command::new("git")
            .arg("-C")
            .arg(&initialized)
            .arg("rev-parse")
            .arg("--verify")
            .arg("HEAD")
            .output()
            .await
            .expect("git rev-parse");
        assert!(
            head.status.success(),
            "expected initial commit, stderr: {}",
            String::from_utf8_lossy(&head.stderr)
        );
    }

    #[tokio::test]
    async fn clone_repo_rejects_empty_repo_url() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let error = clone_repo(RepoCloneRequest {
            repo_url: "   ",
            dest_parent: tmp.path().to_str().expect("utf8 path"),
            branch: None,
            dest_name: None,
        })
        .await
        .expect_err("empty repo url");

        assert_eq!(
            error,
            RepoOnboardingWorkflowError::Path(RepoOnboardingPathError::new("repo_url is required"))
        );
    }

    #[tokio::test]
    async fn inspect_repo_status_rejects_missing_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("missing");

        let error = inspect_repo_status(missing.to_str().expect("utf8 path"))
            .await
            .expect_err("missing path");

        match error {
            RepoOnboardingWorkflowError::Path(path_error) => {
                assert!(
                    path_error.message().starts_with("invalid path '"),
                    "unexpected error: {}",
                    path_error.message()
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn service_errors_redact_and_classify_git_failures() {
        let spawn = repo_git_command_error(RepoGitCommandError::Spawn {
            message: "permission denied".to_string(),
        });
        assert_eq!(spawn.kind(), RepoOnboardingServiceErrorKind::Internal);
        assert_eq!(spawn.message(), "failed to spawn git: permission denied");

        let failed = repo_git_command_error(RepoGitCommandError::Failed {
            action: "git clone",
            stderr: "fatal: https://example.invalid/repo.git?token=secret-token\n".to_string(),
        });
        assert_eq!(failed.kind(), RepoOnboardingServiceErrorKind::BadRequest);
        assert!(failed.message().contains("git clone failed"));
        assert!(!failed.message().contains("secret-token"));
    }

    #[test]
    fn service_errors_preserve_preflight_path_and_staging_categories() {
        let preflight = repo_workflow_error(RepoOnboardingWorkflowError::GitPreflight(
            "git is required".to_string(),
        ));
        assert_eq!(preflight.kind(), RepoOnboardingServiceErrorKind::BadRequest);
        assert_eq!(preflight.message(), "git is required");

        let path = repo_path_error(RepoOnboardingPathError::from(
            "path is required".to_string(),
        ));
        assert_eq!(path.kind(), RepoOnboardingServiceErrorKind::BadRequest);
        assert_eq!(path.message(), "path is required");

        let staging = repo_staging_path_error(RepoOnboardingPathError::from(
            "failed to create staging dir".to_string(),
        ));
        assert_eq!(staging.kind(), RepoOnboardingServiceErrorKind::Internal);
        assert_eq!(staging.message(), "failed to create staging dir");
    }
}
