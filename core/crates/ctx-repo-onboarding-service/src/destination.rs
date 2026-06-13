use std::path::PathBuf;

use super::path_policy::{
    derive_repo_name, expand_tilde, validate_absolute_path, validate_dest_name,
};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RepoOnboardingPathError {
    message: String,
}

impl RepoOnboardingPathError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl From<String> for RepoOnboardingPathError {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

pub(super) struct RepoInitPathRequest<'a> {
    pub path: &'a str,
    pub allow_existing: bool,
    pub allow_non_empty: bool,
}

pub struct RepoValidateDestinationRequest<'a> {
    pub path: &'a str,
    pub must_not_exist: bool,
    pub require_empty_if_exists: bool,
}

pub(super) struct RepoCloneDestinationRequest<'a> {
    pub repo_url: &'a str,
    pub dest_parent: &'a str,
    pub dest_name: Option<&'a str>,
}

pub(super) async fn prepare_repo_init_path(
    req: RepoInitPathRequest<'_>,
) -> Result<PathBuf, RepoOnboardingPathError> {
    let raw = req.path.trim();
    if raw.is_empty() {
        return Err(RepoOnboardingPathError::new("path is required"));
    }

    let path = expand_tilde(raw)?;
    validate_absolute_path(&path, "path")?;

    if path.exists() && !req.allow_existing {
        return Err(RepoOnboardingPathError::new(format!(
            "destination already exists: {}",
            path.display()
        )));
    }
    tokio::fs::create_dir_all(&path).await.map_err(|e| {
        RepoOnboardingPathError::new(format!(
            "failed to create directory '{}': {e}",
            path.display()
        ))
    })?;

    // By default we refuse to init into a non-empty directory.
    // Import onboarding can opt in with allow_non_empty=true after explicit user confirmation.
    if !req.allow_non_empty && directory_has_entries(&path).await? {
        return Err(RepoOnboardingPathError::new(format!(
            "destination is not empty: {}",
            path.display()
        )));
    }

    Ok(path)
}

pub async fn validate_repo_destination(
    req: RepoValidateDestinationRequest<'_>,
) -> Result<PathBuf, RepoOnboardingPathError> {
    let raw = req.path.trim();
    if raw.is_empty() {
        return Err(RepoOnboardingPathError::new("path is required"));
    }
    let expanded = expand_tilde(raw)?;
    validate_absolute_path(&expanded, "path")?;

    let metadata = match tokio::fs::metadata(&expanded).await {
        Ok(meta) => Some(meta),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            return Err(RepoOnboardingPathError::new(format!(
                "failed to inspect destination '{}': {err}",
                expanded.display()
            )));
        }
    };

    if let Some(meta) = metadata {
        if !meta.is_dir() {
            return Err(RepoOnboardingPathError::new(format!(
                "destination exists and is not a directory: {}",
                expanded.display()
            )));
        }

        if req.must_not_exist {
            return Err(RepoOnboardingPathError::new(format!(
                "destination already exists: {}",
                expanded.display()
            )));
        }

        if req.require_empty_if_exists && directory_has_entries(&expanded).await? {
            return Err(RepoOnboardingPathError::new(format!(
                "destination is not empty: {}",
                expanded.display()
            )));
        }
    }

    Ok(tokio::fs::canonicalize(&expanded).await.unwrap_or(expanded))
}

pub(super) async fn prepare_clone_destination(
    req: RepoCloneDestinationRequest<'_>,
) -> Result<PathBuf, RepoOnboardingPathError> {
    let dest_parent = expand_tilde(req.dest_parent)?;
    validate_absolute_path(&dest_parent, "dest_parent")?;

    // Allow cloning into a destination parent that doesn't exist yet by creating it.
    // This keeps the wizard UX simple (users can type a new folder path).
    if !dest_parent.exists() {
        tokio::fs::create_dir_all(&dest_parent).await.map_err(|e| {
            RepoOnboardingPathError::new(format!(
                "failed to create dest_parent '{}': {e}",
                dest_parent.to_string_lossy()
            ))
        })?;
    }

    let dest_parent = tokio::fs::canonicalize(&dest_parent).await.map_err(|e| {
        RepoOnboardingPathError::new(format!(
            "invalid dest_parent '{}': {}",
            dest_parent.to_string_lossy(),
            e
        ))
    })?;

    let name = req
        .dest_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| derive_repo_name(req.repo_url))
        .ok_or_else(|| RepoOnboardingPathError::new("could not derive repo name"))?;
    validate_dest_name(&name)?;

    let dest = dest_parent.join(&name);
    if dest.exists() {
        return Err(RepoOnboardingPathError::new(format!(
            "destination already exists: {}",
            dest.display()
        )));
    }

    Ok(dest)
}

async fn directory_has_entries(path: &std::path::Path) -> Result<bool, RepoOnboardingPathError> {
    let mut dir = tokio::fs::read_dir(path).await.map_err(|e| {
        RepoOnboardingPathError::new(format!(
            "failed to read directory '{}': {e}",
            path.display()
        ))
    })?;
    dir.next_entry()
        .await
        .map(|entry| entry.is_some())
        .map_err(|e| {
            RepoOnboardingPathError::new(format!(
                "failed to read directory '{}': {e}",
                path.display()
            ))
        })
}

#[cfg(test)]
#[path = "destination_tests.rs"]
mod tests;
