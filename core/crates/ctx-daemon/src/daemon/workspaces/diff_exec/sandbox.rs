use std::time::Duration;

use anyhow::Context;
use ctx_core::models::Worktree;
use ctx_harness_runtime::sandbox_container_command;
use ctx_sandbox_container_runtime::command_output_with_timeout;
use ctx_worktree_vcs_service::{
    load_worktree_vcs_session_diff_from_sandbox,
    load_worktree_vcs_session_diff_summary_from_sandbox, WorktreeVcsDiffSummaryCounts,
    WorktreeVcsSessionDiffCommand, WorktreeVcsSessionDiffSandboxExecutor,
};

use crate::daemon::git_status::{WorktreeVcsExecutionHost, WorktreeVcsSandboxTarget};

struct HttpSandboxSessionDiffExecutor<'a> {
    execution: &'a WorktreeVcsExecutionHost,
    worktree: &'a Worktree,
}

#[async_trait::async_trait]
impl WorktreeVcsSessionDiffSandboxExecutor for HttpSandboxSessionDiffExecutor<'_> {
    async fn stdout(&self, command: WorktreeVcsSessionDiffCommand) -> anyhow::Result<Vec<u8>> {
        container_exec_stdout(
            self.execution,
            self.worktree,
            command.program(),
            command.args(),
        )
        .await
    }
}

async fn container_exec_stdout(
    execution: &WorktreeVcsExecutionHost,
    worktree: &Worktree,
    program: &str,
    args: &[String],
) -> anyhow::Result<Vec<u8>> {
    const SANDBOX_EXEC_TIMEOUT: Duration = Duration::from_secs(30);
    let context = execution.sandbox_context(worktree).await?;
    let out = match context.target {
        WorktreeVcsSandboxTarget::NativeContainer { container_name } => {
            let mut cmd = sandbox_container_command(execution.data_root())?;
            cmd.arg("exec")
                .arg("--workdir")
                .arg(&context.live_worktree_root)
                .arg(&container_name)
                .arg(program)
                .args(args);
            command_output_with_timeout(cmd, SANDBOX_EXEC_TIMEOUT)
                .await
                .context("sandbox exec command timed out")?
        }
        WorktreeVcsSandboxTarget::SharedVmContainer => {
            ctx_avf_linux_runtime::run_guest_exec_capture(
                execution.data_root(),
                worktree.workspace_id,
                worktree.id,
                &context.live_worktree_root,
                program,
                args,
                &std::collections::HashMap::new(),
                None,
                false,
            )
            .await
            .context("shared VM container exec command failed")?
        }
    };
    if out.status.success() {
        Ok(out.stdout)
    } else {
        anyhow::bail!("{} {:?} failed: {}", program, args, {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !stderr.is_empty() {
                stderr
            } else if !stdout.is_empty() {
                stdout
            } else {
                "unknown sandbox exec failure".to_string()
            }
        });
    }
}

pub(super) async fn container_diff_worktree(
    execution: &WorktreeVcsExecutionHost,
    worktree: &Worktree,
    base_commit_sha: &str,
) -> anyhow::Result<String> {
    let executor = HttpSandboxSessionDiffExecutor {
        execution,
        worktree,
    };
    load_worktree_vcs_session_diff_from_sandbox(&executor, base_commit_sha).await
}

pub(super) async fn container_diff_worktree_summary(
    execution: &WorktreeVcsExecutionHost,
    worktree: &Worktree,
    base_commit_sha: &str,
) -> anyhow::Result<WorktreeVcsDiffSummaryCounts> {
    let executor = HttpSandboxSessionDiffExecutor {
        execution,
        worktree,
    };
    load_worktree_vcs_session_diff_summary_from_sandbox(&executor, base_commit_sha).await
}
