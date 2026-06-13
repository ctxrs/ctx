use super::lifecycle::{clear_runtime_queues, queue_workspace_stream_reset};
use super::*;
use ctx_workspace_stream_service::replay::WorkspaceStreamReplayDrainHook;
use ctx_workspace_stream_service::subscriptions::planning::WorkspaceStreamSubscriptionTransactionPlan;
use ctx_workspace_stream_service::subscriptions::WorkspaceStreamSubscriptionResolutionError;
use std::collections::HashSet;

mod replay;
#[cfg(test)]
mod tests;

use replay::{
    drain_live_events_blocking_pending_replay, replay_should_stop,
    replay_workspace_stream_subscriptions, WorkspaceStreamReplayRequest,
};

struct ReplayPlanningDrainHook<'a> {
    state: &'a WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    live_rx: &'a mut tokio::sync::broadcast::Receiver<WorkspaceActiveSnapshotEvent>,
    runtime: &'a mut WorkspaceStreamRuntime,
    labels: &'a WorkspaceStreamLabels,
    deferred_live_events: &'a mut Vec<WorkspaceActiveSnapshotEvent>,
}

#[async_trait::async_trait]
impl WorkspaceStreamReplayDrainHook for ReplayPlanningDrainHook<'_> {
    type Error = ();

    async fn before_workspace_stream_replay_step(
        &mut self,
        pending_replay_sessions: &HashSet<SessionId>,
    ) -> Result<(), Self::Error> {
        drain_live_events_blocking_pending_replay(
            self.state,
            self.workspace_id,
            self.live_rx,
            self.runtime,
            self.labels,
            self.deferred_live_events,
            pending_replay_sessions,
        )
        .await
    }

    fn live_subscription_cursor(&self, session_id: SessionId) -> Option<SessionReplayCursor> {
        self.runtime
            .subscriptions
            .get(&session_id)
            .map(|cursor| cursor.last_sent)
    }
}

pub(crate) async fn handle_workspace_stream_subscription(
    state: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    message: WorkspaceActiveSnapshotClientMessage,
    live_rx: &mut tokio::sync::broadcast::Receiver<WorkspaceActiveSnapshotEvent>,
    runtime: &mut WorkspaceStreamRuntime,
    labels: &WorkspaceStreamLabels,
) -> Result<(), ()> {
    let existing_replay_cursors = runtime
        .subscriptions
        .iter()
        .map(|(session_id, cursor)| (*session_id, cursor.last_sent))
        .collect::<HashMap<_, _>>();
    let apply_plan = match state
        .plan_workspace_stream_subscription_transaction(
            workspace_id,
            message,
            &existing_replay_cursors,
            runtime.last_subscription_fingerprint.as_deref(),
        )
        .await
    {
        Ok(WorkspaceStreamSubscriptionTransactionPlan::NoChange) => return Ok(()),
        Ok(WorkspaceStreamSubscriptionTransactionPlan::Apply(plan)) => plan,
        Err(WorkspaceStreamSubscriptionResolutionError::Hydration) => {
            tracing::error!(
                target: "ctx_http.ws_active_snapshot",
                workspace_id = %workspace_id.0,
                "workspace stream hydration failed"
            );
            return Err(());
        }
        Err(WorkspaceStreamSubscriptionResolutionError::Resolution) => {
            tracing::error!(
                target: "ctx_http.ws_active_snapshot",
                workspace_id = %workspace_id.0,
                "{}",
                labels.subscribe_resolution_log,
            );
            queue_workspace_stream_reset(state, workspace_id, runtime).await?;
            return Ok(());
        }
    };
    let include_initial_snapshot = apply_plan.include_initial_snapshot;
    let fingerprint = apply_plan.fingerprint;

    clear_runtime_queues(runtime).await;
    runtime.reset_queued = false;
    runtime.send_control.clear_disconnect_after_flush();
    runtime.subscriptions = apply_plan
        .provisional_subscriptions
        .iter()
        .map(|(session_id, last_sent)| {
            (
                *session_id,
                SessionCursor {
                    last_sent: *last_sent,
                },
            )
        })
        .collect();
    runtime.subscription_state = apply_plan.state.clone();
    state
        .apply_workspace_stream_session_pin_changes(&apply_plan.pin_changes)
        .await;
    let active_head_cursors = if include_initial_snapshot {
        runtime.send_control.set_hydrating();
        let read_model = if let Ok(read_model) =
            queue_snapshot_payload(&runtime.control, state, workspace_id).await
        {
            read_model
        } else {
            return Err(());
        };
        state.active_head_cursors_from_snapshot_read_model(&read_model)
    } else {
        HashMap::new()
    };
    let replay_live_cursors = runtime
        .subscriptions
        .iter()
        .map(|(session_id, cursor)| (*session_id, cursor.last_sent))
        .collect::<HashMap<_, _>>();
    let mut initial_deferred_live_events = Vec::new();
    let replay_program = {
        let mut replay_planning_drain_hook = ReplayPlanningDrainHook {
            state,
            workspace_id,
            live_rx,
            runtime,
            labels,
            deferred_live_events: &mut initial_deferred_live_events,
        };
        state
            .plan_workspace_stream_replay_program_with_step_hook(
                workspace_id,
                &apply_plan.sessions,
                &replay_live_cursors,
                &active_head_cursors,
                include_initial_snapshot,
                &mut replay_planning_drain_hook,
            )
            .await?
    };
    if replay_should_stop(runtime) {
        return Ok(());
    }

    let Some(next_map) = replay_workspace_stream_subscriptions(WorkspaceStreamReplayRequest {
        state,
        workspace_id,
        runtime,
        labels,
        live_rx,
        replay_program,
        initial_deferred_live_events,
    })
    .await?
    else {
        return Ok(());
    };

    let live_cursors = runtime
        .subscriptions
        .iter()
        .map(|(session_id, cursor)| (*session_id, cursor.last_sent))
        .collect::<HashMap<_, _>>();
    let replayed_cursors = next_map
        .into_iter()
        .map(|(session_id, cursor)| (session_id, cursor.last_sent))
        .collect::<HashMap<_, _>>();
    let finalization = state.finalize_workspace_stream_subscription_replay(
        &runtime.subscription_state,
        &live_cursors,
        replayed_cursors,
        &apply_plan.sessions,
    );
    state
        .apply_workspace_stream_session_pin_changes(&finalization.pin_changes)
        .await;
    runtime.subscriptions = finalization
        .subscriptions
        .into_iter()
        .map(|(session_id, last_sent)| (session_id, SessionCursor { last_sent }))
        .collect();
    runtime.last_subscription_fingerprint = Some(fingerprint);
    Ok(())
}
