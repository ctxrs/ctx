use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ctx_core::ids::{WorkspaceId, WorktreeId};

use crate::vcs;

pub fn worktrees_root(data_root: impl AsRef<Path>) -> PathBuf {
    data_root.as_ref().join("worktrees")
}

pub fn managed_worktree_path(
    data_root: impl AsRef<Path>,
    workspace_id: WorkspaceId,
    worktree_id: WorktreeId,
) -> PathBuf {
    worktrees_root(data_root)
        .join(workspace_id.0.to_string())
        .join(worktree_id.0.to_string())
}

pub async fn create_worktree(
    workspace_root: impl AsRef<Path>,
    worktree_path: impl AsRef<Path>,
    base_commit_sha: &str,
    branch_name: &str,
) -> Result<()> {
    let driver = vcs::driver_for_path(workspace_root.as_ref()).await?;
    driver
        .create_worktree(
            workspace_root.as_ref(),
            worktree_path.as_ref(),
            base_commit_sha,
            branch_name,
        )
        .await
}

pub async fn standaloneize_worktree_git_dir(worktree_path: impl AsRef<Path>) -> Result<()> {
    let worktree_path = worktree_path.as_ref();
    let dotgit = worktree_path.join(".git");
    let meta = tokio::fs::metadata(&dotgit)
        .await
        .with_context(|| format!("reading {}", dotgit.display()))?;
    if meta.is_dir() {
        return Ok(());
    }

    let git_dir = resolve_git_dir(worktree_path).await?;
    let common_git_dir = resolve_common_git_dir(&git_dir).await?;
    let staged_dotgit = worktree_path.join(format!(
        ".git.ctx-standalone-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default()
    ));
    let common_git_dir_copy = common_git_dir.clone();
    let git_dir_copy = git_dir.clone();
    let staged_dotgit_copy = staged_dotgit.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        copy_dir_recursive(&common_git_dir_copy, &staged_dotgit_copy)?;
        if git_dir_copy != common_git_dir_copy {
            copy_dir_recursive(&git_dir_copy, &staged_dotgit_copy)?;
        }
        let commondir = staged_dotgit_copy.join("commondir");
        if commondir.exists() {
            std::fs::remove_file(&commondir)?;
        }
        let gitdir = staged_dotgit_copy.join("gitdir");
        if gitdir.exists() {
            std::fs::remove_file(&gitdir)?;
        }
        Ok(())
    })
    .await??;
    tokio::fs::remove_file(&dotgit)
        .await
        .with_context(|| format!("removing {}", dotgit.display()))?;
    tokio::fs::rename(&staged_dotgit, &dotgit)
        .await
        .with_context(|| {
            format!(
                "renaming {} to {}",
                staged_dotgit.display(),
                dotgit.display()
            )
        })?;
    Ok(())
}

pub async fn remove_worktree(
    workspace_root: impl AsRef<Path>,
    worktree_path: impl AsRef<Path>,
) -> Result<()> {
    let driver = vcs::driver_for_path(workspace_root.as_ref()).await?;
    driver
        .remove_worktree(workspace_root.as_ref(), worktree_path.as_ref())
        .await?;
    if tokio::fs::metadata(worktree_path.as_ref()).await.is_ok() {
        tokio::fs::remove_dir_all(worktree_path.as_ref())
            .await
            .context("removing worktree dir")?;
    }
    Ok(())
}

pub async fn prune_worktrees(workspace_root: impl AsRef<Path>) -> Result<()> {
    let driver = vcs::driver_for_path(workspace_root.as_ref()).await?;
    driver.prune_worktrees(workspace_root.as_ref()).await
}

