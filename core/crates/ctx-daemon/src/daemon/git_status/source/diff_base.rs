use anyhow::Result;
use ctx_settings_model::ExecutionMode;
use ctx_workspace_config as workspace_config;
use ctx_worktree_data_plane::resolve_worktree_data_plane_with_host as resolve_worktree_data_plane;
use ctx_worktree_vcs_service::{
    LocalWorktreeVcsSource, SandboxWorktreeVcsSource, WorktreeVcsCommitLookupSource,
    WorktreeVcsDiffBaseSource,
};

use super::super::sandbox::HttpSandboxWorktreeVcsExecutor;
use super::HttpWorktreeVcsSource;

#[async_trait::async_trait]
impl WorktreeVcsDiffBaseSource for HttpWorktreeVcsSource<'_> {
    async fn load_primary_branch(&self) -> Result<Option<String>> {
        let store = self.execution.store_for_worktree(self.worktree.id).await?;
        workspace_config::load_primary_branch(&store).await
    }

    async fn rev_parse_head(&self) -> Result<String> {
        self.resolve_commit("HEAD").await
    }

    async fn rev_parse_refs(&self, references: &[&str]) -> Result<Vec<String>> {
        if references.is_empty() {
            return Ok(Vec::new());
        }
        let data_plane = resolve_worktree_data_plane(self.execution, self.worktree).await?;
        let root = data_plane.live_worktree_root.as_path();
        if matches!(data_plane.execution_mode, ExecutionMode::Sandbox) {
            let executor = HttpSandboxWorktreeVcsExecutor::new(self.execution, self.worktree);
            return SandboxWorktreeVcsSource::new(&executor)
                .rev_parse_refs(references)
                .await;
        }

        LocalWorktreeVcsSource::new(self.worktree, root)
            .rev_parse_refs(references)
            .await
    }

    async fn merge_base(&self, target_branch: &str) -> Result<String> {
        let data_plane = resolve_worktree_data_plane(self.execution, self.worktree).await?;
        let root = data_plane.live_worktree_root.as_path();
        if matches!(data_plane.execution_mode, ExecutionMode::Sandbox) {
            let executor = HttpSandboxWorktreeVcsExecutor::new(self.execution, self.worktree);
            return SandboxWorktreeVcsSource::new(&executor)
                .merge_base(target_branch)
                .await;
        }
        LocalWorktreeVcsSource::new(self.worktree, root)
            .merge_base(target_branch)
            .await
    }

    fn redact_error(&self, err: &anyhow::Error) -> String {
        ctx_observability::logs::redact_sensitive(&err.to_string())
    }
}
