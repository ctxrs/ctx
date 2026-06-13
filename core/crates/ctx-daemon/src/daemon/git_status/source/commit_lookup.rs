use anyhow::Result;
use ctx_settings_model::ExecutionMode;
use ctx_worktree_data_plane::resolve_worktree_data_plane_with_host as resolve_worktree_data_plane;
use ctx_worktree_vcs_service::{
    LocalWorktreeVcsSource, SandboxWorktreeVcsSource, WorktreeVcsCommitLookupSource,
};

use super::super::sandbox::HttpSandboxWorktreeVcsExecutor;
use super::HttpWorktreeVcsSource;

#[async_trait::async_trait]
impl WorktreeVcsCommitLookupSource for HttpWorktreeVcsSource<'_> {
    async fn resolve_commit(&self, reference: &str) -> Result<String> {
        let data_plane = resolve_worktree_data_plane(self.execution, self.worktree).await?;
        let root = data_plane.live_worktree_root.as_path();
        if matches!(data_plane.execution_mode, ExecutionMode::Sandbox) {
            let executor = HttpSandboxWorktreeVcsExecutor::new(self.execution, self.worktree);
            return SandboxWorktreeVcsSource::new(&executor)
                .resolve_commit(reference)
                .await;
        }
        LocalWorktreeVcsSource::new(self.worktree, root)
            .resolve_commit(reference)
            .await
    }
}
