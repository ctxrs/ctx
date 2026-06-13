use std::collections::{HashMap, HashSet};

use ctx_core::ids::{SessionId, WorkspaceId};
use ctx_core::models::{
    WorkspaceActiveSnapshotEvent, WorkspaceActiveSnapshotStreamMessage,
    WorkspaceActiveSnapshotStreamSource,
};
use ctx_workspace_active_snapshot::{
    SessionReplayCursor, WorkspaceSessionReplay, WorkspaceSessionReplayItem,
};
use ctx_workspace_stream_service::replay::{
    self as stream_replay_service, WorkspaceStreamSessionReplayOutcome,
};
pub use ctx_workspace_stream_service::replay::{
    WorkspaceStreamReplayDrainHook, WorkspaceStreamReplayProgram, WorkspaceStreamReplayStep,
};

use crate::daemon::WorkspaceStreamHandle;

use super::replay_cursor::session_replay_tail_cursor;
use super::subscriptions::WorkspaceStreamResolvedSession;

struct NoopWorkspaceStreamReplayStepHook;

#[async_trait::async_trait]
impl WorkspaceStreamReplayDrainHook for NoopWorkspaceStreamReplayStepHook {
    type Error = std::convert::Infallible;

    async fn before_workspace_stream_replay_step(
        &mut self,
        _pending_replay_sessions: &HashSet<SessionId>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

struct DaemonWorkspaceStreamReplayStepHookAdapter<'a, H> {
    handle: &'a WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    inner: &'a mut H,
}

#[async_trait::async_trait]
impl<H> stream_replay_service::WorkspaceStreamReplayStepHook
    for DaemonWorkspaceStreamReplayStepHookAdapter<'_, H>
where
    H: WorkspaceStreamReplayDrainHook + Send,
{
    type Error = H::Error;

    async fn before_workspace_stream_replay_step(
        &mut self,
        pending_replay_sessions: &HashSet<SessionId>,
    ) -> Result<(), Self::Error> {
        self.inner
            .before_workspace_stream_replay_step(pending_replay_sessions)
            .await
    }

    fn live_subscription_cursor(&self, session_id: SessionId) -> Option<SessionReplayCursor> {
        self.inner.live_subscription_cursor(session_id)
    }

    async fn head_only_tail_cursor(
        &mut self,
        session_id: SessionId,
    ) -> Result<SessionReplayCursor, Self::Error> {
        Ok(session_replay_tail_cursor(self.handle, self.workspace_id, session_id).await)
    }
}

const SESSION_REPLAY_HEAD_SEED_LIMIT: u32 = 60;
// Workspace streams render a bounded session head, not an audit-log replay. If a
// subscriber misses more deltas than a recoverable head window, replaying each
// stale delta floods the per-socket head queue and delays fresh foreground
// traffic. Let replay_session_stream turn larger gaps into gap+seed recovery.
const SESSION_REPLAY_DELTA_LIMIT: usize = SESSION_REPLAY_HEAD_SEED_LIMIT as usize;

pub async fn plan_workspace_stream_replay_program(
    handle: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    resolved_sessions: &[WorkspaceStreamResolvedSession],
    live_subscriptions: &HashMap<SessionId, SessionReplayCursor>,
    active_head_cursors: &HashMap<SessionId, SessionReplayCursor>,
    include_initial_snapshot: bool,
) -> WorkspaceStreamReplayProgram {
    let mut hook = NoopWorkspaceStreamReplayStepHook;
    plan_workspace_stream_replay_program_with_step_hook(
        handle,
        workspace_id,
        resolved_sessions,
        live_subscriptions,
        active_head_cursors,
        include_initial_snapshot,
        &mut hook,
    )
    .await
    .unwrap_or_else(|never| match never {})
}

pub async fn plan_workspace_stream_replay_program_with_step_hook<H>(
    handle: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    resolved_sessions: &[WorkspaceStreamResolvedSession],
    live_subscriptions: &HashMap<SessionId, SessionReplayCursor>,
    active_head_cursors: &HashMap<SessionId, SessionReplayCursor>,
    include_initial_snapshot: bool,
    step_hook: &mut H,
) -> Result<WorkspaceStreamReplayProgram, H::Error>
where
    H: WorkspaceStreamReplayDrainHook + Send,
{
    let mut service_hook = DaemonWorkspaceStreamReplayStepHookAdapter {
        handle,
        workspace_id,
        inner: step_hook,
    };
    stream_replay_service::plan_workspace_stream_replay_program_with_step_hook(
        resolved_sessions,
        live_subscriptions,
        active_head_cursors,
        include_initial_snapshot,
        &mut service_hook,
    )
    .await
}

pub async fn replay_session_events<F, Fut>(
    handle: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    session_id: SessionId,
    after_cursor: SessionReplayCursor,
    list_failpoint: &'static str,
    send_failpoint: Option<&'static str>,
    mut emit: F,
) -> Result<WorkspaceStreamSessionReplayOutcome, ()>
where
    F: FnMut(WorkspaceActiveSnapshotStreamMessage) -> Fut,
    Fut: std::future::Future<Output = Result<(), ()>>,
{
    let (snapshot_rev, _) = handle
        .load_workspace_active_snapshot_state(workspace_id)
        .await;
    if crate::fault_injection::maybe_fail(list_failpoint).is_err() {
        return Ok(WorkspaceStreamSessionReplayOutcome::ResetRequired);
    }
    let replay = handle
        .active_snapshot()
        .replay_session_stream(
            workspace_id,
            session_id,
            after_cursor.last_event_seq,
            after_cursor.projection_rev,
            SESSION_REPLAY_DELTA_LIMIT,
        )
        .await;
    match replay {
        WorkspaceSessionReplay::Replay {
            mut items,
            mut last_sent,
        } => {
            let saw_gap = items
                .iter()
                .any(|item| matches!(item, WorkspaceSessionReplayItem::Gap { .. }));
            let saw_seed = items
                .iter()
                .any(|item| matches!(item, WorkspaceSessionReplayItem::Seed(_)));
            if saw_gap && !saw_seed {
                let store = handle
                    .session_store_allow_archived(session_id)
                    .await
                    .map_err(|_| ())?;
                if let Ok(Some(head)) = store
                    .get_session_head_snapshot(session_id, SESSION_REPLAY_HEAD_SEED_LIMIT, true)
                    .await
                {
                    handle
                        .active_snapshot()
                        .update_session_head(head.clone())
                        .await;
                    last_sent = SessionReplayCursor::from_head(&head);
                    items.push(WorkspaceSessionReplayItem::Seed(Box::new(head)));
                }
            }
            let seeded_session_ids = items
                .iter()
                .filter_map(|item| match item {
                    WorkspaceSessionReplayItem::Seed(head) => Some(head.session.id),
                    _ => None,
                })
                .collect::<Vec<_>>();
            for item in items {
                if matches!(item, WorkspaceSessionReplayItem::Delta(_)) {
                    if let Some(label) = send_failpoint {
                        crate::fault_injection::maybe_fail(label).map_err(|_| ())?;
                    }
                }
                let event = match item {
                    WorkspaceSessionReplayItem::Delta(delta) => {
                        WorkspaceActiveSnapshotEvent::SessionHeadDelta {
                            workspace_id,
                            snapshot_rev,
                            delta,
                        }
                    }
                    WorkspaceSessionReplayItem::Gap {
                        session_id,
                        after_seq,
                        reason,
                    } => WorkspaceActiveSnapshotEvent::SessionGap {
                        workspace_id,
                        snapshot_rev,
                        session_id,
                        after_seq,
                        reason,
                        seed_follows: seeded_session_ids.contains(&session_id),
                    },
                    WorkspaceSessionReplayItem::Seed(head) => {
                        WorkspaceActiveSnapshotEvent::SessionHeadSeed {
                            workspace_id,
                            snapshot_rev,
                            head,
                        }
                    }
                };
                emit(WorkspaceActiveSnapshotStreamMessage::Event {
                    rev: 0,
                    event: Box::new(event),
                    stream_source: Some(WorkspaceActiveSnapshotStreamSource::Replay),
                })
                .await?;
            }
            Ok(WorkspaceStreamSessionReplayOutcome::Replay { last_sent })
        }
        WorkspaceSessionReplay::ResetRequired => {
            Ok(WorkspaceStreamSessionReplayOutcome::ResetRequired)
        }
    }
}

impl WorkspaceStreamHandle {
    pub async fn plan_workspace_stream_replay_program(
        &self,
        workspace_id: WorkspaceId,
        resolved_sessions: &[WorkspaceStreamResolvedSession],
        live_subscriptions: &HashMap<SessionId, SessionReplayCursor>,
        active_head_cursors: &HashMap<SessionId, SessionReplayCursor>,
        include_initial_snapshot: bool,
    ) -> WorkspaceStreamReplayProgram {
        plan_workspace_stream_replay_program(
            self,
            workspace_id,
            resolved_sessions,
            live_subscriptions,
            active_head_cursors,
            include_initial_snapshot,
        )
        .await
    }

