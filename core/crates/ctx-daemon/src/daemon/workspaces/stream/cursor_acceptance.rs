use std::collections::HashMap;

use ctx_core::ids::SessionId;
use ctx_core::models::{SessionHeadDelta, SessionHeadSnapshot, SessionSummaryDelta};
use ctx_workspace_active_snapshot::SessionReplayCursor;
pub use ctx_workspace_stream_service::cursor_acceptance::{
    accept_session_delta_cursor, accept_session_head_cursor, is_session_head_delta_after_cursor,
    is_session_summary_delta_after_cursor, merge_replayed_and_live_subscription_cursors,
    WorkspaceStreamCursorAcceptance,
};

use crate::daemon::WorkspaceStreamHandle;

impl WorkspaceStreamHandle {
    pub fn accept_session_delta_cursor(
        &self,
        current: SessionReplayCursor,
        delta: &SessionHeadDelta,
    ) -> WorkspaceStreamCursorAcceptance {
        accept_session_delta_cursor(current, delta)
    }

    pub fn accept_session_head_cursor(
        &self,
        current: SessionReplayCursor,
        head: &SessionHeadSnapshot,
    ) -> WorkspaceStreamCursorAcceptance {
        accept_session_head_cursor(current, head)
    }

    pub fn is_session_head_delta_after_cursor(
        &self,
        delta: &SessionHeadDelta,
        cursor: SessionReplayCursor,
    ) -> bool {
        is_session_head_delta_after_cursor(delta, cursor)
    }

    pub fn is_session_summary_delta_after_cursor(
        &self,
        delta: &SessionSummaryDelta,
        cursor: SessionReplayCursor,
    ) -> bool {
        is_session_summary_delta_after_cursor(delta, cursor)
    }

    pub fn merge_replayed_and_live_subscription_cursors(
        &self,
        live_subscriptions: &HashMap<SessionId, SessionReplayCursor>,
        replayed_subscriptions: HashMap<SessionId, SessionReplayCursor>,
    ) -> HashMap<SessionId, SessionReplayCursor> {
        merge_replayed_and_live_subscription_cursors(live_subscriptions, replayed_subscriptions)
    }
}
