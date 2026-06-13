use std::collections::{HashMap, HashSet};

use ctx_core::ids::SessionId;
use ctx_core::models::WorkspaceActiveSnapshotSessionIntent;
use ctx_workspace_active_snapshot::SessionReplayCursor;

use crate::replay_cursor::{plan_resume_replay_cursor, WorkspaceStreamResumeReplayCursorPlan};
use crate::subscriptions::planning::{
    WorkspaceStreamResolvedSession, WorkspaceStreamSessionReplay,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceStreamReplayProgram {
    pub pending_replay_sessions: HashSet<SessionId>,
    pub steps: Vec<WorkspaceStreamReplayStep>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceStreamReplayStep {
    HeadOnly {
        session_id: SessionId,
        cursor: SessionReplayCursor,
    },
    Replay {
        session_id: SessionId,
        after_seq: i64,
        after_projection_rev: i64,
        replay_cursor: SessionReplayCursor,
    },
    NoReplayRequired {
        session_id: SessionId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceStreamSessionReplayOutcome {
    Replay { last_sent: SessionReplayCursor },
    ResetRequired,
}

#[async_trait::async_trait]
pub trait WorkspaceStreamReplayDrainHook {
    type Error;

    async fn before_workspace_stream_replay_step(
        &mut self,
        pending_replay_sessions: &HashSet<SessionId>,
    ) -> Result<(), Self::Error>;

    fn live_subscription_cursor(&self, _session_id: SessionId) -> Option<SessionReplayCursor> {
        None
    }
}

#[async_trait::async_trait]
pub trait WorkspaceStreamReplayStepHook {
    type Error;

    async fn before_workspace_stream_replay_step(
        &mut self,
        pending_replay_sessions: &HashSet<SessionId>,
    ) -> Result<(), Self::Error>;

    fn live_subscription_cursor(&self, _session_id: SessionId) -> Option<SessionReplayCursor> {
        None
    }

    async fn head_only_tail_cursor(
        &mut self,
        session_id: SessionId,
    ) -> Result<SessionReplayCursor, Self::Error>;
}

pub async fn plan_workspace_stream_replay_program_with_step_hook<H>(
    resolved_sessions: &[WorkspaceStreamResolvedSession],
    live_subscriptions: &HashMap<SessionId, SessionReplayCursor>,
    active_head_cursors: &HashMap<SessionId, SessionReplayCursor>,
    include_initial_snapshot: bool,
    step_hook: &mut H,
) -> Result<WorkspaceStreamReplayProgram, H::Error>
where
    H: WorkspaceStreamReplayStepHook,
{
    let pending_replay_sessions = replay_pending_sessions(resolved_sessions);
    let mut steps = Vec::new();
    for subscription in resolved_sessions {
        step_hook
            .before_workspace_stream_replay_step(&pending_replay_sessions)
            .await?;
        match subscription.intent {
            WorkspaceActiveSnapshotSessionIntent::Head => {
                let session_id = subscription.session_id;
                let live_cursor = step_hook
                    .live_subscription_cursor(session_id)
                    .or_else(|| live_subscriptions.get(&session_id).copied());
                let cursor = head_only_snapshot_cursor(
                    session_id,
                    live_cursor,
                    active_head_cursors.get(&session_id).copied(),
                    include_initial_snapshot,
                    step_hook,
                )
                .await?;
                steps.push(WorkspaceStreamReplayStep::HeadOnly { session_id, cursor });
            }
            WorkspaceActiveSnapshotSessionIntent::Replay => {
                let WorkspaceStreamSessionReplay::Resume {
                    after_seq,
                    after_projection_rev,
                } = subscription.replay
                else {
                    continue;
                };
                let session_id = subscription.session_id;
                let live_cursor = step_hook
                    .live_subscription_cursor(session_id)
                    .or_else(|| live_subscriptions.get(&session_id).copied());
                match plan_resume_replay_cursor(live_cursor, after_seq, after_projection_rev) {
                    WorkspaceStreamResumeReplayCursorPlan::Replay { cursor } => {
                        steps.push(WorkspaceStreamReplayStep::Replay {
                            session_id,
                            after_seq,
                            after_projection_rev,
                            replay_cursor: cursor,
                        });
                    }
                    WorkspaceStreamResumeReplayCursorPlan::NoReplayRequired => {
                        steps.push(WorkspaceStreamReplayStep::NoReplayRequired { session_id });
                    }
                }
            }
        }
    }
    Ok(WorkspaceStreamReplayProgram {
        pending_replay_sessions,
        steps,
    })
}

async fn head_only_snapshot_cursor<H>(
    session_id: SessionId,
    live_cursor: Option<SessionReplayCursor>,
    snapshot_cursor: Option<SessionReplayCursor>,
    include_initial_snapshot: bool,
    step_hook: &mut H,
) -> Result<SessionReplayCursor, H::Error>
where
    H: WorkspaceStreamReplayStepHook,
{
    let snapshot_cursor = if include_initial_snapshot {
        snapshot_cursor
    } else {
        None
    };
    match snapshot_cursor {
        Some(cursor) => Ok(live_cursor.unwrap_or_default().cover(cursor)),
        None => {
            let current_tail = step_hook.head_only_tail_cursor(session_id).await?;
            Ok(live_cursor.unwrap_or_default().cover(current_tail))
        }
    }
}

fn replay_pending_sessions(
    resolved_sessions: &[WorkspaceStreamResolvedSession],
) -> HashSet<SessionId> {
    resolved_sessions
        .iter()
        .filter_map(|subscription| {
            if subscription.intent == WorkspaceActiveSnapshotSessionIntent::Replay
                && matches!(
                    subscription.replay,
                    WorkspaceStreamSessionReplay::Resume { .. }
                )
            {
                Some(subscription.session_id)
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor(last_event_seq: i64, projection_rev: i64) -> SessionReplayCursor {
        SessionReplayCursor {
            last_event_seq,
            projection_rev,
        }
    }

    fn resolved(
        session_id: SessionId,
        intent: WorkspaceActiveSnapshotSessionIntent,
        replay: WorkspaceStreamSessionReplay,
    ) -> WorkspaceStreamResolvedSession {
        WorkspaceStreamResolvedSession {
            session_id,
            intent,
            replay,
        }
    }

    #[derive(Default)]
    struct RecordingReplayStepHook {
        pending_sets: Vec<HashSet<SessionId>>,
        live_cursors: HashMap<SessionId, SessionReplayCursor>,
        tail_cursors: HashMap<SessionId, SessionReplayCursor>,
    }

    #[async_trait::async_trait]
    impl WorkspaceStreamReplayStepHook for RecordingReplayStepHook {
        type Error = ();

        async fn before_workspace_stream_replay_step(
            &mut self,
            pending_replay_sessions: &HashSet<SessionId>,
        ) -> Result<(), Self::Error> {
            self.pending_sets.push(pending_replay_sessions.clone());
            Ok(())
        }

        fn live_subscription_cursor(&self, session_id: SessionId) -> Option<SessionReplayCursor> {
            self.live_cursors.get(&session_id).copied()
        }

        async fn head_only_tail_cursor(
            &mut self,
            session_id: SessionId,
        ) -> Result<SessionReplayCursor, Self::Error> {
            Ok(self
                .tail_cursors
                .get(&session_id)
                .copied()
                .unwrap_or_default())
        }
    }

    #[tokio::test]
    async fn replay_program_computes_head_only_cursor_from_snapshot_or_tail() {
        let session_id = SessionId::new();
        let mut hook = RecordingReplayStepHook {
            tail_cursors: HashMap::from([(session_id, cursor(7, 8))]),
            ..Default::default()
        };

        let from_snapshot = plan_workspace_stream_replay_program_with_step_hook(
            &[resolved(
                session_id,
                WorkspaceActiveSnapshotSessionIntent::Head,
                WorkspaceStreamSessionReplay::Reset,
            )],
            &HashMap::from([(session_id, cursor(4, 5))]),
            &HashMap::from([(session_id, cursor(10, 3))]),
            true,
            &mut hook,
        )
        .await
        .unwrap();

        assert_eq!(
            from_snapshot.steps,
            vec![WorkspaceStreamReplayStep::HeadOnly {
                session_id,
                cursor: cursor(10, 5),
            }]
        );

        let mut hook = RecordingReplayStepHook {
            tail_cursors: HashMap::from([(session_id, cursor(7, 8))]),
            ..Default::default()
        };
        let from_tail = plan_workspace_stream_replay_program_with_step_hook(
            &[resolved(
                session_id,
                WorkspaceActiveSnapshotSessionIntent::Head,
                WorkspaceStreamSessionReplay::Reset,
            )],
            &HashMap::from([(session_id, cursor(4, 5))]),
            &HashMap::from([(session_id, cursor(10, 3))]),
            false,
            &mut hook,
        )
        .await
        .unwrap();

        assert_eq!(
            from_tail.steps,
            vec![WorkspaceStreamReplayStep::HeadOnly {
                session_id,
                cursor: cursor(7, 8),
            }]
        );
    }

    #[tokio::test]
    async fn replay_program_step_hook_runs_before_each_planned_step() {
        let head_session_id = SessionId::new();
        let replay_session_id = SessionId::new();
        let mut hook = RecordingReplayStepHook::default();

        let program = plan_workspace_stream_replay_program_with_step_hook(
            &[
                resolved(
                    head_session_id,
                    WorkspaceActiveSnapshotSessionIntent::Head,
                    WorkspaceStreamSessionReplay::Reset,
                ),
                resolved(
                    replay_session_id,
                    WorkspaceActiveSnapshotSessionIntent::Replay,
                    WorkspaceStreamSessionReplay::Resume {
                        after_seq: 10,
                        after_projection_rev: 12,
                    },
                ),
            ],
            &HashMap::from([
                (head_session_id, cursor(4, 5)),
                (replay_session_id, cursor(15, 16)),
            ]),
            &HashMap::new(),
            false,
            &mut hook,
        )
        .await
        .unwrap();

        assert_eq!(
            hook.pending_sets,
            vec![
                HashSet::from([replay_session_id]),
                HashSet::from([replay_session_id])
            ],
        );
        assert_eq!(
            program.pending_replay_sessions,
            HashSet::from([replay_session_id])
        );
        assert_eq!(program.steps.len(), 2);
    }

    #[tokio::test]
    async fn replay_program_uses_live_cursor_after_step_hook_runs() {
        let session_id = SessionId::new();
        let mut hook = RecordingReplayStepHook {
            live_cursors: HashMap::from([(session_id, cursor(15, 16))]),
            ..Default::default()
        };

        let program = plan_workspace_stream_replay_program_with_step_hook(
            &[resolved(
                session_id,
                WorkspaceActiveSnapshotSessionIntent::Replay,
                WorkspaceStreamSessionReplay::Resume {
                    after_seq: 10,
                    after_projection_rev: 12,
                },
            )],
            &HashMap::from([(session_id, cursor(5, 6))]),
            &HashMap::new(),
            false,
            &mut hook,
        )
        .await
        .unwrap();

        assert_eq!(
            program.steps,
            vec![WorkspaceStreamReplayStep::Replay {
                session_id,
                after_seq: 10,
                after_projection_rev: 12,
                replay_cursor: cursor(15, 16),
            }]
        );
    }

    #[tokio::test]
    async fn replay_program_keeps_noop_resume_as_initial_blocker_until_executed() {
        let session_id = SessionId::new();
        let mut hook = RecordingReplayStepHook::default();

        let program = plan_workspace_stream_replay_program_with_step_hook(
            &[resolved(
                session_id,
                WorkspaceActiveSnapshotSessionIntent::Replay,
                WorkspaceStreamSessionReplay::Resume {
                    after_seq: 10,
                    after_projection_rev: 12,
                },
            )],
            &HashMap::new(),
            &HashMap::new(),
            false,
            &mut hook,
        )
        .await
        .unwrap();

        assert_eq!(
            program.pending_replay_sessions,
            HashSet::from([session_id]),
            "resume candidates block deferred live events until their no-op step executes",
        );
        assert_eq!(
            program.steps,
            vec![WorkspaceStreamReplayStep::NoReplayRequired { session_id }]
        );
    }
}
