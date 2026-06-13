use std::path::PathBuf;

use super::destination::RepoOnboardingPathError;
use super::path_policy::{expand_tilde, validate_absolute_path};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RepoStatusCheck {
    pub canonical_path: PathBuf,
    pub is_repo: bool,
    pub error: Option<String>,
}

pub(super) async fn repo_status(path: &str) -> Result<RepoStatusCheck, RepoOnboardingPathError> {
    let raw = path.trim();
    if raw.is_empty() {
        return Err(RepoOnboardingPathError::new("path is required"));
    }
    let expanded = expand_tilde(raw)?;
    validate_absolute_path(&expanded, "path")?;
    let canonical = tokio::fs::canonicalize(&expanded).await.map_err(|e| {
        RepoOnboardingPathError::new(format!(
            "invalid path '{}': {e}",
            expanded.to_string_lossy()
        ))
    })?;

    let driver = match ctx_fs::vcs::driver_for_path(&canonical).await {
        Ok(driver) => driver,
        Err(err) => {
            return Ok(RepoStatusCheck {
                canonical_path: canonical,
                is_repo: false,
                error: Some(err.to_string()),
            });
        }
    };
    match driver.assert_repo(&canonical).await {
        Ok(()) => Ok(RepoStatusCheck {
            canonical_path: canonical,
            is_repo: true,
            error: None,
        }),
        Err(err) => Ok(RepoStatusCheck {
            canonical_path: canonical,
            is_repo: false,
            error: Some(err.to_string()),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn repo_status_rejects_missing_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("missing");

        let error = repo_status(missing.to_str().expect("utf8 path"))
            .await
            .expect_err("missing path");

        assert!(
            error.message().starts_with("invalid path '"),
            "unexpected error: {}",
            error.message()
        );
    }
}
