use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};

use ctx_core::models::Worktree;
use ctx_worktree_vcs_service::{WorktreeVcsGitCommand, WorktreeVcsSandboxGitExecutor};

use super::{WorktreeVcsExecutionHost, WorktreeVcsSandboxTarget};
use ctx_harness_runtime::sandbox_container_command;

pub(super) struct HttpSandboxWorktreeVcsExecutor<'a> {
    execution: &'a WorktreeVcsExecutionHost,
    worktree: &'a Worktree,
}

impl<'a> HttpSandboxWorktreeVcsExecutor<'a> {
    pub(super) fn new(execution: &'a WorktreeVcsExecutionHost, worktree: &'a Worktree) -> Self {
        Self {
            execution,
            worktree,
        }
    }
}

#[async_trait::async_trait]
impl WorktreeVcsSandboxGitExecutor for HttpSandboxWorktreeVcsExecutor<'_> {
    async fn git_stdout(&self, command: WorktreeVcsGitCommand) -> Result<Vec<u8>> {
        container_git_stdout(self.execution, self.worktree, command).await
    }
}

async fn container_git_output(
    execution: &WorktreeVcsExecutionHost,
    worktree: &Worktree,
    args: &[String],
) -> Result<std::process::Output> {
    const SANDBOX_GIT_TIMEOUT: Duration = Duration::from_secs(30);
    let context = execution.sandbox_context(worktree).await?;
    match context.target {
        WorktreeVcsSandboxTarget::NativeContainer { container_name } => {
            let mut cmd = sandbox_container_command(execution.data_root())?;
            cmd.arg("exec")
                .arg("--workdir")
                .arg(&context.live_worktree_root)
                .arg(&container_name)
                .arg("git")
                .args(args);
            ctx_sandbox_container_runtime::command_output_with_timeout(cmd, SANDBOX_GIT_TIMEOUT)
                .await
                .context("sandbox exec git timed out")
        }
        WorktreeVcsSandboxTarget::SharedVmContainer => {
            let guest_args = args.to_vec();
            tokio::time::timeout(
                SANDBOX_GIT_TIMEOUT,
                ctx_avf_linux_runtime::run_guest_exec_capture(
                    execution.data_root(),
                    worktree.workspace_id,
                    worktree.id,
                    &context.live_worktree_root,
                    "git",
                    &guest_args,
                    &HashMap::new(),
                    None,
                    false,
                ),
            )
            .await
            .context("shared VM container exec git timed out")?
        }
    }
}

pub(super) async fn container_git_stdout(
    execution: &WorktreeVcsExecutionHost,
    worktree: &Worktree,
    command: WorktreeVcsGitCommand,
) -> Result<Vec<u8>> {
    let args = command.args();
    let out = container_git_output(execution, worktree, &args).await?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        anyhow::bail!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
}
