use super::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::TimeDelta;
use chrono::Utc;
use ctx_core::ids::{MergeQueueEntryId, WorkspaceId};
use ctx_core::models::{
    MergeQueueEntry, MergeQueueEntryStatus, MergeQueuePatchSource, VcsKind, Workspace,
};
use ctx_providers::adapters::ProviderAdapter;
use ctx_store::StoreManager;
use ctx_workspace_config::{update_merge_queue_config, MergeQueueConfigUpdate};

async fn setup_state() -> (tempfile::TempDir, Arc<DaemonState>) {
    let data_dir = tempfile::tempdir().unwrap();
    let stores = StoreManager::open(data_dir.path()).await.unwrap();
    let providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    let state = Arc::new(DaemonState::new(
        data_dir.path().to_path_buf(),
        stores,
        providers,
        "http://127.0.0.1:0".to_string(),
        None,
    ));
    (data_dir, state)
}

async fn create_workspace(
    state: &Arc<DaemonState>,
    data_dir: &tempfile::TempDir,
    name: &str,
) -> Workspace {
    let root = data_dir.path().join(name);
    tokio::fs::create_dir_all(&root).await.unwrap();
    state
        .global_store()
        .create_workspace(
            name.to_string(),
            root.to_string_lossy().to_string(),
            VcsKind::Git,
        )
        .await
        .unwrap()
}

fn queued_entry(workspace_id: WorkspaceId, name: &str) -> MergeQueueEntry {
    let now = Utc::now();
    MergeQueueEntry {
        id: MergeQueueEntryId::new(),
        workspace_id,
        worktree_id: None,
        session_id: None,
        target_branch: "main".to_string(),
        message: Some(name.to_string()),
        patch_source: MergeQueuePatchSource::Generated,
        base_commit_sha: Some(format!("{name}-base")),
        head_commit_sha: Some(format!("{name}-head")),
        patch_path: format!("/tmp/{name}.patch"),
        patch_size: 1,
        status: MergeQueueEntryStatus::Queued,
        result_commit_sha: None,
        error_message: None,
        created_at: now,
        updated_at: now,
    }
}

async fn wait_for_entry_status<F>(
    state: &Arc<DaemonState>,
    workspace_id: WorkspaceId,
    entry_id: MergeQueueEntryId,
    predicate: F,
    timeout: Duration,
) -> MergeQueueEntry
where
    F: Fn(MergeQueueEntryStatus) -> bool,
{
    tokio::time::timeout(timeout, async {
        loop {
            let store = state.core.stores.workspace(workspace_id).await.unwrap();
            let entry = store
                .get_merge_queue_entry(entry_id)
                .await
                .unwrap()
                .unwrap();
            if predicate(entry.status.clone()) {
                break entry;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("timed out waiting for merge queue entry status")
}

mod activation;
mod disabled;
mod drain_ownership;
mod listing;
mod resume;
