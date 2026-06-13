use super::burst::{
    handle_workspace_stream_receiver_burst, take_workspace_stream_receiver_burst,
    WorkspaceStreamReceiverBurst,
};
use super::metrics::record_workspace_stream_receiver_drain;
use super::*;

pub(crate) async fn drain_pending_workspace_stream_receiver_burst_deferring<F>(
    state: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    rx: &mut tokio::sync::broadcast::Receiver<WorkspaceActiveSnapshotEvent>,
    runtime: &mut WorkspaceStreamRuntime,
    labels: &WorkspaceStreamLabels,
    deferred_events: &mut Vec<WorkspaceActiveSnapshotEvent>,
    mut should_defer: F,
) -> Result<(), ()>
where
    F: FnMut(&WorkspaceActiveSnapshotEvent) -> bool,
{
    match rx.try_recv() {
        Ok(event) => {
            let burst = take_workspace_stream_receiver_burst(rx, event);
            handle_workspace_stream_receiver_burst_deferring(
                state,
                workspace_id,
                burst,
                runtime,
                labels,
                deferred_events,
                &mut should_defer,
            )
            .await
        }
        Err(tokio::sync::broadcast::error::TryRecvError::Empty) => Ok(()),
        Err(tokio::sync::broadcast::error::TryRecvError::Lagged(lagged)) => {
            handle_workspace_stream_lagged(state, workspace_id, lagged, runtime, labels).await
        }
        Err(tokio::sync::broadcast::error::TryRecvError::Closed) => Err(()),
    }
}

pub(crate) async fn flush_deferred_workspace_stream_receiver_events<F>(
    state: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    runtime: &mut WorkspaceStreamRuntime,
    labels: &WorkspaceStreamLabels,
    deferred_events: &mut Vec<WorkspaceActiveSnapshotEvent>,
    mut should_defer: F,
) -> Result<(), ()>
where
    F: FnMut(&WorkspaceActiveSnapshotEvent) -> bool,
{
    let ready_count = deferred_events
        .iter()
        .take_while(|event| !should_defer(event))
        .count();
    if ready_count == 0 {
        return Ok(());
    }
    let events = deferred_events.drain(..ready_count).collect::<Vec<_>>();
    handle_workspace_stream_receiver_burst(
        state,
        workspace_id,
        WorkspaceStreamReceiverBurst {
            events,
            lagged: None,
            closed: false,
            hit_limit: false,
        },
        runtime,
        labels,
    )
    .await
}

async fn handle_workspace_stream_receiver_burst_deferring<F>(
    state: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    burst: WorkspaceStreamReceiverBurst,
    runtime: &mut WorkspaceStreamRuntime,
    labels: &WorkspaceStreamLabels,
    deferred_events: &mut Vec<WorkspaceActiveSnapshotEvent>,
    should_defer: &mut F,
) -> Result<(), ()>
where
    F: FnMut(&WorkspaceActiveSnapshotEvent) -> bool,
{
    let event_count = burst.events.len();
    let hit_limit = burst.hit_limit;
    let mut ready_events = Vec::new();
    let mut defer_remaining = false;
    for event in burst.events {
        if defer_remaining || should_defer(&event) {
            defer_remaining = true;
            deferred_events.push(event);
        } else {
            ready_events.push(event);
        }
    }
    if !ready_events.is_empty() {
        handle_workspace_stream_receiver_burst(
            state,
            workspace_id,
            WorkspaceStreamReceiverBurst {
                events: ready_events,
                lagged: None,
                closed: false,
                hit_limit,
            },
            runtime,
            labels,
        )
        .await?;
    } else {
        record_workspace_stream_receiver_drain(state, labels, event_count, hit_limit).await;
    }
    if let Some(lagged) = burst.lagged {
        handle_workspace_stream_lagged(state, workspace_id, lagged, runtime, labels).await?;
    }
    if burst.closed {
        return Err(());
    }
    Ok(())
}
