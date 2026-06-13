use anyhow::Result;
use ctx_settings_model::ExecutionMode;
use ctx_worktree_data_plane::resolve_worktree_data_plane_with_host as resolve_worktree_data_plane;
use ctx_worktree_vcs_service::{
    LocalWorktreeVcsSource, SandboxWorktreeVcsSource, WorktreeVcsStatusSource,
    WorktreeVcsStructuredStatus,
};

use super::super::sandbox::HttpSandboxWorktreeVcsExecutor;
use super::HttpWorktreeVcsSource;

#[async_trait::async_trait]
impl WorktreeVcsStatusSource for HttpWorktreeVcsSource<'_> {
    async fn has_vcs_repo(&self) -> Result<bool> {
        let data_plane = resolve_worktree_data_plane(self.execution, self.worktree).await?;
        if matches!(data_plane.execution_mode, ExecutionMode::Sandbox) {
            let executor = HttpSandboxWorktreeVcsExecutor::new(self.execution, self.worktree);
            return SandboxWorktreeVcsSource::new(&executor)
                .has_vcs_repo()
                .await;
        }

        LocalWorktreeVcsSource::new(self.worktree, data_plane.live_worktree_root.as_path())
            .has_vcs_repo()
            .await
    }

    async fn load_structured_status(
        &self,
        include_untracked_files: bool,
        include_entries: bool,
    ) -> Result<WorktreeVcsStructuredStatus> {
        let data_plane = resolve_worktree_data_plane(self.execution, self.worktree).await?;
        let root = data_plane.live_worktree_root.as_path();
        let structured = if matches!(data_plane.execution_mode, ExecutionMode::Sandbox) {
            let executor = HttpSandboxWorktreeVcsExecutor::new(self.execution, self.worktree);
            SandboxWorktreeVcsSource::new(&executor)
                .load_structured_status(include_untracked_files, include_entries)
                .await?
        } else {
            LocalWorktreeVcsSource::new(self.worktree, root)
                .load_structured_status(include_untracked_files, include_entries)
                .await?
        };
        Ok(structured)
    }
}
