use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{bail, Context};
use chrono::{DateTime, Utc};
use ctx_core::ids::{WorkspaceId, WorktreeId};
use ctx_core::models::{VcsKind, Worktree};
use tokio::process::Command;

pub fn managed_worktree_path(
    data_root: impl AsRef<Path>,
    workspace_id: WorkspaceId,
    worktree_id: WorktreeId,
) -> PathBuf {
    ctx_fs::worktrees::managed_worktree_path(data_root, workspace_id, worktree_id)
}

pub fn matching_managed_worktree_path(
    data_root: impl AsRef<Path>,
    workspace_id: WorkspaceId,
    worktree_id: WorktreeId,
    worktree_root: impl AsRef<Path>,
) -> Option<PathBuf> {
    let expected = managed_worktree_path(data_root, workspace_id, worktree_id);
    if normalize_path_for_comparison(worktree_root.as_ref())
        == normalize_path_for_comparison(&expected)
    {
        Some(expected)
    } else {
        None
    }
}

pub async fn create_managed_worktree(
    data_root: impl AsRef<Path>,
    workspace_root: impl AsRef<Path>,
    workspace_id: WorkspaceId,
    worktree_id: WorktreeId,
    base_commit_sha: &str,
    branch_name: &str,
) -> anyhow::Result<PathBuf> {
    let canonical_root = managed_worktree_path(data_root, workspace_id, worktree_id);
    if let Some(parent) = canonical_root.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    ctx_fs::worktrees::create_worktree(
        workspace_root,
        &canonical_root,
        base_commit_sha,
        branch_name,
    )
    .await?;
    Ok(canonical_root)
}

pub fn managed_worktree_record(
    workspace_id: WorkspaceId,
    worktree_id: WorktreeId,
    root_path: impl AsRef<Path>,
    base_commit_sha: &str,
    branch_name: &str,
    created_at: DateTime<Utc>,
) -> Worktree {
    Worktree {
        id: worktree_id,
        workspace_id,
        root_path: root_path.as_ref().to_string_lossy().to_string(),
        base_commit_sha: base_commit_sha.to_string(),
        git_branch: Some(branch_name.to_string()),
        vcs_kind: Some(VcsKind::Git),
        base_revision: Some(base_commit_sha.to_string()),
        vcs_ref: Some(branch_name.to_string()),
        created_at,
        bootstrap_status: None,
        bootstrap_started_at: None,
        bootstrap_finished_at: None,
        bootstrap_exit_code: None,
        bootstrap_timeout_sec: None,
        bootstrap_error: None,
        bootstrap_log_path: None,
        bootstrap_log_truncated: None,
        bootstrap_command: None,
        bootstrap_script_path: None,
    }
}

pub async fn branch_exists(
    workspace_root: impl AsRef<Path>,
    branch_name: &str,
) -> anyhow::Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_root.as_ref())
        .arg("show-ref")
        .arg("--verify")
        .arg("--quiet")
        .arg(format!("refs/heads/{branch_name}"))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git show-ref --verify")?;
    if output.status.success() {
        return Ok(true);
    }
    if output.status.code() == Some(1) {
        return Ok(false);
    }
    bail!(
        "git show-ref failed: {}",
        String::from_utf8_lossy(&output.stderr)
    )
}

pub async fn is_git_worktree(worktree_path: impl AsRef<Path>) -> anyhow::Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree_path.as_ref())
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git rev-parse --is-inside-work-tree")?;
    if !output.status.success() {
        return Ok(false);
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim() == "true")
}

pub async fn remove_worktree(
    workspace_root: impl AsRef<Path>,
    worktree_path: impl AsRef<Path>,
) -> anyhow::Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_root.as_ref())
        .arg("worktree")
        .arg("remove")
        .arg("--force")
        .arg(worktree_path.as_ref())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git worktree remove")?;
    if !output.status.success() {
        bail!(
            "git worktree remove failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    if tokio::fs::metadata(worktree_path.as_ref()).await.is_ok() {
        tokio::fs::remove_dir_all(worktree_path.as_ref())
            .await
            .context("removing worktree dir")?;
    }
    Ok(())
}

pub async fn delete_worktree_branch(
    workspace_root: impl AsRef<Path>,
    branch_name: &str,
) -> anyhow::Result<()> {
    ctx_fs::git::delete_branch(workspace_root, branch_name).await
}

pub async fn standaloneize_worktree_git_dir(worktree_path: impl AsRef<Path>) -> anyhow::Result<()> {
    ctx_fs::worktrees::standaloneize_worktree_git_dir(worktree_path).await
}

