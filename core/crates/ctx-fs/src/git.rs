mod patch;
mod status;

use std::path::Path;
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use tokio::process::Command;

use crate::vcs::{VcsStatusBranchInfo, VcsStatusEntry, VcsStructuredStatus};
pub(crate) use patch::{branch_exists, is_git_worktree};
pub use patch::{
    git_apply_patch, git_apply_patch_allow_noop, git_diff_untracked_file, list_tracked_files,
};
use status::parse_git_diff_name_status_bytes;
pub use status::{
    git_diff_name_status_paths, git_status_structured, git_status_structured_from_bytes,
    git_status_structured_from_bytes_with_entries,
};

#[derive(Debug, Clone, Copy)]
pub enum ApplyPatchTarget {
    Worktree,
    Index,
}

pub async fn assert_git_repo(root_path: impl AsRef<Path>) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root_path.as_ref())
        .args(["rev-parse", "--is-inside-work-tree"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git rev-parse --is-inside-work-tree")?;
    if !output.status.success() {
        bail!(
            "not a git repository: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    if String::from_utf8_lossy(&output.stdout).trim() != "true" {
        bail!("not a git repository");
    }
    Ok(())
}

pub async fn rev_parse_head(root_path: impl AsRef<Path>) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root_path.as_ref())
        .arg("rev-parse")
        .arg("HEAD")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git rev-parse HEAD")?;
    if !output.status.success() {
        bail!(
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub async fn rev_parse_ref(root_path: impl AsRef<Path>, reference: &str) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root_path.as_ref())
        .arg("rev-parse")
        .arg(reference)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .with_context(|| format!("running git rev-parse {reference}"))?;
    if !output.status.success() {
        bail!(
            "git rev-parse {reference} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub async fn git_merge_base(root_path: impl AsRef<Path>, a: &str, b: &str) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root_path.as_ref())
        .arg("merge-base")
        .arg(a)
        .arg(b)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git merge-base")?;
    if !output.status.success() {
        bail!(
            "git merge-base failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub async fn git_ref_exists(root_path: impl AsRef<Path>, reference: &str) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root_path.as_ref())
        .args(["show-ref", "--verify", "--quiet", reference])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git show-ref")?;
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

pub async fn git_default_branch(root_path: impl AsRef<Path>) -> Result<Option<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root_path.as_ref())
        .args(["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git symbolic-ref refs/remotes/origin/HEAD")?;
    if output.status.success() {
        let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if let Some(stripped) = raw.strip_prefix("refs/remotes/") {
            if !stripped.trim().is_empty() {
                return Ok(Some(stripped.to_string()));
            }
        }
        if !raw.trim().is_empty() {
            return Ok(Some(raw));
        }
    }
    let head = Command::new("git")
        .arg("-C")
        .arg(root_path.as_ref())
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git symbolic-ref --short HEAD")?;
    if head.status.success() {
        let raw = String::from_utf8_lossy(&head.stdout).trim().to_string();
        if !raw.trim().is_empty() {
            return Ok(Some(raw));
        }
    }
    let candidates = [
        ("refs/remotes/origin/main", "origin/main"),
        ("refs/remotes/origin/master", "origin/master"),
        ("refs/heads/main", "main"),
        ("refs/heads/master", "master"),
    ];
    for (reference, branch) in candidates {
        if git_ref_exists(root_path.as_ref(), reference)
            .await
            .unwrap_or(false)
        {
            return Ok(Some(branch.to_string()));
        }
    }
    Ok(None)
}

pub async fn git_is_ancestor(
    root_path: impl AsRef<Path>,
    ancestor: &str,
    descendant: &str,
) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root_path.as_ref())
        .arg("merge-base")
        .arg("--is-ancestor")
        .arg(ancestor)
        .arg(descendant)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git merge-base --is-ancestor")?;
    if output.status.success() {
        return Ok(true);
    }
    if output.status.code() == Some(1) {
        return Ok(false);
    }
    bail!(
        "git merge-base --is-ancestor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    )
}

pub async fn delete_branch(root_path: impl AsRef<Path>, branch: &str) -> Result<()> {
    let branch = branch.trim();
    if branch.is_empty() {
        return Ok(());
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(root_path.as_ref())
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git show-ref")?;
    if output.status.success() {
        let output = Command::new("git")
            .arg("-C")
            .arg(root_path.as_ref())
            .args(["branch", "-D", branch])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .context("running git branch -D")?;
        if !output.status.success() {
            bail!(
                "git branch -D failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return Ok(());
    }
    if output.status.code() == Some(1) {
        return Ok(());
    }
    bail!(
        "git show-ref failed: {}",
        String::from_utf8_lossy(&output.stderr)
    )
}

pub async fn git_diff(root_path: impl AsRef<Path>, base_commit_sha: &str) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root_path.as_ref())
        .arg("diff")
        .arg(base_commit_sha)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git diff")?;
    if !output.status.success() {
        bail!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub async fn git_diff_numstat(
    root_path: impl AsRef<Path>,
    base_commit_sha: &str,
) -> Result<Vec<(i64, i64, String)>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root_path.as_ref())
        .arg("diff")
        .arg("--numstat")
        .arg("-z")
        .arg(base_commit_sha)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git diff --numstat")?;
    if !output.status.success() {
        bail!(
            "git diff --numstat failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let bytes = output.stdout;
    let mut out = Vec::new();
    for entry in bytes.split(|b| *b == 0) {
        if entry.is_empty() {
            continue;
        }
        let mut fields = entry.splitn(3, |b| *b == b'\t');
        let add = fields.next().unwrap_or_default();
        let del = fields.next();
        let path = fields.next();
        let (Some(del), Some(path)) = (del, path) else {
            continue;
        };
        let add = String::from_utf8_lossy(add);
        let del = String::from_utf8_lossy(del);
        let path = String::from_utf8_lossy(path).to_string();
        if path.trim().is_empty() {
            continue;
        }
        let add_count = add.parse::<i64>().unwrap_or(0);
        let del_count = del.parse::<i64>().unwrap_or(0);
        out.push((add_count, del_count, path));
    }
    Ok(out)
}

pub async fn git_diff_unstaged(root_path: impl AsRef<Path>) -> Result<String> {
    // Shows worktree changes relative to the index (used for edit review; staging acts as "accept").
    let output = Command::new("git")
        .arg("-C")
        .arg(root_path.as_ref())
        .arg("diff")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git diff (unstaged)")?;
    if !output.status.success() {
        bail!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[derive(Debug, Clone)]
pub struct GitNameStatusEntry {
    pub status: String,
    pub path: String,
    pub orig_path: Option<String>,
}

pub async fn git_diff_name_status(
    root_path: impl AsRef<Path>,
    base_commit_sha: &str,
) -> Result<Vec<GitNameStatusEntry>> {
    git_diff_name_status_inner(root_path, base_commit_sha, false).await
}

pub async fn git_diff_name_status_no_renames(
    root_path: impl AsRef<Path>,
    base_commit_sha: &str,
) -> Result<Vec<GitNameStatusEntry>> {
    git_diff_name_status_inner(root_path, base_commit_sha, true).await
}

async fn git_diff_name_status_inner(
    root_path: impl AsRef<Path>,
    base_commit_sha: &str,
    no_renames: bool,
) -> Result<Vec<GitNameStatusEntry>> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root_path.as_ref()).arg("diff");
    if no_renames {
        cmd.arg("--no-renames");
    }
    let output = cmd
        .arg("--name-status")
        .arg("-z")
        .arg(base_commit_sha)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git diff --name-status")?;
    if !output.status.success() {
        bail!(
            "git diff --name-status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(parse_git_diff_name_status_bytes(&output.stdout)
        .into_iter()
        .map(|(status, path, orig_path)| GitNameStatusEntry {
            status,
            path,
            orig_path,
        })
        .collect())
}

pub async fn git_diff_numstat_unstaged(
    root_path: impl AsRef<Path>,
) -> Result<Vec<(i64, i64, String)>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root_path.as_ref())
        .arg("diff")
        .arg("--numstat")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git diff --numstat")?;
    if !output.status.success() {
        bail!(
            "git diff --numstat failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut out = Vec::new();
    for line in stdout.lines() {
        let mut parts = line.split('\t');
        let add = parts.next().unwrap_or("0");
        let del = parts.next().unwrap_or("0");
        let path = parts.next().unwrap_or("").trim();
        if path.is_empty() {
            continue;
        }
        let add_count = add.parse::<i64>().unwrap_or(0);
        let del_count = del.parse::<i64>().unwrap_or(0);
        out.push((add_count, del_count, path.to_string()));
    }
    Ok(out)
}

pub async fn git_status_short(root_path: impl AsRef<Path>) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root_path.as_ref())
        .arg("status")
        .arg("-sb")
        .arg("--untracked-files=all")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git status -sb")?;
    if !output.status.success() {
        bail!(
            "git status -sb failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub async fn list_untracked_files(root_path: impl AsRef<Path>) -> Result<Vec<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root_path.as_ref())
        .arg("ls-files")
        .arg("--others")
        .arg("--exclude-standard")
        .arg("-z")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git ls-files --others")?;
    if !output.status.success() {
        bail!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let bytes = output.stdout;
    let mut out = Vec::new();
    for part in bytes.split(|b| *b == 0) {
        if part.is_empty() {
            continue;
        }
        out.push(String::from_utf8_lossy(part).to_string());
    }
    Ok(out)
}

pub async fn git_status_porcelain(root_path: impl AsRef<Path>) -> Result<Vec<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root_path.as_ref())
        .arg("status")
        .arg("--porcelain")
        .arg("-z")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git status --porcelain")?;
    if !output.status.success() {
        bail!(
            "git status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let bytes = output.stdout;
    let mut out = Vec::new();
    for entry in bytes.split(|b| *b == 0) {
        if entry.is_empty() {
            continue;
        }
        out.push(String::from_utf8_lossy(entry).to_string());
    }
    Ok(out)
}
