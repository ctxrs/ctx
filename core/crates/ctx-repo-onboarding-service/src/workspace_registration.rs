use std::path::{Path, PathBuf};

use anyhow::Context;
use ctx_core::models::VcsKind;
use ctx_fs::git::git_default_branch;
use ctx_fs::vcs;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct WorkspaceRegistrationCandidate {
    pub root_path: PathBuf,
    pub vcs_kind: VcsKind,
    pub primary_branch: String,
    pub default_name: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct WorkspaceRegistrationError {
    message: String,
}

impl WorkspaceRegistrationError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

pub async fn prepare_workspace_registration(
    root_path: &str,
) -> Result<WorkspaceRegistrationCandidate, WorkspaceRegistrationError> {
    let expanded = expand_workspace_root(root_path)?;
    let root_path = tokio::fs::canonicalize(&expanded).await.map_err(|error| {
        WorkspaceRegistrationError::new(format!(
            "invalid root_path '{}': {}",
            expanded.to_string_lossy(),
            error
        ))
    })?;

    let driver = vcs::driver_for_path(&root_path)
        .await
        .map_err(|error| WorkspaceRegistrationError::new(error.to_string()))?;
    driver
        .assert_repo(&root_path)
        .await
        .map_err(|error| WorkspaceRegistrationError::new(error.to_string()))?;

    let vcs_kind = driver.kind();
    let primary_branch =
        detect_workspace_primary_branch(vcs_kind.clone(), &root_path, driver.as_ref())
            .await
            .map_err(|error| WorkspaceRegistrationError::new(error.to_string()))?;
    let default_name = root_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace")
        .to_string();

    Ok(WorkspaceRegistrationCandidate {
        root_path,
        vcs_kind,
        primary_branch,
        default_name,
    })
}

pub async fn validate_workspace_root_repo(root_path: &Path) -> anyhow::Result<VcsKind> {
    let driver = vcs::driver_for_path(root_path).await?;
    driver.assert_repo(root_path).await?;
    Ok(driver.kind())
}

pub async fn validate_workspace_primary_branch(
    root_path: &Path,
    primary_branch: &str,
) -> Result<String, WorkspaceRegistrationError> {
    let primary_branch = primary_branch.trim();
    if primary_branch.is_empty() {
        return Err(WorkspaceRegistrationError::new(
            "primary_branch is required",
        ));
    }

    let driver = vcs::driver_for_path(root_path)
        .await
        .map_err(|error| WorkspaceRegistrationError::new(error.to_string()))?;
    driver
        .rev_parse_ref(root_path, primary_branch)
        .await
        .map_err(|error| {
            WorkspaceRegistrationError::new(format!(
                "primary_branch `{primary_branch}` does not resolve: {error}"
            ))
        })?;
    Ok(primary_branch.to_string())
}

fn expand_workspace_root(raw_root_path: &str) -> Result<PathBuf, WorkspaceRegistrationError> {
    let raw = raw_root_path.trim();
    if raw.is_empty() {
        return Err(WorkspaceRegistrationError::new("root_path is required"));
    }
    if raw == "~" || raw.starts_with("~/") {
        let base = directories::BaseDirs::new().ok_or_else(|| {
            WorkspaceRegistrationError::new("could not resolve home directory to expand '~'")
        })?;
        let home = base.home_dir();
        if raw == "~" {
            return Ok(home.to_path_buf());
        }
        return Ok(home.join(raw.trim_start_matches("~/")));
    }
    Ok(PathBuf::from(raw))
}

pub async fn detect_workspace_primary_branch(
    vcs_kind: VcsKind,
    root_path: &Path,
    driver: &dyn vcs::VcsDriver,
) -> anyhow::Result<String> {
    match vcs_kind {
        VcsKind::Git => {
            let branch = git_default_branch(root_path)
                .await?
                .ok_or_else(|| anyhow::anyhow!("unable to detect default git branch"))?;
            normalize_detected_primary_branch(&branch)
        }
        VcsKind::Jj => {
            driver
                .rev_parse_ref(root_path, "main")
                .await
                .context("resolving jj primary bookmark `main`")?;
            Ok("main".to_string())
        }
        _ => anyhow::bail!("primary branch detection is only supported for git and jj workspaces"),
    }
}

fn normalize_detected_primary_branch(branch: &str) -> anyhow::Result<String> {
    let trimmed = branch.trim().to_string();
    if trimmed.is_empty() {
        anyhow::bail!("detected default git branch is empty");
    }
    Ok(trimmed)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        expand_workspace_root, normalize_detected_primary_branch, validate_workspace_primary_branch,
    };

    #[test]
    fn normalize_detected_primary_branch_trims_git_output() {
        let branch = normalize_detected_primary_branch(" main \n").expect("branch");
        assert_eq!(branch, "main");
    }

    #[test]
    fn normalize_detected_primary_branch_rejects_empty_output() {
        let error = normalize_detected_primary_branch(" \n").expect_err("empty branch");
        assert_eq!(error.to_string(), "detected default git branch is empty");
    }

    #[test]
    fn expand_workspace_root_rejects_empty_input() {
        let error = expand_workspace_root(" \n").expect_err("empty root");
        assert_eq!(error.message(), "root_path is required");
    }

    #[tokio::test]
    async fn validate_workspace_primary_branch_rejects_empty_branch() {
        let error = validate_workspace_primary_branch(Path::new("/tmp"), " \n")
            .await
            .expect_err("empty branch");
        assert_eq!(error.message(), "primary_branch is required");
    }
}
