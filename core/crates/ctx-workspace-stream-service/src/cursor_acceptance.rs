use std::collections::HashMap;

use ctx_core::ids::SessionId;
use ctx_core::models::{SessionHeadDelta, SessionHeadSnapshot, SessionSummaryDelta};
use ctx_workspace_active_snapshot::{is_transient_session_delta, SessionReplayCursor};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkspaceStreamCursorAcceptance {
    pub accepted: bool,
    pub next_cursor: SessionReplayCursor,
}

pub fn accept_session_delta_cursor(
    current: SessionReplayCursor,
    delta: &SessionHeadDelta,
) -> WorkspaceStreamCursorAcceptance {
    if is_transient_session_delta(delta) {
        return WorkspaceStreamCursorAcceptance {
            accepted: true,
            next_cursor: current,
        };
    }
    accept_session_cursor(current, SessionReplayCursor::from_delta(delta))
}

pub fn accept_session_head_cursor(
    current: SessionReplayCursor,
    head: &SessionHeadSnapshot,
) -> WorkspaceStreamCursorAcceptance {
    accept_session_cursor(current, SessionReplayCursor::from_head(head))
}

pub fn is_session_head_delta_after_cursor(
    delta: &SessionHeadDelta,
    cursor: SessionReplayCursor,
) -> bool {
    SessionReplayCursor::from_delta(delta) > cursor
}

pub fn is_session_summary_delta_after_cursor(
    delta: &SessionSummaryDelta,
    cursor: SessionReplayCursor,
) -> bool {
    let Some(last_event_seq) = delta.last_event_seq else {
        return true;
    };
    let event_cursor = SessionReplayCursor {
        last_event_seq: last_event_seq.max(0),
        projection_rev: delta.projection_rev.unwrap_or_default().max(0),
    };
    event_cursor > cursor
}

pub fn merge_replayed_and_live_subscription_cursors(
    live_subscriptions: &HashMap<SessionId, SessionReplayCursor>,
    replayed_subscriptions: HashMap<SessionId, SessionReplayCursor>,
) -> HashMap<SessionId, SessionReplayCursor> {
    live_subscriptions
        .iter()
        .map(|(session_id, live_cursor)| {
            let last_sent = replayed_subscriptions
                .get(session_id)
                .map(|replayed_cursor| replayed_cursor.cover(*live_cursor))
                .unwrap_or(*live_cursor);
            (*session_id, last_sent)
        })
        .collect()
}

fn accept_session_cursor(
    current: SessionReplayCursor,
    incoming: SessionReplayCursor,
) -> WorkspaceStreamCursorAcceptance {
    if incoming <= current {
        return WorkspaceStreamCursorAcceptance {
            accepted: false,
            next_cursor: current,
        };
    }
    WorkspaceStreamCursorAcceptance {
        accepted: true,
        next_cursor: incoming,
    }
}
