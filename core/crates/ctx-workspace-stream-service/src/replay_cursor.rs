use std::collections::HashMap;

use ctx_core::ids::SessionId;
use ctx_workspace_active_snapshot::{replay_cursor_after_live_progress, SessionReplayCursor};

use crate::read_model::WorkspaceStreamSnapshotReadModel;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceStreamResumeReplayCursorPlan {
    Replay { cursor: SessionReplayCursor },
    NoReplayRequired,
}

pub fn active_head_cursors_from_snapshot_read_model(
    read_model: &WorkspaceStreamSnapshotReadModel,
) -> HashMap<SessionId, SessionReplayCursor> {
    read_model
        .active_heads
        .heads
        .iter()
        .map(|head| (head.session.id, SessionReplayCursor::from_head(head)))
        .collect()
}

pub fn plan_resume_replay_cursor(
    live_cursor: Option<SessionReplayCursor>,
    after_seq: i64,
    after_projection_rev: i64,
) -> WorkspaceStreamResumeReplayCursorPlan {
    let requested_cursor = resume_replay_cursor(after_seq, after_projection_rev);
    match replay_cursor_after_live_progress(live_cursor, requested_cursor) {
        Some(cursor) => WorkspaceStreamResumeReplayCursorPlan::Replay { cursor },
        None => WorkspaceStreamResumeReplayCursorPlan::NoReplayRequired,
    }
}

fn resume_replay_cursor(after_seq: i64, after_projection_rev: i64) -> SessionReplayCursor {
    SessionReplayCursor {
        last_event_seq: after_seq,
        projection_rev: if after_projection_rev > 0 {
            after_projection_rev
        } else {
            i64::MAX
        },
    }
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

    #[test]
    fn resume_replay_cursor_planning_preserves_live_coverage_semantics() {
        assert_eq!(
            plan_resume_replay_cursor(Some(cursor(15, 16)), 10, 12),
            WorkspaceStreamResumeReplayCursorPlan::Replay {
                cursor: cursor(15, 16)
            }
        );
        assert_eq!(
            plan_resume_replay_cursor(Some(cursor(15, 16)), 20, 0),
            WorkspaceStreamResumeReplayCursorPlan::Replay {
                cursor: cursor(20, i64::MAX)
            }
        );
        assert_eq!(
            plan_resume_replay_cursor(None, 10, 12),
            WorkspaceStreamResumeReplayCursorPlan::NoReplayRequired
        );
    }
}
