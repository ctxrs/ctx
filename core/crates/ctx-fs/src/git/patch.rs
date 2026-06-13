use std::path::Path;
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use tokio::process::Command;

use super::ApplyPatchTarget;

pub(crate) async fn branch_exists(
    workspace_root: impl AsRef<Path>,
    branch_name: &str,
) -> Result<bool> {
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

pub(crate) async fn is_git_worktree(worktree_path: impl AsRef<Path>) -> Result<bool> {
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

pub async fn list_tracked_files(root_path: impl AsRef<Path>) -> Result<Vec<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root_path.as_ref())
        .arg("ls-files")
        .arg("-z")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git ls-files")?;
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

pub async fn git_diff_untracked_file(
    root_path: impl AsRef<Path>,
    rel_path: &str,
) -> Result<String> {
    // `git diff --no-index` uses exit code 1 to indicate differences; treat 0/1 as success.
    let output = Command::new("git")
        .arg("-C")
        .arg(root_path.as_ref())
        .arg("diff")
        .arg("--no-index")
        .arg("--")
        .arg("/dev/null")
        .arg(rel_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git diff --no-index for untracked file")?;

    if !output.status.success() && output.status.code() != Some(1) {
        bail!(
            "git diff --no-index failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub async fn git_apply_patch(
    root_path: impl AsRef<Path>,
    patch: &str,
    target: ApplyPatchTarget,
    reverse: bool,
) -> Result<()> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root_path.as_ref()).arg("apply");
    if matches!(target, ApplyPatchTarget::Index) {
        cmd.arg("--cached");
    }
    if reverse {
        cmd.arg("--reverse");
    }
    cmd.arg("--whitespace=nowarn").arg("-");

    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning git apply")?;

    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin
            .write_all(patch.as_bytes())
            .await
            .context("writing patch to git apply stdin")?;
    }

    let output = child
        .wait_with_output()
        .await
        .context("waiting for git apply")?;
    if !output.status.success() {
        bail!(
            "git apply failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

pub async fn git_apply_patch_allow_noop(
    root_path: impl AsRef<Path>,
    patch: &str,
    target: ApplyPatchTarget,
    reverse: bool,
) -> Result<()> {
    match git_apply_patch(root_path, patch, target, reverse).await {
        Ok(()) => Ok(()),
        Err(e) => {
            // Best-effort for index updates: it's fine if nothing was staged.
            let msg = e.to_string().to_lowercase();
            if msg.contains("patch does not apply") || msg.contains("did not match any files") {
                return Ok(());
            }
            Err(e)
        }
    }
}
