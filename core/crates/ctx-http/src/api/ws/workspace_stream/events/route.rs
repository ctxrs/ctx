use super::super::lifecycle::queue_workspace_stream_reset;
use super::super::*;
use ctx_workspace_stream_service::event_routing::{
    WorkspaceStreamControlLane, WorkspaceStreamEventRoutePlan, WorkspaceStreamHeadLane,
};

#[cfg(test)]
mod tests;

pub(super) async fn push_workspace_stream_event_route_plan(
    state: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    route_plan: WorkspaceStreamEventRoutePlan,
    runtime: &mut WorkspaceStreamRuntime,
    labels: &WorkspaceStreamLabels,
) -> Result<(), ()> {
    match route_plan {
        WorkspaceStreamEventRoutePlan::Drop => Ok(()),
        WorkspaceStreamEventRoutePlan::HeadDelta {
            snapshot_rev,
            delta,
            lane,
        } => {
            push_planned_head_delta(
                state,
                workspace_id,
                snapshot_rev,
                *delta,
                lane,
                runtime,
                labels,
            )
            .await
        }
        WorkspaceStreamEventRoutePlan::Summary { event } => {
            push_planned_summary_delta(state, workspace_id, event, runtime, labels).await
        }
        WorkspaceStreamEventRoutePlan::Control {
            event,
            session_id,
            lane,
        } => {
            push_planned_control_event(
                state,
                workspace_id,
                session_id,
                event,
                lane,
                runtime,
                labels,
            )
            .await
        }
    }
}

async fn push_planned_head_delta(
    state: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    snapshot_rev: i64,
    delta: SessionHeadDelta,
    lane: WorkspaceStreamHeadLane,
    runtime: &mut WorkspaceStreamRuntime,
    labels: &WorkspaceStreamLabels,
) -> Result<(), ()> {
    let head_buffer = match lane {
        WorkspaceStreamHeadLane::Foreground => &runtime.foreground_head_buffer,
        WorkspaceStreamHeadLane::Background => &runtime.background_head_buffer,
    };
    if let Err(error) = head_buffer.push(snapshot_rev, delta).await {
        log_head_batch_push_error(labels.event_queue_label, workspace_id, &error);
        if runtime.reset_queued {
            return Ok(());
        }
        queue_workspace_stream_reset(state, workspace_id, runtime).await?;
    }
    Ok(())
}

async fn push_planned_summary_delta(
    state: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    event: WorkspaceActiveSnapshotEvent,
    runtime: &mut WorkspaceStreamRuntime,
    labels: &WorkspaceStreamLabels,
) -> Result<(), ()> {
    if let Err(error) = runtime.summary_buffer.push(event).await {
        log_summary_batch_push_error(labels.event_queue_label, workspace_id, &error);
        if runtime.reset_queued {
            return Ok(());
        }
        queue_workspace_stream_reset(state, workspace_id, runtime).await?;
    }
    Ok(())
}

async fn push_planned_control_event(
    state: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    session_id: Option<SessionId>,
    event: WorkspaceActiveSnapshotEvent,
    lane: WorkspaceStreamControlLane,
    runtime: &mut WorkspaceStreamRuntime,
    labels: &WorkspaceStreamLabels,
) -> Result<(), ()> {
    let target = match lane {
        WorkspaceStreamControlLane::Priority => &runtime.priority_control,
        WorkspaceStreamControlLane::Normal => &runtime.control,
    };
    if push_stream_message(
        target,
        workspace_id,
        session_id,
        labels.event_queue_label,
        WorkspaceActiveSnapshotStreamMessage::Event {
            rev: 0,
            event: Box::new(event),
            stream_source: None,
        },
    )
    .await
    .is_err()
    {
        if runtime.reset_queued {
            return Ok(());
        }
        queue_workspace_stream_reset(state, workspace_id, runtime).await?;
    }
    Ok(())
}
