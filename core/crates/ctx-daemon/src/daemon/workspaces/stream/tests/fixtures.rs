use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::daemon::{
    route_handles_from_state, DaemonState, WorkspaceStreamHandle, WorkspaceVcsStreamHandle,
};
use ctx_core::ids::{SessionId, WorkspaceId};
use ctx_core::models::{ExecutionEnvironment, Worktree};
use ctx_store::StoreManager;

pub(super) fn session_id(value: &str) -> SessionId {
    SessionId(uuid::Uuid::parse_str(value).unwrap())
}

pub(super) async fn test_state(root: &Path) -> Arc<DaemonState> {
    Arc::new(DaemonState::new(
        root.to_path_buf(),
        StoreManager::open(root).await.unwrap(),
        HashMap::new(),
        "http://127.0.0.1:4399".to_string(),
        Some("daemon-secret".to_string()),
    ))
}

pub(super) fn workspace_stream_handle(state: &Arc<DaemonState>) -> WorkspaceStreamHandle {
    route_handles_from_state(state).workspace_stream
}

pub(super) fn workspace_vcs_stream_handle(state: &Arc<DaemonState>) -> WorkspaceVcsStreamHandle {
    route_handles_from_state(state).workspace_vcs_stream
}

pub(super) async fn create_workspace_session(
    state: &Arc<DaemonState>,
    root: &Path,
) -> (WorkspaceId, SessionId) {
    let (workspace_id, worktree) = create_workspace_worktree(state, root).await;
    let store = state.store_for_workspace(workspace_id).await.unwrap();
    let task = store
        .create_task(workspace_id, "task".to_string(), None)
        .await
        .unwrap();
    let session = store
        .create_session(
            task.id,
            workspace_id,
            worktree.id,
            ExecutionEnvironment::Host,
            "fake".to_string(),
            "model".to_string(),
            "implementer".to_string(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    state
        .global_store()
        .upsert_workspace_session_index(session.id, workspace_id)
        .await
        .unwrap();
    (workspace_id, session.id)
}

pub(super) async fn create_workspace_worktree(
    state: &Arc<DaemonState>,
    root: &Path,
) -> (WorkspaceId, Worktree) {
    let workspace = state
        .global_store()
        .create_workspace(
            format!("ws-{}", uuid::Uuid::new_v4()),
            root.join(format!("ws-{}", uuid::Uuid::new_v4()))
                .to_string_lossy()
                .to_string(),
            ctx_core::models::VcsKind::Git,
        )
        .await
        .unwrap();
    let worktree = create_worktree_for_workspace(state, root, workspace.id).await;
    (workspace.id, worktree)
}

pub(super) async fn create_worktree_for_workspace(
    state: &Arc<DaemonState>,
    root: &Path,
    workspace_id: WorkspaceId,
) -> Worktree {
    let store = state.store_for_workspace(workspace_id).await.unwrap();
    let worktree = store
        .create_worktree(
            workspace_id,
            root.join(format!("worktree-{}", uuid::Uuid::new_v4()))
                .to_string_lossy()
                .to_string(),
            "deadbeef".to_string(),
            None,
        )
        .await
        .unwrap();
    state
        .global_store()
        .upsert_workspace_worktree_index(worktree.id, workspace_id)
        .await
        .unwrap();
    worktree
}
