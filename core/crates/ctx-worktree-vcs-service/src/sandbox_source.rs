use anyhow::Result;

use super::{
    is_no_vcs_repo_error, parse_git_diff_name_status, parse_git_list_untracked, parse_git_refs,
    parse_git_single_ref, worktree_vcs_structured_status_from_vcs, WorktreeVcsCommitLookupSource,
    WorktreeVcsDiffPathSource, WorktreeVcsGitCommand, WorktreeVcsStatusSource,
    WorktreeVcsStructuredStatus,
};

#[async_trait::async_trait]
pub trait WorktreeVcsSandboxGitExecutor: Send + Sync {
    async fn git_stdout(&self, command: WorktreeVcsGitCommand) -> Result<Vec<u8>>;
}

pub struct SandboxWorktreeVcsSource<'a, E> {
    executor: &'a E,
}

impl<'a, E> SandboxWorktreeVcsSource<'a, E> {
    pub fn new(executor: &'a E) -> Self {
        Self { executor }
    }
}

impl<E> SandboxWorktreeVcsSource<'_, E>
where
    E: WorktreeVcsSandboxGitExecutor,
{
    pub async fn has_vcs_repo(&self) -> Result<bool> {
        match self
            .executor
            .git_stdout(WorktreeVcsGitCommand::IsInsideWorkTree)
            .await
        {
            Ok(_) => Ok(true),
            Err(err) if is_no_vcs_repo_error(&err) => Ok(false),
            Err(err) => Err(err),
        }
    }

    pub async fn load_structured_status(
        &self,
        include_untracked_files: bool,
        include_entries: bool,
    ) -> Result<WorktreeVcsStructuredStatus> {
        let bytes = self
            .executor
            .git_stdout(WorktreeVcsGitCommand::Status {
                include_untracked_files,
            })
            .await?;
        Ok(worktree_vcs_structured_status_from_vcs(
            ctx_fs::git::git_status_structured_from_bytes_with_entries(&bytes, include_entries),
        ))
    }

    pub async fn resolve_commit(&self, reference: &str) -> Result<String> {
        let bytes = self
            .executor
            .git_stdout(WorktreeVcsGitCommand::RevParse {
                reference: reference.to_string(),
            })
            .await?;
        Ok(parse_git_single_ref(&bytes))
    }

    pub async fn rev_parse_refs(&self, references: &[&str]) -> Result<Vec<String>> {
        if references.is_empty() {
            return Ok(Vec::new());
        }
        let bytes = self
            .executor
            .git_stdout(WorktreeVcsGitCommand::RevParseRefs {
                references: references
                    .iter()
                    .map(|reference| (*reference).to_string())
                    .collect(),
            })
            .await?;
        parse_git_refs(&bytes, references.len())
    }

    pub async fn merge_base(&self, target_branch: &str) -> Result<String> {
        let bytes = self
            .executor
            .git_stdout(WorktreeVcsGitCommand::MergeBase {
                target_branch: target_branch.to_string(),
            })
            .await?;
        Ok(parse_git_single_ref(&bytes))
    }

    pub async fn diff_name_status(
        &self,
        base_commit_sha: &str,
        summary_count: bool,
    ) -> Result<Vec<(String, String, Option<String>)>> {
        let bytes = self
            .executor
            .git_stdout(WorktreeVcsGitCommand::DiffNameStatus {
                base_commit_sha: base_commit_sha.to_string(),
                no_renames: summary_count,
            })
            .await?;
        Ok(parse_git_diff_name_status(&bytes))
    }

    pub async fn list_untracked(&self) -> Result<Vec<String>> {
        let bytes = self
            .executor
            .git_stdout(WorktreeVcsGitCommand::ListUntracked)
            .await?;
        Ok(parse_git_list_untracked(&bytes))
    }
}

