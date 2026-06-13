use super::fixtures::{test_state, workspace_stream_handle};
use super::*;
use ctx_core::ids::{SessionId, WorkspaceId};
use ctx_core::models::WorkspaceActiveSnapshotSessionIntent;
use ctx_workspace_active_snapshot::SessionReplayCursor;
use std::collections::{HashMap, HashSet};

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
}

#[async_trait::async_trait]
impl WorkspaceStreamReplayDrainHook for RecordingReplayStepHook {
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
}

#[tokio::test]
async fn replay_program_computes_head_only_cursor_in_daemon() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let handle = workspace_stream_handle(&state);
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();

    let program = plan_workspace_stream_replay_program(
        &handle,
        workspace_id,
        &[resolved(
            session_id,
            WorkspaceActiveSnapshotSessionIntent::Head,
            WorkspaceStreamSessionReplay::Reset,
        )],
        &HashMap::from([(session_id, cursor(4, 5))]),
        &HashMap::from([(session_id, cursor(10, 3))]),
        true,
    )
    .await;

    assert!(program.pending_replay_sessions.is_empty());
    assert_eq!(
        program.steps,
        vec![WorkspaceStreamReplayStep::HeadOnly {
            session_id,
            cursor: cursor(10, 5),
        }]
    );
}

#[tokio::test]
async fn replay_program_step_hook_runs_before_each_planned_step() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let handle = workspace_stream_handle(&state);
    let workspace_id = WorkspaceId::new();
    let head_session_id = SessionId::new();
    let replay_session_id = SessionId::new();
    let mut hook = RecordingReplayStepHook::default();

    let program = plan_workspace_stream_replay_program_with_step_hook(
        &handle,
        workspace_id,
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
        "transport can drain live events before each async planning step while preserving initial blockers",
    );
    assert_eq!(
        program.pending_replay_sessions,
        HashSet::from([replay_session_id])
    );
    assert_eq!(program.steps.len(), 2);
}

#[tokio::test]
async fn replay_program_uses_live_cursor_after_step_hook_runs() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let handle = workspace_stream_handle(&state);
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();
    let mut hook = RecordingReplayStepHook {
        live_cursors: HashMap::from([(session_id, cursor(15, 16))]),
        ..Default::default()
    };

    let program = plan_workspace_stream_replay_program_with_step_hook(
        &handle,
        workspace_id,
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
async fn replay_program_plans_resume_replay_with_covered_live_cursor() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let handle = workspace_stream_handle(&state);
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();

    let program = plan_workspace_stream_replay_program(
        &handle,
        workspace_id,
        &[resolved(
            session_id,
            WorkspaceActiveSnapshotSessionIntent::Replay,
            WorkspaceStreamSessionReplay::Resume {
                after_seq: 10,
                after_projection_rev: 12,
            },
        )],
        &HashMap::from([(session_id, cursor(15, 16))]),
        &HashMap::new(),
        false,
    )
    .await;

    assert_eq!(program.pending_replay_sessions, HashSet::from([session_id]));
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
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let handle = workspace_stream_handle(&state);
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();

    let program = plan_workspace_stream_replay_program(
        &handle,
        workspace_id,
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
    )
    .await;

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

#[tokio::test]
async fn replay_program_skips_reset_replay_entries() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let handle = workspace_stream_handle(&state);
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();

    let program = plan_workspace_stream_replay_program(
        &handle,
        workspace_id,
        &[resolved(
            session_id,
            WorkspaceActiveSnapshotSessionIntent::Replay,
            WorkspaceStreamSessionReplay::Reset,
        )],
        &HashMap::new(),
        &HashMap::new(),
        false,
    )
    .await;

    assert!(program.pending_replay_sessions.is_empty());
    assert!(program.steps.is_empty());
}
