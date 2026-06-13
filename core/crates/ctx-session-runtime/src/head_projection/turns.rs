use ctx_core::models::{
    Message, MessageDelivery, SessionEvent, SessionEventType, SessionTurn, SessionTurnStatus,
    SessionTurnToolSummary,
};
use ctx_core::session_projection::{
    terminal_status_from_finished_payload, turn_failure_from_finished_payload,
};

use super::events::event_context_window;

pub(super) fn terminal_status_from_finished_event(
    event: &SessionEvent,
) -> Option<SessionTurnStatus> {
    terminal_status_from_finished_payload(&event.payload_json)
}

pub fn patch_turn_from_event(turn: &mut SessionTurn, event: &SessionEvent) {
    match event.event_type {
        SessionEventType::TurnQueued => {
            turn.status = SessionTurnStatus::Queued;
        }
        SessionEventType::TurnStarted => {
            turn.status = SessionTurnStatus::Running;
        }
        SessionEventType::TurnInterrupted => {
            turn.status = SessionTurnStatus::Interrupted;
            turn.end_seq = Some(event.seq);
            turn.failure = None;
        }
        SessionEventType::TurnFinished => {
            let Some(status) = terminal_status_from_finished_event(event) else {
                return;
            };
            turn.status = status;
            turn.end_seq = Some(event.seq);
            turn.failure = turn_failure_from_finished_payload(&event.payload_json);
        }
        _ => {}
    }
    if let Some(metrics_json) = event_context_window(event) {
        turn.metrics_json = Some(metrics_json);
    }
    turn.updated_at = event.created_at;
}

pub fn recompute_turn_tool_counts(
    turn: &mut SessionTurn,
    tool_summaries: &[SessionTurnToolSummary],
) {
    let mut total = 0_i64;
    let mut pending = 0_i64;
    let mut running = 0_i64;
    let mut completed = 0_i64;
    let mut failed = 0_i64;
    for summary in tool_summaries
        .iter()
        .filter(|summary| summary.turn_id == turn.turn_id)
    {
        total += 1;
        match summary.status.as_deref() {
            Some("running") | Some("in_progress") => running += 1,
            Some("completed") | Some("complete") | Some("ok") | Some("succeeded") => {
                completed += 1;
            }
            Some("failed") | Some("error") => failed += 1,
            _ => pending += 1,
        }
    }
    turn.tool_total = total;
    turn.tool_pending = pending;
    turn.tool_running = running;
    turn.tool_completed = completed;
    turn.tool_failed = failed;
}

pub fn turn_from_event(event: &SessionEvent, message: Option<&Message>) -> Option<SessionTurn> {
    if !matches!(event.event_type, SessionEventType::UserMessage) {
        return None;
    }
    let turn_id = event.turn_id?;
    let delivery = message
        .map(|msg| msg.delivery.clone())
        .unwrap_or(MessageDelivery::Immediate);
    let status = if matches!(delivery, MessageDelivery::Queued) {
        SessionTurnStatus::Queued
    } else {
        SessionTurnStatus::Starting
    };
    Some(SessionTurn {
        turn_id,
        session_id: event.session_id,
        run_id: event.run_id,
        user_message_id: message.map(|msg| msg.id),
        status,
        start_seq: Some(event.seq),
        end_seq: None,
        started_at: event.created_at,
        updated_at: event.created_at,
        assistant_partial: None,
        thought_partial: None,
        metrics_json: None,
        failure: None,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
    })
}

pub fn should_refresh_turn_from_store(event_type: &SessionEventType) -> bool {
    matches!(
        event_type,
        SessionEventType::TurnQueued
            | SessionEventType::TurnStarted
            | SessionEventType::Done
            | SessionEventType::TurnFinished
            | SessionEventType::TurnInterrupted
    )
}
