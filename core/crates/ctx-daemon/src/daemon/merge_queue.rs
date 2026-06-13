use std::sync::Arc;

use anyhow::Result;

use ctx_core::ids::{MergeQueueEntryId, WorkspaceId};
use ctx_core::models::MergeQueueEntry;

use ctx_merge_queue::MergeQueueSubmitParams;
#[cfg(test)]
use ctx_merge_queue::WorkspaceDrainStop;

use crate::daemon::{merge_queue_route_host_from_state, DaemonState};

mod host;
mod route_contract;
mod submit_route;

pub(in crate::daemon) use host::MergeQueueRouteHost;

pub(in crate::daemon) fn route_host_from_state(state: &DaemonState) -> Arc<MergeQueueRouteHost> {
    merge_queue_route_host_from_state(state)
}

pub async fn get_workspace_merge_queue_entry(
    state: &DaemonState,
    workspace_id: WorkspaceId,
    entry_id: MergeQueueEntryId,
) -> Result<MergeQueueEntry> {
    let host = route_host_from_state(state);
    ctx_merge_queue::get_workspace_merge_queue_entry::<MergeQueueRouteHost>(
        host.as_ref(),
        workspace_id,
        entry_id,
    )
    .await
}

pub async fn submit_merge_queue_entry(
    state: &Arc<DaemonState>,
    params: MergeQueueSubmitParams,
) -> Result<MergeQueueEntry> {
    let host = route_host_from_state(state.as_ref());
    ctx_merge_queue::submit_merge_queue_entry::<MergeQueueRouteHost>(&host, params).await
}

pub async fn cancel_merge_queue_entry(
    state: &Arc<DaemonState>,
    workspace_id: WorkspaceId,
    entry_id: MergeQueueEntryId,
) -> Result<MergeQueueEntry> {
    let host = route_host_from_state(state.as_ref());
    ctx_merge_queue::cancel_merge_queue_entry::<MergeQueueRouteHost>(&host, workspace_id, entry_id)
        .await
}

pub async fn retry_merge_queue_entry(
    state: &Arc<DaemonState>,
    workspace_id: WorkspaceId,
    entry_id: MergeQueueEntryId,
) -> Result<MergeQueueEntry> {
    let host = route_host_from_state(state.as_ref());
    ctx_merge_queue::retry_merge_queue_entry::<MergeQueueRouteHost>(&host, workspace_id, entry_id)
        .await
}

pub fn spawn_merge_queue_runner(state: Arc<DaemonState>) {
    let host = route_host_from_state(state.as_ref());
    ctx_merge_queue::spawn_merge_queue_runner::<MergeQueueRouteHost>(host);
}

pub async fn schedule_workspace_if_enabled_and_queued(
    state: &Arc<DaemonState>,
    workspace_id: WorkspaceId,
) -> Result<bool> {
    let host = route_host_from_state(state.as_ref());
    ctx_merge_queue::schedule_workspace_if_enabled_and_queued::<MergeQueueRouteHost>(
        &host,
        workspace_id,
    )
    .await
}

pub async fn activate_workspace_merge_queue(state: &Arc<DaemonState>, workspace_id: WorkspaceId) {
    let host = route_host_from_state(state.as_ref());
    ctx_merge_queue::activate_workspace_merge_queue::<MergeQueueRouteHost>(&host, workspace_id)
        .await;
}

pub async fn cancel_queued_entries_for_disabled_workspace(
    state: &Arc<DaemonState>,
    store: &ctx_store::Store,
    workspace_id: WorkspaceId,
) -> Result<()> {
    let host = route_host_from_state(state.as_ref());
    ctx_merge_queue::cancel_queued_entries_for_disabled_workspace::<MergeQueueRouteHost>(
        &host,
        store,
        workspace_id,
    )
    .await
}

#[cfg(test)]
pub async fn list_queued_entries_for_workspace(
    state: &DaemonState,
    workspace_id: WorkspaceId,
) -> Result<Vec<MergeQueueEntry>> {
    let host = route_host_from_state(state);
    ctx_merge_queue::list_queued_entries_for_workspace::<MergeQueueRouteHost>(
        host.as_ref(),
        workspace_id,
    )
    .await
}

#[cfg(test)]
pub async fn begin_workspace_drain(state: &DaemonState, workspace_id: WorkspaceId) -> bool {
    let host = route_host_from_state(state);
    ctx_merge_queue::begin_workspace_drain::<MergeQueueRouteHost>(host.as_ref(), workspace_id).await
}

#[cfg(test)]
pub async fn finish_workspace_drain(state: &DaemonState, workspace_id: WorkspaceId) -> bool {
    let host = route_host_from_state(state);
    ctx_merge_queue::finish_workspace_drain::<MergeQueueRouteHost>(host.as_ref(), workspace_id)
        .await
}

#[cfg(test)]
pub async fn schedule_workspace_drain(state: &Arc<DaemonState>, workspace_id: WorkspaceId) {
    let host = route_host_from_state(state.as_ref());
    ctx_merge_queue::schedule_workspace_drain::<MergeQueueRouteHost>(&host, workspace_id).await;
}

#[cfg(test)]
pub async fn reschedule_workspace_after_drain(
    state: &Arc<DaemonState>,
    workspace_id: WorkspaceId,
    stop: WorkspaceDrainStop,
) -> bool {
    let host = route_host_from_state(state.as_ref());
    ctx_merge_queue::reschedule_workspace_after_drain::<MergeQueueRouteHost>(
        &host,
        workspace_id,
        stop,
    )
    .await
}

#[cfg(test)]
mod tests;
