use super::*;

pub(super) const SESSION_HEAD_MAX_TURNS: u32 = 200;
pub(super) const SESSION_HEAD_MESSAGE_LIMIT: usize = 200;
pub(super) const SESSION_HEAD_TOOL_SUMMARY_LIMIT: usize = 96;
pub(super) const SESSION_HEAD_EVENT_LIMIT: usize = 200;
pub(super) const SESSION_HEAD_BYTE_LIMIT: usize = 256_000;
pub(super) const ACTIVE_SNAPSHOT_HEAD_LIMIT: u32 = 5;
pub(super) const ACTIVE_SNAPSHOT_TOOL_SUMMARY_LIMIT: usize = 96;
pub(super) const SESSION_HEAD_ARCHIVED_TURN_LIMIT: u32 = 50;

fn retain_messages_for_turns(messages: &mut Vec<Message>, turns: &[SessionTurn]) {
    if turns.is_empty() {
        messages.clear();
        return;
    }
    let mut allowed = std::collections::HashSet::new();
    for turn in turns {
        allowed.insert(turn.turn_id);
    }
    messages.retain(|msg| match msg.turn_id {
        Some(turn_id) => allowed.contains(&turn_id),
        None => true,
    });
}

fn retain_tool_summaries_for_turns(
    tool_summaries: &mut Vec<SessionTurnToolSummary>,
    turns: &[SessionTurn],
) {
    if turns.is_empty() {
        tool_summaries.clear();
        return;
    }
    let mut allowed = std::collections::HashSet::new();
    for turn in turns {
        allowed.insert(turn.turn_id);
    }
    tool_summaries.retain(|tool| allowed.contains(&tool.turn_id));
}

pub(super) fn trim_tool_summaries_for_limit(
    tool_summaries: &mut Vec<SessionTurnToolSummary>,
    limit: usize,
) -> bool {
    if tool_summaries.len() <= limit {
        return false;
    }
    tool_summaries.sort_by(compare_tool_summary_order);
    let drop = tool_summaries.len() - limit;
    tool_summaries.drain(0..drop);
    true
}

pub(super) fn strip_snapshot_partials(turns: &mut [SessionTurn], events: &mut Vec<SessionEvent>) {
    for turn in turns.iter_mut() {
        turn.assistant_partial = None;
        turn.thought_partial = None;
    }
    if events.is_empty() {
        return;
    }
    events.retain(|event| {
        !matches!(
            event.event_type,
            SessionEventType::AssistantChunk
                | SessionEventType::AssistantComplete
                | SessionEventType::ThoughtChunk
        )
    });
}

#[allow(clippy::too_many_arguments)]
pub(super) fn trim_session_head_window(
    turns: &mut Vec<SessionTurn>,
    messages: &mut Vec<Message>,
    tool_summaries: &mut Vec<SessionTurnToolSummary>,
    events: &mut Vec<SessionEvent>,
    has_more_turns: &mut bool,
    turn_limit: usize,
    message_limit: usize,
    event_limit: usize,
    tool_summary_limit: usize,
    byte_limit: usize,
) -> SessionHeadWindow {
    let mut truncated = false;

    while turns.len() > turn_limit {
        turns.remove(0);
        truncated = true;
        *has_more_turns = true;
    }
    retain_messages_for_turns(messages, turns);
    retain_tool_summaries_for_turns(tool_summaries, turns);
    if trim_tool_summaries_for_limit(tool_summaries, tool_summary_limit) {
        truncated = true;
    }

    // Session heads are turn-atomic under the current history contract. Once only
    // one turn remains, preserve it even if its message count exceeds the soft cap.
    while messages.len() > message_limit && turns.len() > 1 {
        turns.remove(0);
        truncated = true;
        *has_more_turns = true;
        retain_messages_for_turns(messages, turns);
        retain_tool_summaries_for_turns(tool_summaries, turns);
        if trim_tool_summaries_for_limit(tool_summaries, tool_summary_limit) {
            truncated = true;
        }
    }

    if events.len() > event_limit {
        let drop = events.len() - event_limit;
        events.drain(0..drop);
        truncated = true;
    }

    loop {
        let bytes = head_window_bytes(turns, tool_summaries, events, messages);
        if bytes <= byte_limit || (turns.is_empty() && events.is_empty()) {
            break;
        }
        if turns.len() > 1 {
            turns.remove(0);
            truncated = true;
            *has_more_turns = true;
            retain_messages_for_turns(messages, turns);
            retain_tool_summaries_for_turns(tool_summaries, turns);
            if trim_tool_summaries_for_limit(tool_summaries, tool_summary_limit) {
                truncated = true;
            }
            continue;
        }
        if !events.is_empty() {
            events.remove(0);
            truncated = true;
            continue;
        }
        // Preserve the newest remaining turn even if it exceeds soft byte limits.
        break;
    }

    let bytes = head_window_bytes(turns, tool_summaries, events, messages);
    SessionHeadWindow {
        turn_limit: turn_limit as i64,
        message_limit: message_limit as i64,
        event_limit: event_limit as i64,
        byte_limit: byte_limit as i64,
        turn_count: turns.len() as i64,
        message_count: messages.len() as i64,
        event_count: events.len() as i64,
        bytes: bytes as i64,
        truncated,
    }
}

pub(super) fn session_head_kind_to_str(kind: SessionHeadKind) -> &'static str {
    match kind {
        SessionHeadKind::Active => "active",
        SessionHeadKind::Archived => "archived",
    }
}

pub(super) fn session_head_limits(kind: SessionHeadKind, turn_limit: u32) -> SessionHeadLimits {
    let max_turns = match kind {
        SessionHeadKind::Active => SESSION_HEAD_MAX_TURNS,
        SessionHeadKind::Archived => SESSION_HEAD_ARCHIVED_TURN_LIMIT,
    };
    let turn_limit = turn_limit.clamp(1, max_turns) as usize;
    SessionHeadLimits {
        turn_limit,
        message_limit: SESSION_HEAD_MESSAGE_LIMIT,
        tool_summary_limit: SESSION_HEAD_TOOL_SUMMARY_LIMIT,
        event_limit: SESSION_HEAD_EVENT_LIMIT,
        byte_limit: SESSION_HEAD_BYTE_LIMIT,
    }
}

pub(super) fn apply_session_head_limits(
    mut head: SessionHead,
    limits: SessionHeadLimits,
    include_events: bool,
) -> SessionHead {
    let was_truncated = head.head_window.truncated;
    if !include_events {
        head.events.clear();
    }
    strip_snapshot_partials(&mut head.turns, &mut head.events);
    let mut has_more_turns = head.has_more_turns;
    let mut head_window = trim_session_head_window(
        &mut head.turns,
        &mut head.messages,
        &mut head.tool_summaries,
        &mut head.events,
        &mut has_more_turns,
        limits.turn_limit,
        limits.message_limit,
        limits.event_limit,
        limits.tool_summary_limit,
        limits.byte_limit,
    );
    head_window.truncated = head_window.truncated || was_truncated;
    head.has_more_turns = has_more_turns;
    head.head_window = head_window;
    head
}