#[async_trait::async_trait]
impl<E> WorktreeVcsStatusSource for SandboxWorktreeVcsSource<'_, E>
where
    E: WorktreeVcsSandboxGitExecutor,
{
    async fn has_vcs_repo(&self) -> Result<bool> {
        SandboxWorktreeVcsSource::has_vcs_repo(self).await
    }

    async fn load_structured_status(
        &self,
        include_untracked_files: bool,
        include_entries: bool,
    ) -> Result<WorktreeVcsStructuredStatus> {
        SandboxWorktreeVcsSource::load_structured_status(
            self,
            include_untracked_files,
            include_entries,
        )
        .await
    }
}

#[async_trait::async_trait]
impl<E> WorktreeVcsCommitLookupSource for SandboxWorktreeVcsSource<'_, E>
where
    E: WorktreeVcsSandboxGitExecutor,
{
    async fn resolve_commit(&self, reference: &str) -> Result<String> {
        SandboxWorktreeVcsSource::resolve_commit(self, reference).await
    }
}

#[async_trait::async_trait]
impl<E> WorktreeVcsDiffPathSource for SandboxWorktreeVcsSource<'_, E>
where
    E: WorktreeVcsSandboxGitExecutor,
{
    async fn diff_name_status(
        &self,
        base_commit_sha: &str,
        summary_count: bool,
    ) -> Result<Vec<(String, String, Option<String>)>> {
        SandboxWorktreeVcsSource::diff_name_status(self, base_commit_sha, summary_count).await
    }

    async fn list_untracked(&self) -> Result<Vec<String>> {
        SandboxWorktreeVcsSource::list_untracked(self).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use anyhow::anyhow;

    use super::*;

    #[derive(Default)]
    struct FakeSandboxGitExecutor {
        commands: Arc<Mutex<Vec<WorktreeVcsGitCommand>>>,
        stdout: Vec<u8>,
        error: Option<String>,
    }

    #[async_trait::async_trait]
    impl WorktreeVcsSandboxGitExecutor for FakeSandboxGitExecutor {
        async fn git_stdout(&self, command: WorktreeVcsGitCommand) -> Result<Vec<u8>> {
            self.commands.lock().expect("commands lock").push(command);
            if let Some(error) = &self.error {
                return Err(anyhow!(error.clone()));
            }
            Ok(self.stdout.clone())
        }
    }

    #[tokio::test]
    async fn sandbox_source_reports_no_repo_from_executor_error() {
        let executor = FakeSandboxGitExecutor {
            error: Some("fatal: not a git repository".to_string()),
            ..Default::default()
        };
        let source = SandboxWorktreeVcsSource::new(&executor);

        assert!(!source.has_vcs_repo().await.expect("repo check"));
    }

    #[tokio::test]
    async fn sandbox_source_resolves_refs_from_stdout() {
        let executor = FakeSandboxGitExecutor {
            stdout: b"abc123\n".to_vec(),
            ..Default::default()
        };
        let source = SandboxWorktreeVcsSource::new(&executor);

        assert_eq!(
            source.resolve_commit("HEAD").await.expect("resolve head"),
            "abc123"
        );
        assert_eq!(
            executor.commands.lock().expect("commands lock").as_slice(),
            &[WorktreeVcsGitCommand::RevParse {
                reference: "HEAD".to_string()
            }]
        );
    }

    #[tokio::test]
    async fn sandbox_source_summary_diff_disables_renames() {
        let executor = FakeSandboxGitExecutor {
            stdout: b"M\0src/lib.rs\0".to_vec(),
            ..Default::default()
        };
        let source = SandboxWorktreeVcsSource::new(&executor);

        let entries = source
            .diff_name_status("base", true)
            .await
            .expect("diff entries");

        assert_eq!(
            entries,
            vec![("M".to_string(), "src/lib.rs".to_string(), None)]
        );
        assert_eq!(
            executor.commands.lock().expect("commands lock").as_slice(),
            &[WorktreeVcsGitCommand::DiffNameStatus {
                base_commit_sha: "base".to_string(),
                no_renames: true,
            }]
        );
    }
}