fn normalize_path_for_comparison(path: &Path) -> PathBuf {
    let mut suffix = Vec::new();
    let mut cursor = path;
    loop {
        match std::fs::canonicalize(cursor) {
            Ok(canonical) => {
                let mut normalized = canonical;
                for component in suffix.iter().rev() {
                    normalized.push(component);
                }
                return normalized;
            }
            Err(_) => {
                let Some(parent) = cursor.parent() else {
                    return path.to_path_buf();
                };
                let Some(name) = cursor.file_name() else {
                    return path.to_path_buf();
                };
                suffix.push(name.to_os_string());
                cursor = parent;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(args: &[&str], cwd: &Path) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn matching_managed_worktree_path_accepts_equivalent_existing_parent() {
        let data_root = tempfile::tempdir().expect("data root");
        let workspace_id = WorkspaceId::new();
        let worktree_id = WorktreeId::new();
        let expected = managed_worktree_path(data_root.path(), workspace_id, worktree_id);
        std::fs::create_dir_all(expected.parent().expect("parent")).expect("create parent");

        let matched = matching_managed_worktree_path(
            data_root.path(),
            workspace_id,
            worktree_id,
            expected.as_path(),
        )
        .expect("managed path");

        assert_eq!(matched, expected);
    }

    #[test]
    fn matching_managed_worktree_path_rejects_external_root() {
        let data_root = tempfile::tempdir().expect("data root");
        let external_root = tempfile::tempdir().expect("external root");

        assert!(matching_managed_worktree_path(
            data_root.path(),
            WorkspaceId::new(),
            WorktreeId::new(),
            external_root.path(),
        )
        .is_none());
    }

    #[test]
    fn managed_worktree_record_sets_git_metadata_and_bootstrap_defaults() {
        let workspace_id = WorkspaceId::new();
        let worktree_id = WorktreeId::new();
        let created_at = DateTime::parse_from_rfc3339("2026-05-12T00:00:00Z")
            .expect("timestamp")
            .with_timezone(&Utc);

        let record = managed_worktree_record(
            workspace_id,
            worktree_id,
            "/tmp/ctx/worktree",
            "abc123",
            "ctx/task",
            created_at,
        );

        assert_eq!(record.id, worktree_id);
        assert_eq!(record.workspace_id, workspace_id);
        assert_eq!(record.root_path, "/tmp/ctx/worktree");
        assert_eq!(record.base_commit_sha, "abc123");
        assert_eq!(record.git_branch.as_deref(), Some("ctx/task"));
        assert_eq!(record.vcs_kind, Some(VcsKind::Git));
        assert_eq!(record.base_revision.as_deref(), Some("abc123"));
        assert_eq!(record.vcs_ref.as_deref(), Some("ctx/task"));
        assert_eq!(record.created_at, created_at);
        assert!(record.bootstrap_status.is_none());
        assert!(record.bootstrap_command.is_none());
    }

    #[tokio::test]
    async fn delete_worktree_branch_removes_existing_branch() {
        let repo = tempfile::tempdir().expect("repo");
        git(&["init"], repo.path());
        git(&["symbolic-ref", "HEAD", "refs/heads/main"], repo.path());
        git(&["config", "user.email", "ctx@example.com"], repo.path());
        git(&["config", "user.name", "Ctx Test"], repo.path());
        std::fs::write(repo.path().join("README.md"), "hello\n").expect("write readme");
        git(&["add", "README.md"], repo.path());
        git(&["commit", "-m", "initial"], repo.path());
        git(&["branch", "stale"], repo.path());
        assert!(branch_exists(repo.path(), "stale")
            .await
            .expect("branch exists"));

        delete_worktree_branch(repo.path(), "stale")
            .await
            .expect("delete branch");

        assert!(!branch_exists(repo.path(), "stale")
            .await
            .expect("branch removed"));
    }
}

pub async fn prune_worktrees(workspace_root: impl AsRef<Path>) -> anyhow::Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_root.as_ref())
        .arg("worktree")
        .arg("prune")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git worktree prune")?;
    if !output.status.success() {
        bail!(
            "git worktree prune failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

pub async fn ensure_worktree_attached(
    workspace_root: impl AsRef<Path>,
    worktree_path: impl AsRef<Path>,
    base_commit_sha: &str,
    branch_name: &str,
) -> anyhow::Result<()> {
    let workspace_root = workspace_root.as_ref();
    let worktree_path = worktree_path.as_ref();
    if let Some(parent) = worktree_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .context("creating worktree parent dir")?;
    }

    let mut prune_stale_registration = false;
    match tokio::fs::metadata(worktree_path).await {
        Ok(metadata) => {
            if is_git_worktree(worktree_path).await.unwrap_or(false) {
                return Ok(());
            }
            if metadata.is_dir() {
                tokio::fs::remove_dir_all(worktree_path)
                    .await
                    .context("removing stale worktree dir")?;
            } else {
                tokio::fs::remove_file(worktree_path)
                    .await
                    .context("removing stale worktree file")?;
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            prune_stale_registration = true;
        }
        Err(err) => {
            return Err(err).context("reading managed worktree root metadata");
        }
    }

    if prune_stale_registration {
        prune_worktrees(workspace_root)
            .await
            .context("pruning stale managed worktree registrations")?;
    }

    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(workspace_root)
        .arg("worktree")
        .arg("add")
        .arg(worktree_path);
    if branch_exists(workspace_root, branch_name).await? {
        cmd.arg(branch_name);
    } else {
        cmd.arg("-b").arg(branch_name).arg(base_commit_sha);
    }
    let output = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git worktree add")?;
    if !output.status.success() {
        bail!(
            "git worktree add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}
