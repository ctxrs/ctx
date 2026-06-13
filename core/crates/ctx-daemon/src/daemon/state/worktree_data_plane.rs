use anyhow::Result;
use async_trait::async_trait;

use ctx_core::ids::WorkspaceId;
use ctx_store::Store;
use ctx_worktree_data_plane::WorktreeDataPlaneHost;

use crate::daemon::DaemonState;

#[async_trait]
impl WorktreeDataPlaneHost for DaemonState {
    async fn get_workspace(
        state: &Self,
        workspace_id: WorkspaceId,
    ) -> Result<Option<ctx_core::models::Workspace>> {
        state.global_store().get_workspace(workspace_id).await
    }

    async fn workspace_store(state: &Self, workspace_id: WorkspaceId) -> Result<Store> {
        state.store_for_workspace(workspace_id).await
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use ctx_core::ids::WorktreeId;
    use ctx_core::models::{ExecutionEnvironment, VcsKind, Worktree};
    use ctx_store::StoreManager;
    use std::collections::HashMap;
    use std::sync::Arc;
    use uuid::Uuid;

    use crate::daemon::DaemonState;

    #[tokio::test]
    async fn resolve_worktree_data_plane_rejects_sandbox_session_without_binding() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo_root = temp.path().join("repo");
        std::fs::create_dir_all(&repo_root).expect("create repo root");
        let state = Arc::new(DaemonState::new(
            temp.path().to_path_buf(),
            StoreManager::open(temp.path()).await.expect("open stores"),
            HashMap::new(),
            "http://127.0.0.1:4310".to_string(),
            None,
        ));
        let workspace = state
            .global_store()
            .create_workspace(
                "ws".to_string(),
                repo_root.to_string_lossy().to_string(),
                VcsKind::Git,
            )
            .await
            .expect("create workspace");
        let store = state
            .store_for_workspace(workspace.id)
            .await
            .expect("workspace store");
        let task = store
            .create_task(workspace.id, "task".to_string(), None)
            .await
            .expect("create task");
        let worktree_root = temp.path().join("managed-worktree");
        std::fs::create_dir_all(&worktree_root).expect("create managed root");
        let worktree = store
            .insert_worktree(Worktree {
                id: WorktreeId(Uuid::new_v4()),
                workspace_id: workspace.id,
                root_path: worktree_root.to_string_lossy().to_string(),
                base_commit_sha: "abc123".to_string(),
                git_branch: Some("ctx/test".to_string()),
                vcs_kind: Some(VcsKind::Git),
                base_revision: Some("abc123".to_string()),
                vcs_ref: Some("ctx/test".to_string()),
                created_at: Utc::now(),
                bootstrap_status: None,
                bootstrap_started_at: None,
                bootstrap_finished_at: None,
                bootstrap_exit_code: None,
                bootstrap_timeout_sec: None,
                bootstrap_error: None,
                bootstrap_log_path: None,
                bootstrap_log_truncated: None,
                bootstrap_command: None,
                bootstrap_script_path: None,
            })
            .await
            .expect("insert worktree");
        store
            .create_session(
                task.id,
                workspace.id,
                worktree.id,
                ExecutionEnvironment::Sandbox,
                "fake".to_string(),
                "model".to_string(),
                "session".to_string(),
                None,
                None,
                None,
            )
            .await
            .expect("create sandbox session");

        let err = ctx_worktree_data_plane::resolve_worktree_data_plane_with_host(
            state.as_ref(),
            &worktree,
        )
        .await
        .expect_err("sandbox session without binding must fail closed");

        assert!(err
            .to_string()
            .contains("sandbox binding is missing for sandbox worktree"));
    }
}
