mod access;
mod cursor_acceptance;
mod event_routing;
mod read_model;
mod replay;
mod replay_cursor;
mod runtime_facade;
mod subscriptions;
mod vcs;

pub use access::{WorkspaceStreamAccessError, WorkspaceStreamRouteAdmission};
pub use ctx_workspace_stream_service::subscriptions::WorkspaceStreamSubscriptionResolutionError;
pub use cursor_acceptance::WorkspaceStreamCursorAcceptance;
#[cfg(test)]
pub(in crate::daemon) use cursor_acceptance::{
    accept_session_delta_cursor, accept_session_head_cursor, is_session_head_delta_after_cursor,
    is_session_summary_delta_after_cursor, merge_replayed_and_live_subscription_cursors,
};
#[cfg(test)]
pub(in crate::daemon) use event_routing::{
    event_blocks_pending_replay, event_snapshot_rev, plan_workspace_stream_event_route,
};
#[cfg(test)]
pub(in crate::daemon) use event_routing::{
    event_blocks_pending_replay_with_active_task_sessions, filter_partial_delta_for_active_tasks,
    is_priority_control_event, should_stream_head_delta,
};
pub use event_routing::{
    WorkspaceStreamControlLane, WorkspaceStreamEventRoutePlan, WorkspaceStreamHeadLane,
};
pub use read_model::{
    initial_stream_state, load_initial_snapshot_read_model, prepare_subscription_read_model,
    WorkspaceStreamInitialState, WorkspaceStreamSnapshotReadModel,
};
pub use replay::{
    plan_workspace_stream_replay_program, plan_workspace_stream_replay_program_with_step_hook,
    replay_session_events, WorkspaceStreamReplayDrainHook, WorkspaceStreamReplayProgram,
    WorkspaceStreamReplayStep,
};
pub use replay_cursor::active_head_cursors_from_snapshot_read_model;
pub(in crate::daemon) use replay_cursor::active_task_subscription_cursor;
#[cfg(test)]
pub(in crate::daemon) use replay_cursor::{
    head_only_snapshot_cursor, plan_resume_replay_cursor, WorkspaceStreamResumeReplayCursorPlan,
};
pub use subscriptions::{
    apply_workspace_stream_live_event, apply_workspace_stream_subscription_event,
    finalize_workspace_stream_subscription_replay, plan_workspace_stream_subscription,
    plan_workspace_stream_subscription_transaction,
    resolve_workspace_active_snapshot_subscriptions, WorkspaceStreamLiveEventApplication,
    WorkspaceStreamResolvedSession, WorkspaceStreamSessionPinChanges, WorkspaceStreamSessionReplay,
    WorkspaceStreamSubscriptionApplyPlan, WorkspaceStreamSubscriptionEventApplication,
    WorkspaceStreamSubscriptionPlan, WorkspaceStreamSubscriptionReplayFinalization,
    WorkspaceStreamSubscriptionTransactionPlan,
};
pub use vcs::{
    filter_workspace_worktree_ids, plan_workspace_vcs_lag_reseed, plan_workspace_vcs_refresh,
    plan_workspace_vcs_subscription_update, refresh_worktree_vcs_for_worktrees,
    release_workspace_vcs_demand, route_workspace_vcs_snapshot, WorkspaceVcsDemandState,
    WorkspaceVcsLagReseedPlan, WorkspaceVcsRefreshPlan, WorkspaceVcsSnapshotRoute,
    WorkspaceVcsSnapshotSeed, WorkspaceVcsStreamRuntime, WorkspaceVcsSubscriptionPlan,
};

#[cfg(test)]
mod tests;
