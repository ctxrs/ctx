use std::collections::HashMap;
use std::sync::Arc;

use ctx_core::models::{ExecutionEnvironment, Session, VcsKind};
use ctx_mcp_auth::{McpAuthCapabilities, McpAuthContext};
use ctx_store::StoreManager;

use crate::daemon::DaemonState;

pub(super) async fn seeded_state() -> (tempfile::TempDir, Arc<DaemonState>, Session) {
    let data_dir = tempfile::tempdir().expect("create tempdir");
    let stores = StoreManager::open(data_dir.path())
        .await
        .expect("open stores");
    let workspace = stores
        .global()
        .create_workspace(
            "ws".to_string(),
            data_dir.path().to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .expect("create workspace");
    let store = stores
        .workspace(workspace.id)
        .await
        .expect("open workspace store");
    let worktree = store
        .create_worktree(
            workspace.id,
            data_dir.path().to_string_lossy().to_string(),
            "base".to_string(),
            None,
        )
        .await
        .expect("create worktree");
    stores
        .global()
        .upsert_workspace_worktree_index(worktree.id, workspace.id)
        .await
        .expect("index worktree");
    let task = store
        .create_task(workspace.id, "task".to_string(), None)
        .await
        .expect("create task");
    stores
        .global()
        .upsert_workspace_task_index(task.id, workspace.id)
        .await
        .expect("index task");
    let session = store
        .create_session(
            task.id,
            workspace.id,
            worktree.id,
            ExecutionEnvironment::Host,
            "fake".to_string(),
            "fake-model".to_string(),
            "implementer".to_string(),
            None,
            None,
            None,
        )
        .await
        .expect("create session");
    stores
        .global()
        .upsert_workspace_session_index(session.id, workspace.id)
        .await
        .expect("index session");

    let state = Arc::new(DaemonState::new(
        data_dir.path().to_path_buf(),
        stores,
        HashMap::new(),
        "http://127.0.0.1:0".to_string(),
        None,
    ));
    (data_dir, state, session)
}

pub(super) async fn block_workspace_store_for_session(
    data_dir: &tempfile::TempDir,
    state: &Arc<DaemonState>,
    session: &Session,
) {
    state.task_session_cleanup.cleanup_session(session.id).await;
    state
        .core
        .stores
        .evict_workspace(session.workspace_id)
        .await;

    let blocked_workspace_store_dir = data_dir
        .path()
        .join("db")
        .join("workspaces")
        .join(session.workspace_id.0.to_string());
    if let Ok(metadata) = tokio::fs::metadata(&blocked_workspace_store_dir).await {
        if metadata.is_dir() {
            tokio::fs::remove_dir_all(&blocked_workspace_store_dir)
                .await
                .expect("remove workspace store dir");
        } else {
            tokio::fs::remove_file(&blocked_workspace_store_dir)
                .await
                .expect("remove workspace store file");
        }
    }
    tokio::fs::create_dir_all(
        blocked_workspace_store_dir
            .parent()
            .expect("workspace store parent"),
    )
    .await
    .expect("create workspace store parent");
    tokio::fs::write(&blocked_workspace_store_dir, b"blocked workspace store")
        .await
        .expect("block workspace store");
}

pub(super) fn context_for(session: &Session) -> McpAuthContext {
    McpAuthContext {
        session_id: session.id,
        workspace_id: session.workspace_id,
        worktree_id: session.worktree_id,
        capabilities: McpAuthCapabilities::provider_session(),
    }
}
