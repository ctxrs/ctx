use std::path::{Path, PathBuf};

use super::destination::RepoOnboardingPathError;

pub async fn create_repo_staging_path(
    data_root: &Path,
) -> Result<PathBuf, RepoOnboardingPathError> {
    let staging_dir = data_root
        .join("workspaces")
        .join("staging")
        .join(uuid::Uuid::new_v4().to_string());

    tokio::fs::create_dir_all(&staging_dir).await.map_err(|e| {
        RepoOnboardingPathError::new(format!(
            "failed to create staging dir '{}': {e}",
            staging_dir.display()
        ))
    })?;

    Ok(tokio::fs::canonicalize(&staging_dir)
        .await
        .unwrap_or(staging_dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_repo_staging_path_uses_data_root_workspace_staging_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let staging = create_repo_staging_path(tmp.path())
            .await
            .expect("staging path");
        let expected_root = tmp
            .path()
            .canonicalize()
            .expect("canonical tempdir")
            .join("workspaces")
            .join("staging");

        assert!(staging.exists());
        assert!(staging.starts_with(expected_root));
    }
}
