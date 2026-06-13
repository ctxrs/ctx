use std::sync::Arc;

use ctx_core::ids::WorkspaceId;
use ctx_store::Store;
use ctx_workspace_active_snapshot::WorkspaceActiveSnapshotHub;

use crate::daemon::state::ProtectedWorkspaceStoreLookup;
use crate::daemon::StoreLookup;

mod payload;

use payload::{
    apply_workspace_snapshot_hydration_payload, load_workspace_snapshot_hydration_payload,
};

#[cfg(test)]
pub(in crate::daemon::workspaces::hydration) use payload::{
    WorkspaceSnapshotHydrationPayload, WorkspaceSnapshotHydrationStore,
};

#[derive(Debug)]
pub enum WorkspaceHydrationError {
    NotFound,
    Load(anyhow::Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceHydrationErrorKind {
    NotFound,
    Load,
}

impl WorkspaceHydrationError {
    pub fn kind(&self) -> WorkspaceHydrationErrorKind {
        match self {
            WorkspaceHydrationError::NotFound => WorkspaceHydrationErrorKind::NotFound,
            WorkspaceHydrationError::Load(_) => WorkspaceHydrationErrorKind::Load,
        }
    }
}

#[derive(Clone)]
pub(in crate::daemon) struct WorkspaceActiveHydrationRuntime {
    global_store: Store,
    workspace_stores: ProtectedWorkspaceStoreLookup,
    active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
}

impl WorkspaceActiveHydrationRuntime {
    pub(in crate::daemon) fn new(
        global_store: Store,
        workspace_stores: ProtectedWorkspaceStoreLookup,
        active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    ) -> Self {
        Self {
            global_store,
            workspace_stores,
            active_snapshot,
        }
    }

    pub(in crate::daemon) async fn ensure_workspace_active_snapshot_hydrated(
        &self,
        workspace_id: WorkspaceId,
    ) -> std::result::Result<(), WorkspaceHydrationError> {
        ensure_workspace_active_snapshot_hydrated_with_deps(
            &self.global_store,
            &self.workspace_stores,
            self.active_snapshot.as_ref(),
            workspace_id,
        )
        .await
    }
}

pub(in crate::daemon) async fn ensure_workspace_active_snapshot_hydrated_with_deps(
    global_store: &Store,
    workspace_stores: &ProtectedWorkspaceStoreLookup,
    active_snapshot: &WorkspaceActiveSnapshotHub,
    workspace_id: WorkspaceId,
) -> std::result::Result<(), WorkspaceHydrationError> {
    let hydration_start = std::time::Instant::now();
    if !active_snapshot.needs_hydration(workspace_id).await {
        return Ok(());
    }
    let workspace_exists_start = std::time::Instant::now();
    let workspace_exists = match global_store.get_workspace(workspace_id).await {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(err) => {
            tracing::warn!(
                workspace_id = ?workspace_id,
                "failed to check workspace existence before hydration: {err:#}"
            );
            return Err(WorkspaceHydrationError::Load(err));
        }
    };
    let workspace_exists_ms = workspace_exists_start.elapsed().as_millis();
    if !workspace_exists {
        return Err(WorkspaceHydrationError::NotFound);
    }
    let lookup_store_start = std::time::Instant::now();
    let store = match workspace_stores.lookup_workspace_store(workspace_id).await {
        StoreLookup::Found(store) => store,
        StoreLookup::Missing | StoreLookup::Deleting => {
            return Err(WorkspaceHydrationError::NotFound);
        }
        StoreLookup::Unavailable(err) => {
            tracing::warn!(
                workspace_id = ?workspace_id,
                err = %err,
                "failed to hydrate workspace snapshot (store lookup)"
            );
            return Err(WorkspaceHydrationError::Load(err));
        }
    };
    let lookup_store_ms = lookup_store_start.elapsed().as_millis();
    let load_payload_start = std::time::Instant::now();
    let payload = match load_workspace_snapshot_hydration_payload(&store, workspace_id).await {
        Ok(payload) => payload,
        Err(err) => {
            tracing::warn!(
                workspace_id = ?workspace_id,
                "failed to load workspace snapshot hydration payload: {err:#}"
            );
            return Err(WorkspaceHydrationError::Load(err));
        }
    };
    let load_payload_ms = load_payload_start.elapsed().as_millis();
    let active_task_count = payload.tasks.len();
    let active_head_count = payload.heads.len();
    let apply_payload_start = std::time::Instant::now();
    apply_workspace_snapshot_hydration_payload(active_snapshot, workspace_id, payload).await;
    let apply_payload_ms = apply_payload_start.elapsed().as_millis();
    if std::env::var_os("CTX_DEBUG_WORKSPACE_STREAM_TIMINGS").is_some() {
        eprintln!(
            "CTX_WS_TIMING hydration workspace_id={} workspace_exists_ms={} lookup_store_ms={} load_payload_ms={} apply_payload_ms={} active_tasks={} active_heads={} total_ms={}",
            workspace_id.0,
            workspace_exists_ms,
            lookup_store_ms,
            load_payload_ms,
            apply_payload_ms,
            active_task_count,
            active_head_count,
            hydration_start.elapsed().as_millis(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;
