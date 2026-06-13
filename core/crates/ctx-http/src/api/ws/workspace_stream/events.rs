use super::lifecycle::queue_workspace_stream_reset;
use super::*;
use serde_json::json;

mod receiver;
mod route;

pub(crate) use receiver::{
    drain_pending_workspace_stream_receiver_burst_deferring,
    flush_deferred_workspace_stream_receiver_events, handle_workspace_stream_receiver_burst,
    take_workspace_stream_receiver_burst,
};
use route::push_workspace_stream_event_route_plan;

pub(crate) async fn handle_workspace_stream_lagged(
    state: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    lagged: u64,
    runtime: &mut WorkspaceStreamRuntime,
    labels: &WorkspaceStreamLabels,
) -> Result<(), ()> {
    if runtime.reset_queued {
        return Ok(());
    }
    tracing::error!(
        target: "ctx_http.ws_active_snapshot",
        workspace_id = %workspace_id.0,
        lagged,
        "{}",
        labels.lagged_log,
    );
    emit_workspace_stream_incident(
        state,
        "workspace_stream_lagged",
        workspace_id,
        &[
            ("lagged", json!(lagged)),
            ("queue_label", json!(labels.event_queue_label)),
        ],
    )
    .await;
    queue_workspace_stream_reset(state, workspace_id, runtime).await
}

pub(crate) async fn handle_workspace_stream_event(
    state: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    event: WorkspaceActiveSnapshotEvent,
    runtime: &mut WorkspaceStreamRuntime,
    labels: &WorkspaceStreamLabels,
) -> Result<(), ()> {
    if let Some(rev) = state.event_snapshot_rev(&event) {
        bump_latest_snapshot_rev(&runtime.latest_snapshot_rev, rev);
    }

    if runtime.reset_queued {
        return Ok(());
    }

    let current_subscriptions = runtime
        .subscriptions
        .iter()
        .map(|(session_id, cursor)| (*session_id, cursor.last_sent))
        .collect::<HashMap<_, _>>();
    let application = state
        .apply_workspace_stream_live_event(
            workspace_id,
            runtime.subscription_state.clone(),
            current_subscriptions,
            event,
        )
        .await;
    runtime.subscription_state = application.state;
    runtime.subscriptions = application
        .subscriptions
        .into_iter()
        .map(|(session_id, last_sent)| (session_id, SessionCursor { last_sent }))
        .collect();
    state
        .apply_workspace_stream_session_pin_changes(&application.pin_changes)
        .await;

    push_workspace_stream_event_route_plan(
        state,
        workspace_id,
        application.route_plan,
        runtime,
        labels,
    )
    .await
}
