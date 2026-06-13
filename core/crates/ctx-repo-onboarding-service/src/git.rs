use std::path::{Path, PathBuf};

use tokio::process::Command;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RepoGitCommandError {
    Spawn {
        message: String,
    },
    Failed {
        action: &'static str,
        stderr: String,
    },
}

impl RepoGitCommandError {
    pub fn spawn_message(&self) -> Option<&str> {
        match self {
            RepoGitCommandError::Spawn { message } => Some(message.as_str()),
            RepoGitCommandError::Failed { .. } => None,
        }
    }

    pub fn failed_message(&self) -> Option<String> {
        match self {
            RepoGitCommandError::Spawn { .. } => None,
            RepoGitCommandError::Failed { action, stderr } => {
                Some(format!("{action} failed: {stderr}"))
            }
        }
    }
}

pub(super) async fn ensure_git_usable() -> Result<(), String> {
    let output = Command::new("git")
        .arg("--version")
        .output()
        .await
        .map_err(|e| format!("git is required but could not be executed: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let msg = if !stderr.trim().is_empty() {
        stderr.trim().to_string()
    } else if !stdout.trim().is_empty() {
        stdout.trim().to_string()
    } else {
        format!("git exited with status {}", output.status)
    };
    Err(format!("git is required but appears unusable: {msg}"))
}

pub(super) async fn run_git_clone(
    repo_url: &str,
    branch: Option<&str>,
    dest: &Path,
) -> Result<(), RepoGitCommandError> {
    let mut cmd = Command::new("git");
    cmd.arg("clone");
    if let Some(branch) = branch {
        cmd.arg("--branch").arg(branch).arg("--single-branch");
    }
    cmd.arg("--").arg(repo_url).arg(dest);
    run_git_command(cmd, "git clone").await
}

pub(super) async fn init_git_repo_with_initial_commit(
    path: &Path,
) -> Result<(), RepoGitCommandError> {
    let mut init = Command::new("git");
    init.arg("init").arg("--").arg(path);
    run_git_command(init, "git init").await?;

    let mut commit = Command::new("git");
    commit
        .arg("-C")
        .arg(path)
        .arg("-c")
        .arg("user.name=ctx")
        .arg("-c")
        .arg("user.email=ctx@localhost")
        .arg("commit")
        .arg("--allow-empty")
        .arg("-m")
        .arg("Initial commit");
    run_git_command(commit, "git commit").await
}

pub(super) async fn canonical_clone_dest(dest: PathBuf) -> PathBuf {
    tokio::fs::canonicalize(&dest).await.unwrap_or(dest)
}

async fn run_git_command(
    mut cmd: Command,
    action: &'static str,
) -> Result<(), RepoGitCommandError> {
    let output = cmd.output().await.map_err(|e| RepoGitCommandError::Spawn {
        message: e.to_string(),
    })?;
    if output.status.success() {
        return Ok(());
    }
    Err(RepoGitCommandError::Failed {
        action,
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_git_command_error_preserves_existing_messages() {
        let spawn = RepoGitCommandError::Spawn {
            message: "permission denied".to_string(),
        };
        assert_eq!(spawn.spawn_message(), Some("permission denied"));
        assert_eq!(spawn.failed_message(), None);

        let failed = RepoGitCommandError::Failed {
            action: "git clone",
            stderr: "fatal: missing repo\n".to_string(),
        };
        assert_eq!(failed.spawn_message(), None);
        assert_eq!(
            failed.failed_message(),
            Some("git clone failed: fatal: missing repo\n".to_string())
        );
    }
}
