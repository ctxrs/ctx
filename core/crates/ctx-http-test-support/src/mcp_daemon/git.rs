use std::path::Path;

use anyhow::{bail, Context, Result};
use tempfile::TempDir;
use tokio::process::Command;

pub(crate) async fn init_git_repo() -> Result<TempDir> {
    let dir = tempfile::tempdir().context("create git repo tempdir")?;
    let root = dir.path();
    run_git(root, &["init"]).await?;
    run_git(root, &["config", "user.email", "test@example.com"]).await?;
    run_git(root, &["config", "user.name", "Test"]).await?;
    tokio::fs::write(root.join("README.md"), "ok\n")
        .await
        .context("write README")?;
    run_git(root, &["add", "."]).await?;
    run_git(root, &["commit", "-m", "init"]).await?;
    Ok(dir)
}

pub(crate) async fn run_git_output(root: &Path, args: &[&str]) -> Result<String> {
    let output = run_git_command(root, args).await?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

async fn run_git(root: &Path, args: &[&str]) -> Result<()> {
    run_git_command(root, args).await.map(|_| ())
}

async fn run_git_command(root: &Path, args: &[&str]) -> Result<std::process::Output> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .await
        .with_context(|| format!("run git {args:?}"))?;
    if !output.status.success() {
        bail!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output)
}
