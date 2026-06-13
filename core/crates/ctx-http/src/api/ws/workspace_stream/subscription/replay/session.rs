use super::*;
use ctx_workspace_stream_service::event_routing::{
    WorkspaceStreamControlLane, WorkspaceStreamEventRoutePlan, WorkspaceStreamHeadLane,
};
use ctx_workspace_stream_service::replay::WorkspaceStreamSessionReplayOutcome;

#[cfg(test)]
mod tests;

pub(super) async fn replay_workspace_session(
    state: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    session_id: SessionId,
    replay_cursor: SessionReplayCursor,
    labels: &WorkspaceStreamLabels,
    next_state: &WorkspaceActiveSubscriptionState,
    runtime: &WorkspaceStreamRuntime,
) -> Result<WorkspaceStreamSessionReplayOutcome, ()> {
    let control = runtime.control.clone();
    let priority_control = runtime.priority_control.clone();
    let foreground_head_buffer = runtime.foreground_head_buffer.clone();
    let background_head_buffer = runtime.background_head_buffer.clone();
    let summary_buffer = runtime.summary_buffer.clone();
    let next_state = next_state.clone();

    state
        .replay_session_events(
            workspace_id,
            session_id,
            replay_cursor,
            labels.replay_list_metric,
            labels.replay_send_metric,
            move |event| {
                let control = control.clone();
                let priority_control = priority_control.clone();
                let foreground_head_buffer = foreground_head_buffer.clone();
                let background_head_buffer = background_head_buffer.clone();
                let summary_buffer = summary_buffer.clone();
                let next_state = next_state.clone();
                async move {
                    match event {
                        WorkspaceActiveSnapshotStreamMessage::Event { event, .. } => {
                            let sinks = ReplayRouteSinks {
                                background_head_buffer: &background_head_buffer,
                                control: &control,
                                foreground_head_buffer: &foreground_head_buffer,
                                priority_control: &priority_control,
                                summary_buffer: &summary_buffer,
                            };
                            push_replay_event_route_plan(
                                workspace_id,
                                labels,
                                &sinks,
                                state.plan_workspace_stream_event_route(&next_state, *event),
                            )
                            .await
                        }
                        other => {
                            push_stream_message(
                                &control,
                                workspace_id,
                                Some(session_id),
                                labels.replay_queue_label,
                                other,
                            )
                            .await
                        }
                    }
                }
            },
        )
        .await
}

struct ReplayRouteSinks<'a> {
    background_head_buffer: &'a HeadBatchBuffer,
    control: &'a StreamQueue<WorkspaceActiveSnapshotStreamMessage>,
    foreground_head_buffer: &'a HeadBatchBuffer,
    priority_control: &'a StreamQueue<WorkspaceActiveSnapshotStreamMessage>,
    summary_buffer: &'a SummaryBatchBuffer,
}

async fn push_replay_event_route_plan(
    workspace_id: WorkspaceId,
    labels: &WorkspaceStreamLabels,
    sinks: &ReplayRouteSinks<'_>,
    plan: WorkspaceStreamEventRoutePlan,
) -> Result<(), ()> {
    match plan {
        WorkspaceStreamEventRoutePlan::Drop => Ok(()),
        WorkspaceStreamEventRoutePlan::HeadDelta {
            snapshot_rev,
            delta,
            lane,
        } => {
            let head_buffer = match lane {
                WorkspaceStreamHeadLane::Foreground => sinks.foreground_head_buffer,
                WorkspaceStreamHeadLane::Background => sinks.background_head_buffer,
            };
            if let Err(error) = head_buffer
                .push_with_source(
                    snapshot_rev,
                    *delta,
                    WorkspaceActiveSnapshotStreamSource::Replay,
                )
                .await
            {
                log_head_batch_push_error(labels.replay_queue_label, workspace_id, &error);
                return Err(());
            }
            Ok(())
        }
        WorkspaceStreamEventRoutePlan::Summary { event } => {
            sinks
                .summary_buffer
                .push_with_source(event, WorkspaceActiveSnapshotStreamSource::Replay)
                .await
                .map_err(|error| {
                    log_summary_batch_push_error(labels.replay_queue_label, workspace_id, &error);
                })?;
            Ok(())
        }
        WorkspaceStreamEventRoutePlan::Control {
            event,
            session_id,
            lane,
        } => {
            let target = match lane {
                WorkspaceStreamControlLane::Priority => sinks.priority_control,
                WorkspaceStreamControlLane::Normal => sinks.control,
            };
            push_stream_message(
                target,
                workspace_id,
                session_id,
                labels.replay_queue_label,
                WorkspaceActiveSnapshotStreamMessage::Event {
                    rev: 0,
                    event: Box::new(event),
                    stream_source: Some(WorkspaceActiveSnapshotStreamSource::Replay),
                },
            )
            .await
        }
    }
}
