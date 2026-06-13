use anyhow::Result;
use ctx_settings_model::ExecutionMode;
use ctx_worktree_data_plane::resolve_worktree_data_plane_with_host as resolve_worktree_data_plane;
use ctx_worktree_vcs_service::{
    LocalWorktreeVcsSource, SandboxWorktreeVcsSource, WorktreeVcsDiffPathSource,
};

use super::super::sandbox::HttpSandboxWorktreeVcsExecutor;
use super::HttpWorktreeVcsSource;

#[async_trait::async_trait]
impl WorktreeVcsDiffPathSource for HttpWorktreeVcsSource<'_> {
    async fn diff_name_status(
        &self,
        base_commit_sha: &str,
        summary_count: bool,
    ) -> Result<Vec<(String, String, Option<String>)>> {
        let data_plane = resolve_worktree_data_plane(self.execution, self.worktree).await?;
        let root = data_plane.live_worktree_root.as_path();
        if matches!(data_plane.execution_mode, ExecutionMode::Sandbox) {
            let executor = HttpSandboxWorktreeVcsExecutor::new(self.execution, self.worktree);
            return SandboxWorktreeVcsSource::new(&executor)
                .diff_name_status(base_commit_sha, summary_count)
                .await;
        }
        LocalWorktreeVcsSource::new(self.worktree, root)
            .diff_name_status(base_commit_sha, summary_count)
            .await
    }

    async fn list_untracked(&self) -> Result<Vec<String>> {
        let data_plane = resolve_worktree_data_plane(self.execution, self.worktree).await?;
        let root = data_plane.live_worktree_root.as_path();
        if matches!(data_plane.execution_mode, ExecutionMode::Sandbox) {
            let executor = HttpSandboxWorktreeVcsExecutor::new(self.execution, self.worktree);
            return SandboxWorktreeVcsSource::new(&executor)
                .list_untracked()
                .await;
        }
        LocalWorktreeVcsSource::new(self.worktree, root)
            .list_untracked()
            .await
    }
}