    pub async fn plan_workspace_stream_replay_program_with_step_hook<H>(
        &self,
        workspace_id: WorkspaceId,
        resolved_sessions: &[WorkspaceStreamResolvedSession],
        live_subscriptions: &HashMap<SessionId, SessionReplayCursor>,
        active_head_cursors: &HashMap<SessionId, SessionReplayCursor>,
        include_initial_snapshot: bool,
        step_hook: &mut H,
    ) -> Result<WorkspaceStreamReplayProgram, H::Error>
    where
        H: WorkspaceStreamReplayDrainHook + Send,
    {
        plan_workspace_stream_replay_program_with_step_hook(
            self,
            workspace_id,
            resolved_sessions,
            live_subscriptions,
            active_head_cursors,
            include_initial_snapshot,
            step_hook,
        )
        .await
    }

    pub async fn replay_session_events<F, Fut>(
        &self,
        workspace_id: WorkspaceId,
        session_id: SessionId,
        after_cursor: SessionReplayCursor,
        list_failpoint: &'static str,
        send_failpoint: Option<&'static str>,
        emit: F,
    ) -> Result<WorkspaceStreamSessionReplayOutcome, ()>
    where
        F: FnMut(WorkspaceActiveSnapshotStreamMessage) -> Fut,
        Fut: std::future::Future<Output = Result<(), ()>>,
    {
        replay_session_events(
            self,
            workspace_id,
            session_id,
            after_cursor,
            list_failpoint,
            send_failpoint,
            emit,
        )
        .await
    }
}
