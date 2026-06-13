use anyhow::{bail, Context, Result};
use ctx_core::models::{Workspace, Worktree};
use ctx_worktree_vcs_service::WorktreeHookExecution;
use std::path::Path;

use super::super::sandbox::sandbox_command;

pub(super) async fn sandbox_git_config_get(
    data_root: &Path,
    workspace: &Workspace,
    worktree: &Worktree,
    execution: &WorktreeHookExecution,
    key: &str,
) -> Result<Option<String>> {
    let mut cmd = sandbox_command(
        data_root,
        workspace,
        worktree,
        execution,
        "git",
        &[
            "config".to_string(),
            "--worktree".to_string(),
            "--get".to_string(),
            key.to_string(),
        ],
    )?;
    let output = cmd
        .output()
        .await
        .context("running sandbox git config --get")?;
    if output.status.success() {
        return Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        ));
    }
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    bail!(
        "sandbox git config --get failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

pub(super) async fn sandbox_git_config_set(
    data_root: &Path,
    workspace: &Workspace,
    worktree: &Worktree,
    execution: &WorktreeHookExecution,
    key: &str,
    value: &str,
) -> Result<()> {
    let mut cmd = sandbox_command(
        data_root,
        workspace,
        worktree,
        execution,
        "git",
        &[
            "config".to_string(),
            "--worktree".to_string(),
            key.to_string(),
            value.to_string(),
        ],
    )?;
    let output = cmd.output().await.context("running sandbox git config")?;
    if !output.status.success() {
        bail!(
            "sandbox git config --worktree failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

pub(super) async fn sandbox_git_config_unset(
    data_root: &Path,
    workspace: &Workspace,
    worktree: &Worktree,
    execution: &WorktreeHookExecution,
    key: &str,
) -> Result<()> {
    let mut cmd = sandbox_command(
        data_root,
        workspace,
        worktree,
        execution,
        "git",
        &[
            "config".to_string(),
            "--worktree".to_string(),
            "--unset-all".to_string(),
            key.to_string(),
        ],
    )?;
    let output = cmd
        .output()
        .await
        .context("running sandbox git config --unset-all")?;
    if output.status.success() || matches!(output.status.code(), Some(1)) {
        return Ok(());
    }
    bail!(
        "sandbox git config --unset-all failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )
}
