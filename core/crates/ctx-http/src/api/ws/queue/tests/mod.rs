use super::*;
use chrono::Utc;
use serde_json::json;
use std::time::Duration;

use ctx_core::ids::*;
use ctx_core::models::*;
use ctx_workspace_active_snapshot::SessionReplayCursor;

fn partial_delta(session_id: SessionId) -> SessionHeadDelta {
    SessionHeadDelta {
        session_id,
        last_event_seq: 5,
        projection_rev: 7,
        state_rev: 0,
        emitted_at_ms: None,
        session: None,
        activity: None,
        event: Some(SessionEvent {
            seq: -1,
            id: SessionEventId::new(),
            session_id,
            run_id: None,
            turn_id: Some(TurnId::new()),
            event_type: SessionEventType::AssistantChunk,
            payload_json: json!({ "content_fragment": "partial" }),
            transient: true,
            created_at: Utc::now(),
        }),
        turn: None,
        message: None,
        tool_summaries: Vec::new(),
    }
}

fn cursor_delta(session_id: SessionId, last_event_seq: i64) -> SessionHeadDelta {
    cursor_delta_with_projection(session_id, last_event_seq, 7)
}

fn cursor_delta_with_projection(
    session_id: SessionId,
    last_event_seq: i64,
    projection_rev: i64,
) -> SessionHeadDelta {
    let mut delta = partial_delta(session_id);
    delta.last_event_seq = last_event_seq;
    delta.projection_rev = projection_rev;
    if let Some(event) = delta.event.as_mut() {
        event.seq = last_event_seq;
        event.event_type = SessionEventType::Notice;
        event.transient = false;
        event.payload_json = json!({ "seq": last_event_seq, "projectionRev": projection_rev });
    }
    delta
}

fn session_summary_delta_event(
    workspace_id: WorkspaceId,
    session_id: SessionId,
    last_event_seq: i64,
) -> WorkspaceActiveSnapshotEvent {
    session_summary_delta_event_with_projection(workspace_id, session_id, last_event_seq, 7)
}

fn session_summary_delta_event_with_projection(
    workspace_id: WorkspaceId,
    session_id: SessionId,
    last_event_seq: i64,
    projection_rev: i64,
) -> WorkspaceActiveSnapshotEvent {
    WorkspaceActiveSnapshotEvent::SessionSummaryDelta {
        workspace_id,
        snapshot_rev: last_event_seq,
        delta: Box::new(SessionSummaryDelta {
            session_id,
            task_id: TaskId::new(),
            activity: None,
            last_message_at: None,
            last_message_preview: None,
            last_event_seq: Some(last_event_seq),
            projection_rev: Some(projection_rev),
            state_rev: None,
            emitted_at_ms: None,
        }),
    }
}

fn head_delta_after_cursor(delta: &SessionHeadDelta, cursor: SessionReplayCursor) -> bool {
    SessionReplayCursor::from_delta(delta) > cursor
}

fn summary_delta_after_cursor(delta: &SessionSummaryDelta, cursor: SessionReplayCursor) -> bool {
    let Some(last_event_seq) = delta.last_event_seq else {
        return true;
    };
    SessionReplayCursor {
        last_event_seq: last_event_seq.max(0),
        projection_rev: delta.projection_rev.unwrap_or_default().max(0),
    } > cursor
}

mod buffers;
mod control_events;
mod priority_ordering;
