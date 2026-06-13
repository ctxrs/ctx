use super::trim::{
    should_include_event, strip_snapshot_partials, trim_head_window, upsert_event, upsert_message,
    upsert_turn,
};
use ctx_core::models::{
    SessionHeadDelta, SessionHeadSnapshot, SessionSnapshotSummary, SessionSummaryDelta, SessionTurn,
};

pub(super) fn apply_session_summary_delta(
    summary: &mut SessionSnapshotSummary,
    delta: &SessionSummaryDelta,
) -> bool {
    let mut changed = false;
    let current_last_event_seq = summary.last_event_seq.unwrap_or(i64::MIN);
    let incoming_projection_rev = delta.projection_rev.unwrap_or(summary.projection_rev);
    if let Some(activity) = delta.activity.as_ref() {
        let incoming_last_event_seq = delta.last_event_seq.unwrap_or(current_last_event_seq);
        if incoming_last_event_seq >= current_last_event_seq && summary.activity != *activity {
            summary.activity = activity.clone();
            changed = true;
        }
    }
    if let Some(last_message_at) = delta.last_message_at {
        let should_update = match summary.last_message_at {
            Some(current) => last_message_at > current,
            None => true,
        };
        if should_update {
            summary.last_message_at = Some(last_message_at);
            changed = true;
        }
    }
    if let Some(preview) = delta.last_message_preview.clone() {
        let next_preview = if preview.is_empty() {
            None
        } else {
            Some(preview)
        };
        if summary.last_message_preview != next_preview {
            summary.last_message_preview = next_preview;
            changed = true;
        }
    }
    if let Some(last_event_seq) = delta.last_event_seq {
        let next = summary
            .last_event_seq
            .map(|current| current.max(last_event_seq))
            .unwrap_or(last_event_seq);
        if summary.last_event_seq != Some(next) {
            summary.last_event_seq = Some(next);
            changed = true;
        }
    }
    if incoming_projection_rev > summary.projection_rev {
        summary.projection_rev = incoming_projection_rev;
        changed = true;
    }
    if let Some(state_rev) = delta.state_rev {
        if state_rev > summary.state_rev {
            summary.state_rev = state_rev;
            changed = true;
        }
    }
    changed
}

pub(super) fn apply_head_delta(head: &mut SessionHeadSnapshot, delta: &SessionHeadDelta) {
    let next_seq = delta
        .event
        .as_ref()
        .map(|event| event.seq)
        .unwrap_or(delta.last_event_seq);
    let stale_delta = next_seq < head.last_event_seq
        || (next_seq == head.last_event_seq && delta.projection_rev < head.projection_rev);
    if next_seq > head.last_event_seq {
        head.last_event_seq = next_seq;
    }
    if delta.projection_rev > head.projection_rev {
        head.projection_rev = delta.projection_rev;
    }
    if !stale_delta {
        head.state_rev = head.state_rev.max(delta.state_rev);
    }
    if !stale_delta {
        if let Some(session) = delta.session.as_ref() {
            head.session = session.clone();
        }
        if let Some(activity) = delta.activity.as_ref() {
            head.activity = activity.clone();
        }
    }

    let mut changed = false;
    if let Some(turn) = delta.turn.as_ref() {
        let turn = if stale_delta {
            preserve_stale_turn_lifecycle(&head.turns, turn)
        } else {
            turn.clone()
        };
        upsert_turn(&mut head.turns, &turn);
        changed = true;
    }
    if let Some(message) = delta.message.as_ref() {
        upsert_message(&mut head.messages, message);
        changed = true;
    }
    if let Some(event) = delta.event.as_ref() {
        if should_include_event(event) {
            upsert_event(&mut head.events, event);
            changed = true;
        }
    }
    if !delta.tool_summaries.is_empty() {
        for summary in &delta.tool_summaries {
            if let Some(pos) = head
                .tool_summaries
                .iter()
                .position(|item| item.tool_call_id == summary.tool_call_id)
            {
                head.tool_summaries[pos] = summary.clone();
            } else {
                head.tool_summaries.push(summary.clone());
            }
        }
        changed = true;
    }
    if changed {
        strip_snapshot_partials(&mut head.turns, &mut head.events);
        trim_head_window(head);
    }
}

fn preserve_stale_turn_lifecycle(turns: &[SessionTurn], next: &SessionTurn) -> SessionTurn {
    let Some(existing) = turns.iter().find(|turn| turn.turn_id == next.turn_id) else {
        return next.clone();
    };
    SessionTurn {
        status: existing.status.clone(),
        end_seq: existing.end_seq,
        tool_total: existing.tool_total,
        tool_pending: existing.tool_pending,
        tool_running: existing.tool_running,
        tool_completed: existing.tool_completed,
        tool_failed: existing.tool_failed,
        ..next.clone()
    }
}