pub async fn ensure_worktree_attached(
    workspace_root: impl AsRef<Path>,
    worktree_path: impl AsRef<Path>,
    base_commit_sha: &str,
    branch_name: &str,
) -> Result<()> {
    let driver = vcs::driver_for_path(workspace_root.as_ref()).await?;
    let worktree_path = worktree_path.as_ref();
    if let Some(parent) = worktree_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .context("creating worktree parent dir")?;
    }

    if tokio::fs::metadata(worktree_path).await.is_ok() {
        if driver.is_worktree(worktree_path).await.unwrap_or(false) {
            return Ok(());
        }
        tokio::fs::remove_dir_all(worktree_path)
            .await
            .context("removing stale worktree dir")?;
    }

    // Prune stale registrations before recreating. The worktree path may still be
    // registered in git's worktree metadata from a previous incomplete teardown (e.g.,
    // the directory was removed without running `git worktree remove`). Without pruning
    // first, `git worktree add` would fail with "branch already checked out".
    prune_worktrees(workspace_root.as_ref())
        .await
        .context("pruning stale worktree registrations before create")?;

    driver
        .create_worktree(
            workspace_root.as_ref(),
            worktree_path,
            base_commit_sha,
            branch_name,
        )
        .await
}

pub async fn diff_worktree(
    worktree_path: impl AsRef<Path>,
    base_commit_sha: &str,
) -> Result<String> {
    let root = worktree_path.as_ref();
    let driver = vcs::driver_for_path(root).await?;
    let mut out = driver.diff(root, base_commit_sha).await?;

    // Base diffs do not include untracked files, but we want the UI to show newly created
    // files even before they are staged.
    let untracked = driver.list_untracked(root).await.unwrap_or_default();
    for rel in untracked {
        // Avoid dumping huge blobs into the diff view.
        let abs = root.join(&rel);
        if let Ok(meta) = tokio::fs::metadata(&abs).await {
            const MAX_UNTRACKED_BYTES: u64 = 512 * 1024;
            if meta.len() > MAX_UNTRACKED_BYTES {
                out.push_str(&format!(
                    "\n# untracked: {} ({} bytes; omitted)\n",
                    rel,
                    meta.len()
                ));
                continue;
            }
        }

        if let Ok(patch) = driver.diff_untracked_file(root, &rel).await {
            if !patch.trim().is_empty() {
                out.push('\n');
                out.push_str(&patch);
            }
        }
    }

    Ok(out)
}

pub async fn diff_worktree_summary(
    worktree_path: impl AsRef<Path>,
    base_commit_sha: &str,
) -> Result<(i64, i64, i64)> {
    let root = worktree_path.as_ref();
    let driver = vcs::driver_for_path(root).await?;
    let (mut file_count, mut additions, deletions) =
        driver.diff_summary(root, base_commit_sha).await?;

    let untracked = driver.list_untracked(root).await.unwrap_or_default();
    if !untracked.is_empty() {
        for rel in untracked {
            file_count += 1;
            let abs = root.join(&rel);
            if let Ok(meta) = tokio::fs::metadata(&abs).await {
                const MAX_UNTRACKED_BYTES: u64 = 512 * 1024;
                if meta.len() > MAX_UNTRACKED_BYTES {
                    continue;
                }
            }
            if let Ok(bytes) = tokio::fs::read(&abs).await {
                let mut line_count = bytes.iter().filter(|b| **b == b'\n').count() as i64;
                if !bytes.is_empty() && !bytes.ends_with(b"\n") {
                    line_count += 1;
                }
                additions += line_count;
            }
        }
    }

    Ok((file_count, additions, deletions))
}

async fn resolve_git_dir(worktree_root: &Path) -> Result<PathBuf> {
    let dotgit = worktree_root.join(".git");
    let meta = tokio::fs::metadata(&dotgit)
        .await
        .with_context(|| format!("reading {}", dotgit.display()))?;
    if meta.is_dir() {
        return Ok(dotgit);
    }
    let txt = tokio::fs::read_to_string(&dotgit)
        .await
        .with_context(|| format!("reading {}", dotgit.display()))?;
    let line = txt
        .lines()
        .find(|value| value.trim_start().starts_with("gitdir:"))
        .ok_or_else(|| anyhow::anyhow!("invalid .git file: missing gitdir"))?;
    let raw = line.trim_start().trim_start_matches("gitdir:").trim();
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(worktree_root.join(path))
    }
}

