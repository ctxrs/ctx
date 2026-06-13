use ctx_core::models::{WorkspaceActiveHeadBatch, WorkspaceActiveSnapshot};
use ctx_workspace_active_snapshot::{
    WorkspaceActiveHeadCacheEntry, WorkspaceActiveSnapshotCacheEntry,
};

use crate::daemon::state::{TimedEntry, WorkspaceActiveHeadsCache, WorkspaceActiveSnapshotCache};

#[derive(Clone)]
pub(in crate::daemon) struct WorkspaceActiveCacheRuntime {
    snapshot_cache: WorkspaceActiveSnapshotCache,
    heads_cache: WorkspaceActiveHeadsCache,
}

impl WorkspaceActiveCacheRuntime {
    pub(in crate::daemon) fn new(
        snapshot_cache: WorkspaceActiveSnapshotCache,
        heads_cache: WorkspaceActiveHeadsCache,
    ) -> Self {
        Self {
            snapshot_cache,
            heads_cache,
        }
    }

    pub(in crate::daemon) async fn cache_workspace_active_snapshot(
        &self,
        snapshot: WorkspaceActiveSnapshot,
    ) {
        let workspace_id = snapshot.workspace_id;
        let mut cache = self.snapshot_cache.lock().await;
        cache.insert(
            workspace_id,
            TimedEntry::new(WorkspaceActiveSnapshotCacheEntry { snapshot }),
        );
    }

    pub(in crate::daemon) async fn cache_workspace_active_heads(
        &self,
        batch: WorkspaceActiveHeadBatch,
    ) {
        let workspace_id = batch.workspace_id;
        let mut cache = self.heads_cache.lock().await;
        cache.insert(
            workspace_id,
            TimedEntry::new(WorkspaceActiveHeadCacheEntry { batch }),
        );
    }
}
