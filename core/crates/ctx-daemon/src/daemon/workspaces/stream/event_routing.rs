use std::collections::HashSet;

use ctx_core::ids::SessionId;
use ctx_core::models::{WorkspaceActiveSnapshotEvent, WorkspaceActiveTaskSummary};
use ctx_workspace_active_snapshot::WorkspaceActiveSubscriptionState;
pub use ctx_workspace_stream_service::event_routing::{
    event_blocks_pending_replay, event_snapshot_rev, plan_workspace_stream_event_route,
    primary_session_id_for_active_task_event, WorkspaceStreamControlLane,
    WorkspaceStreamEventRoutePlan, WorkspaceStreamHeadLane,
};
#[cfg(test)]
pub use ctx_workspace_stream_service::event_routing::{
    event_blocks_pending_replay_with_active_task_sessions, filter_partial_delta_for_active_tasks,
    is_priority_control_event, should_stream_head_delta,
};

use crate::daemon::WorkspaceStreamHandle;

impl WorkspaceStreamHandle {
    pub fn primary_session_id_for_active_task_event(
        &self,
        task: &WorkspaceActiveTaskSummary,
    ) -> SessionId {
        primary_session_id_for_active_task_event(task)
    }

    pub fn event_snapshot_rev(&self, event: &WorkspaceActiveSnapshotEvent) -> Option<i64> {
        event_snapshot_rev(event)
    }

    pub fn event_blocks_pending_replay(
        &self,
        event: &WorkspaceActiveSnapshotEvent,
        pending_replay_sessions: &HashSet<SessionId>,
        subscription_state: &WorkspaceActiveSubscriptionState,
    ) -> bool {
        event_blocks_pending_replay(event, pending_replay_sessions, subscription_state)
    }

    pub fn plan_workspace_stream_event_route(
        &self,
        subscription_state: &WorkspaceActiveSubscriptionState,
        event: WorkspaceActiveSnapshotEvent,
    ) -> WorkspaceStreamEventRoutePlan {
        plan_workspace_stream_event_route(subscription_state, event)
    }
}
