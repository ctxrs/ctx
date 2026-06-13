use anyhow::Result;
use async_trait::async_trait;
use ctx_core::ids::WorkspaceId;
use ctx_core::models::{SessionHeadSnapshot, WorkspaceActiveTaskSummary};
use ctx_store::Store;

use ctx_workspace_active_snapshot::WorkspaceActiveSnapshotHub;

#[derive(Debug)]
pub(in crate::daemon::workspaces::hydration) struct WorkspaceSnapshotHydrationPayload {
    pub(in crate::daemon::workspaces::hydration) snapshot_rev: i64,
    pub(in crate::daemon::workspaces::hydration) archived_rev: i64,
    pub(in crate::daemon::workspaces::hydration) tasks: Vec<WorkspaceActiveTaskSummary>,
    pub(in crate::daemon::workspaces::hydration) heads: Vec<SessionHeadSnapshot>,
}

#[async_trait]
pub(in crate::daemon::workspaces::hydration) trait WorkspaceSnapshotHydrationStore {
    async fn get_snapshot_state(&self, workspace_id: WorkspaceId) -> Result<(i64, i64)>;
    async fn list_active_page_for_hydration(
        &self,
        workspace_id: WorkspaceId,
        limit: i64,
    ) -> Result<Vec<WorkspaceActiveTaskSummary>>;
    async fn list_active_heads(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Vec<SessionHeadSnapshot>>;
}

#[async_trait]
impl WorkspaceSnapshotHydrationStore for Store {
    async fn get_snapshot_state(&self, workspace_id: WorkspaceId) -> Result<(i64, i64)> {
        self.get_workspace_active_snapshot_state(workspace_id).await
    }

    async fn list_active_page_for_hydration(
        &self,
        workspace_id: WorkspaceId,
        limit: i64,
    ) -> Result<Vec<WorkspaceActiveTaskSummary>> {
        self.list_workspace_active_page_without_total(workspace_id, limit)
            .await
    }

    async fn list_active_heads(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Vec<SessionHeadSnapshot>> {
        self.list_workspace_active_head_snapshots(workspace_id)
            .await
    }
}

pub(super) async fn load_workspace_snapshot_hydration_payload<
    S: WorkspaceSnapshotHydrationStore + Sync,
>(
    store: &S,
    workspace_id: WorkspaceId,
) -> Result<WorkspaceSnapshotHydrationPayload> {
    let payload_start = std::time::Instant::now();
    let snapshot_state_start = std::time::Instant::now();
    let (snapshot_rev, archived_rev) = store.get_snapshot_state(workspace_id).await?;
    let snapshot_state_ms = snapshot_state_start.elapsed().as_millis();
    let active_page_start = std::time::Instant::now();
    let tasks = store
        .list_active_page_for_hydration(workspace_id, i64::MAX)
        .await?;
    let active_page_ms = active_page_start.elapsed().as_millis();
    let active_heads_start = std::time::Instant::now();
    let heads = store.list_active_heads(workspace_id).await?;
    let active_heads_ms = active_heads_start.elapsed().as_millis();
    if std::env::var_os("CTX_DEBUG_WORKSPACE_STREAM_TIMINGS").is_some() {
        eprintln!(
            "CTX_WS_TIMING hydration_payload workspace_id={} snapshot_state_ms={} active_page_ms={} active_heads_ms={} active_tasks={} active_heads={} total_ms={}",
            workspace_id.0,
            snapshot_state_ms,
            active_page_ms,
            active_heads_ms,
            tasks.len(),
            heads.len(),
            payload_start.elapsed().as_millis(),
        );
    }
    Ok(WorkspaceSnapshotHydrationPayload {
        snapshot_rev,
        archived_rev,
        tasks,
        heads,
    })
}

pub(super) async fn apply_workspace_snapshot_hydration_payload(
    active_snapshot: &WorkspaceActiveSnapshotHub,
    workspace_id: WorkspaceId,
    payload: WorkspaceSnapshotHydrationPayload,
) {
    active_snapshot
        .hydrate_snapshot(
            workspace_id,
            payload.snapshot_rev,
            payload.archived_rev,
            payload.tasks,
            payload.heads,
        )
        .await;
}