async fn resolve_common_git_dir(git_dir: &Path) -> Result<PathBuf> {
    let commondir = git_dir.join("commondir");
    let meta = match tokio::fs::symlink_metadata(&commondir).await {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(git_dir.to_path_buf());
        }
        Err(err) => return Err(err).with_context(|| format!("reading {}", commondir.display())),
    };
    if !meta.is_file() {
        return Ok(git_dir.to_path_buf());
    }
    let raw = tokio::fs::read_to_string(&commondir)
        .await
        .with_context(|| format!("reading {}", commondir.display()))?;
    let path = PathBuf::from(raw.trim());
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(git_dir.join(path))
    }
}

#[cfg(unix)]
fn symlink_path(target: &Path, dest: &Path, _is_dir: bool) -> Result<()> {
    std::os::unix::fs::symlink(target, dest)?;
    Ok(())
}

#[cfg(windows)]
fn symlink_path(target: &Path, dest: &Path, is_dir: bool) -> Result<()> {
    if is_dir {
        std::os::windows::fs::symlink_dir(target, dest)?;
    } else {
        std::os::windows::fs::symlink_file(target, dest)?;
    }
    Ok(())
}

fn copy_dir_recursive(source: &Path, target: &Path) -> Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let entry_path = entry.path();
        let dest = target.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry_path, &dest)?;
        } else if file_type.is_symlink() {
            if dest.exists() {
                let _ = std::fs::remove_file(&dest);
                let _ = std::fs::remove_dir_all(&dest);
            }
            let link_target = std::fs::read_link(&entry_path)?;
            let is_dir = std::fs::metadata(&entry_path)
                .map(|meta| meta.is_dir())
                .unwrap_or(false);
            symlink_path(&link_target, &dest, is_dir)?;
        } else if file_type.is_file() {
            std::fs::copy(&entry_path, &dest)?;
        }
    }
    Ok(())
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

    fn git_output(args: &[&str], cwd: &Path) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("run git output");
        assert!(out.status.success(), "git {args:?} failed");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn init_repo(root: &Path) -> String {
        git(&["init"], root);
        git(&["symbolic-ref", "HEAD", "refs/heads/main"], root);
        git(&["config", "user.email", "test@example.com"], root);
        git(&["config", "user.name", "Test"], root);
        std::fs::write(root.join("README.md"), "hello\n").expect("write readme");
        git(&["add", "README.md"], root);
        git(&["commit", "-m", "initial"], root);
        git_output(&["rev-parse", "HEAD"], root)
    }

    /// Regression: ensure_worktree_attached must succeed even when the worktree path
    /// was removed without calling `git worktree remove` (leaving a stale registration).
    /// Without pruning first, `git worktree add` would fail with "branch already
    /// checked out at <path>".
    #[tokio::test]
    async fn ensure_worktree_attached_prunes_stale_registration_before_recreate() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo_root = temp.path().join("repo");
        std::fs::create_dir_all(&repo_root).expect("create repo root");
        let base_commit = init_repo(&repo_root);

        let worktree_path = temp.path().join("worktree");
        git(
            &[
                "worktree",
                "add",
                "-b",
                "ctx/test-stale",
                worktree_path.to_str().expect("worktree path"),
                &base_commit,
            ],
            &repo_root,
        );

        // Remove the worktree directory without running `git worktree remove`.
        // This leaves a stale registration in the git repo metadata.
        tokio::fs::remove_dir_all(&worktree_path)
            .await
            .expect("remove worktree dir");

        // Verify the stale registration is present before the fix.
        let list = git_output(&["worktree", "list", "--porcelain"], &repo_root);
        assert!(
            list.contains(worktree_path.to_string_lossy().as_ref()),
            "stale worktree registration should exist before ensure_worktree_attached"
        );

        // Calling ensure_worktree_attached should prune the stale entry and recreate
        // the worktree without a "branch already checked out" error.
        ensure_worktree_attached(&repo_root, &worktree_path, &base_commit, "ctx/test-stale")
            .await
            .expect("ensure_worktree_attached should succeed despite stale registration");

        assert!(
            worktree_path.exists(),
            "worktree directory should be recreated"
        );
        let list_after = git_output(&["worktree", "list", "--porcelain"], &repo_root);
        assert!(
            list_after.contains(worktree_path.to_string_lossy().as_ref()),
            "recreated worktree should appear in git worktree list"
        );
    }
}
