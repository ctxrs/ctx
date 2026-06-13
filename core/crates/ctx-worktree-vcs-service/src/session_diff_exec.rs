use std::path::Path;

use anyhow::Result;

use super::{
    parse_worktree_vcs_diff_summary_counts, WorktreeVcsDiffSummaryCounts,
    WORKTREE_VCS_CONTAINER_DIFF_SCRIPT, WORKTREE_VCS_CONTAINER_DIFF_SUMMARY_SCRIPT,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeVcsSessionDiffCommand {
    program: String,
    args: Vec<String>,
}

impl WorktreeVcsSessionDiffCommand {
    pub fn program(&self) -> &str {
        &self.program
    }

    pub fn args(&self) -> &[String] {
        &self.args
    }
}

#[async_trait::async_trait]
pub trait WorktreeVcsSessionDiffSandboxExecutor: Send + Sync {
    async fn stdout(&self, command: WorktreeVcsSessionDiffCommand) -> Result<Vec<u8>>;
}

pub async fn load_worktree_vcs_session_diff_from_host(
    worktree_root: &Path,
    base_commit_sha: &str,
) -> Result<String> {
    ctx_fs::worktrees::diff_worktree(worktree_root, base_commit_sha).await
}

pub async fn load_worktree_vcs_session_diff_summary_from_host(
    worktree_root: &Path,
    base_commit_sha: &str,
) -> Result<WorktreeVcsDiffSummaryCounts> {
    let (file_count, line_additions, line_deletions) =
        ctx_fs::worktrees::diff_worktree_summary(worktree_root, base_commit_sha).await?;
    Ok(WorktreeVcsDiffSummaryCounts {
        file_count,
        line_additions,
        line_deletions,
    })
}

pub async fn load_worktree_vcs_session_diff_from_sandbox(
    executor: &impl WorktreeVcsSessionDiffSandboxExecutor,
    base_commit_sha: &str,
) -> Result<String> {
    let bytes = executor
        .stdout(diff_command(
            WORKTREE_VCS_CONTAINER_DIFF_SCRIPT,
            base_commit_sha,
        ))
        .await?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

pub async fn load_worktree_vcs_session_diff_summary_from_sandbox(
    executor: &impl WorktreeVcsSessionDiffSandboxExecutor,
    base_commit_sha: &str,
) -> Result<WorktreeVcsDiffSummaryCounts> {
    let bytes = executor
        .stdout(diff_command(
            WORKTREE_VCS_CONTAINER_DIFF_SUMMARY_SCRIPT,
            base_commit_sha,
        ))
        .await?;
    parse_worktree_vcs_diff_summary_counts(&bytes)
}

fn diff_command(script: &str, base_commit_sha: &str) -> WorktreeVcsSessionDiffCommand {
    WorktreeVcsSessionDiffCommand {
        program: "bash".to_string(),
        args: vec![
            "-lc".to_string(),
            script.to_string(),
            "--".to_string(),
            base_commit_sha.to_string(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone)]
    struct FakeSandboxDiffExecutor {
        commands: Arc<Mutex<Vec<WorktreeVcsSessionDiffCommand>>>,
        stdout: Vec<u8>,
    }

    #[async_trait::async_trait]
    impl WorktreeVcsSessionDiffSandboxExecutor for FakeSandboxDiffExecutor {
        async fn stdout(&self, command: WorktreeVcsSessionDiffCommand) -> Result<Vec<u8>> {
            self.commands.lock().expect("commands lock").push(command);
            Ok(self.stdout.clone())
        }
    }

    #[tokio::test]
    async fn sandbox_diff_uses_owned_container_script_command() {
        let executor = FakeSandboxDiffExecutor {
            commands: Arc::new(Mutex::new(Vec::new())),
            stdout: b"diff --git a/src/lib.rs b/src/lib.rs\n".to_vec(),
        };

        let diff = load_worktree_vcs_session_diff_from_sandbox(&executor, "abc123")
            .await
            .expect("load diff");

        assert!(diff.contains("diff --git"));
        let commands = executor.commands.lock().expect("commands lock");
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].program(), "bash");
        assert_eq!(commands[0].args()[0], "-lc");
        assert_eq!(commands[0].args()[1], WORKTREE_VCS_CONTAINER_DIFF_SCRIPT);
        assert_eq!(commands[0].args()[2], "--");
        assert_eq!(commands[0].args()[3], "abc123");
    }

    #[tokio::test]
    async fn sandbox_diff_summary_parses_counts_from_owned_command() {
        let executor = FakeSandboxDiffExecutor {
            commands: Arc::new(Mutex::new(Vec::new())),
            stdout: b"2 10 3\n".to_vec(),
        };

        let counts = load_worktree_vcs_session_diff_summary_from_sandbox(&executor, "base")
            .await
            .expect("load summary");

        assert_eq!(
            counts,
            WorktreeVcsDiffSummaryCounts {
                file_count: 2,
                line_additions: 10,
                line_deletions: 3,
            }
        );
        let commands = executor.commands.lock().expect("commands lock");
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].program(), "bash");
        assert_eq!(
            commands[0].args()[1],
            WORKTREE_VCS_CONTAINER_DIFF_SUMMARY_SCRIPT
        );
        assert_eq!(commands[0].args()[3], "base");
    }
}
