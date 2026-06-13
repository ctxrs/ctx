use ctx_core::ids::WorkspaceId;
use ctx_core::models::{
    WorkspaceActiveHeadBatch, WorkspaceActiveSnapshot, WorkspaceActiveSnapshotEvent,
};
pub use ctx_workspace_stream_service::read_model::{
    WorkspaceStreamInitialState, WorkspaceStreamSnapshotReadModel,
};
use tokio::sync::broadcast;

use crate::daemon::workspaces::WorkspaceHydrationError;
use crate::daemon::WorkspaceStreamHandle;

pub async fn initial_stream_state(
    handle: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
) -> WorkspaceStreamInitialState {
    let (snapshot_rev, archived_rev) = handle
        .load_workspace_active_snapshot_state(workspace_id)
        .await;
    WorkspaceStreamInitialState {
        snapshot_rev,
        archived_rev,
    }
}

pub async fn prepare_subscription_read_model(
    handle: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
) -> Result<(), WorkspaceHydrationError> {
    handle
        .ensure_workspace_active_snapshot_hydrated(workspace_id)
        .await?;
    handle.activate_workspace_merge_queue(workspace_id).await;
    Ok(())
}

pub async fn load_initial_snapshot_read_model(
    handle: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
) -> Result<WorkspaceStreamSnapshotReadModel, WorkspaceHydrationError> {
    prepare_subscription_read_model(handle, workspace_id).await?;
    let active_snapshot = handle
        .active_snapshot()
        .active_snapshot(workspace_id, i64::MAX)
        .await;
    let active_heads = handle.active_snapshot().active_heads(workspace_id).await;
    Ok(WorkspaceStreamSnapshotReadModel {
        active_snapshot,
        active_heads,
    })
}

impl WorkspaceStreamHandle {
    pub async fn subscribe_workspace_active_snapshot(
        &self,
        workspace_id: WorkspaceId,
    ) -> broadcast::Receiver<WorkspaceActiveSnapshotEvent> {
        self.active_snapshot().subscribe(workspace_id).await
    }

    pub async fn load_workspace_active_snapshot_state(
        &self,
        workspace_id: WorkspaceId,
    ) -> (i64, i64) {
        self.active_snapshot().snapshot_state(workspace_id).await
    }

    pub async fn initial_stream_state(
        &self,
        workspace_id: WorkspaceId,
    ) -> WorkspaceStreamInitialState {
        initial_stream_state(self, workspace_id).await
    }

    pub async fn load_initial_snapshot_read_model(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<WorkspaceStreamSnapshotReadModel, WorkspaceHydrationError> {
        load_initial_snapshot_read_model(self, workspace_id).await
    }

    pub async fn workspace_active_snapshot(
        &self,
        workspace_id: WorkspaceId,
    ) -> WorkspaceActiveSnapshot {
        self.active_snapshot()
            .active_snapshot(workspace_id, i64::MAX)
            .await
    }

    pub async fn workspace_active_heads(
        &self,
        workspace_id: WorkspaceId,
    ) -> WorkspaceActiveHeadBatch {
        self.active_snapshot().active_heads(workspace_id).await
    }
}
