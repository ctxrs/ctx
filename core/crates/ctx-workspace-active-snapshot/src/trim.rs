use serde::Serialize;
use std::cmp::Ordering;
use std::collections::HashSet;

use ctx_core::models::{
    Message, Session, SessionActivityState, SessionEvent, SessionEventType, SessionHeadSnapshot,
    SessionMetadata, SessionTurn, SessionTurnToolSummary,
};

pub(super) const ACTIVE_HEAD_TURN_LIMIT: usize = 5;
pub(super) const ACTIVE_HEAD_MESSAGE_LIMIT: usize = 200;
pub(super) const ACTIVE_HEAD_EVENT_LIMIT: usize = 200;
pub(super) const ACTIVE_HEAD_BYTE_LIMIT: usize = 256_000;
pub(super) const ACTIVE_HEAD_TOOL_SUMMARY_LIMIT: usize = 96;

#[derive(Serialize)]
struct SessionHeadWindowPayload<'a> {
    turns: &'a [SessionTurn],
    tool_summaries: &'a [SessionTurnToolSummary],
    events: &'a [SessionEvent],
    messages: &'a [Message],
}

pub fn session_metadata_from_session(session: &Session) -> SessionMetadata {
    SessionMetadata {
        id: session.id,
        task_id: session.task_id,
        workspace_id: session.workspace_id,
        worktree_id: session.worktree_id,
        execution_environment: session.execution_environment,
        parent_session_id: session.parent_session_id,
        relationship: session.relationship.clone(),
        provider_id: session.provider_id.clone(),
        model_id: session.model_id.clone(),
        reasoning_effort: session.reasoning_effort.clone(),
        title: session.title.clone(),
        agent_role: session.agent_role.clone(),
        status: session.status.clone(),
        provider_session_ref: session.provider_session_ref.clone(),
        created_at: session.created_at,
        updated_at: session.updated_at,
    }
}

pub(super) fn new_head_window() -> ctx_core::models::SessionHeadWindow {
    ctx_core::models::SessionHeadWindow {
        turn_limit: ACTIVE_HEAD_TURN_LIMIT as i64,
        message_limit: ACTIVE_HEAD_MESSAGE_LIMIT as i64,
        event_limit: ACTIVE_HEAD_EVENT_LIMIT as i64,
        byte_limit: ACTIVE_HEAD_BYTE_LIMIT as i64,
        turn_count: 0,
        message_count: 0,
        event_count: 0,
        bytes: 0,
        truncated: false,
    }
}

pub(super) fn new_head_snapshot(session: &Session) -> SessionHeadSnapshot {
    SessionHeadSnapshot {
        session: session_metadata_from_session(session),
        turns: Vec::new(),
        tool_summaries: Vec::new(),
        events: Vec::new(),
        messages: Vec::new(),
        last_event_seq: 0,
        projection_rev: 0,
        state_rev: 0,
        activity: SessionActivityState::default(),
        has_more_turns: false,
        history_cursor: None,
        has_more_history: false,
        summary_checkpoint: None,
        head_window: new_head_window(),
    }
}

fn is_partial_event(event: &SessionEvent) -> bool {
    matches!(
        event.event_type,
        SessionEventType::AssistantChunk
            | SessionEventType::AssistantComplete
            | SessionEventType::ContextWindowUpdate
            | SessionEventType::ThoughtChunk
    )
}

pub(super) fn should_include_event(event: &SessionEvent) -> bool {
    if event.seq < 0 {
        return false;
    }
    !is_partial_event(event)
}

pub(super) fn compare_turn_order(a: &SessionTurn, b: &SessionTurn) -> Ordering {
    match (a.start_seq, b.start_seq) {
        (Some(sa), Some(sb)) if sa != sb => sa.cmp(&sb),
        _ => a.started_at.cmp(&b.started_at),
    }
}

pub(super) fn upsert_turn(turns: &mut Vec<SessionTurn>, next: &SessionTurn) {
    if let Some(pos) = turns.iter().position(|turn| turn.turn_id == next.turn_id) {
        turns[pos] = next.clone();
    } else {
        turns.push(next.clone());
    }
    turns.sort_by(compare_turn_order);
}

pub(super) fn compare_message_order(a: &Message, b: &Message) -> Ordering {
    let created = a.created_at.cmp(&b.created_at);
    if created != Ordering::Equal {
        return created;
    }
    match (a.turn_sequence, b.turn_sequence) {
        (Some(sa), Some(sb)) if sa != sb => sa.cmp(&sb),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        _ => a.id.0.cmp(&b.id.0),
    }
}

pub(super) fn upsert_message(messages: &mut Vec<Message>, next: &Message) {
    if let Some(pos) = messages.iter().position(|msg| msg.id == next.id) {
        messages[pos] = next.clone();
    } else {
        messages.push(next.clone());
    }
    messages.sort_by(compare_message_order);
}

pub(super) fn upsert_event(events: &mut Vec<SessionEvent>, next: &SessionEvent) {
    if let Some(pos) = events.iter().position(|event| event.seq == next.seq) {
        events[pos] = next.clone();
    } else {
        events.push(next.clone());
    }
    events.sort_by_key(|event| event.seq);
}

fn head_window_bytes(
    turns: &[SessionTurn],
    tool_summaries: &[SessionTurnToolSummary],
    events: &[SessionEvent],
    messages: &[Message],
) -> usize {
    let payload = SessionHeadWindowPayload {
        turns,
        tool_summaries,
        events,
        messages,
    };
    serde_json::to_vec(&payload)
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

pub(super) fn retain_messages_for_turns(messages: &mut Vec<Message>, turns: &[SessionTurn]) {
    if turns.is_empty() {
        messages.clear();
        return;
    }
    let mut allowed = HashSet::new();
    for turn in turns {
        allowed.insert(turn.turn_id);
    }
    messages.retain(|msg| match msg.turn_id {
        Some(turn_id) => allowed.contains(&turn_id),
        None => true,
    });
}

pub(super) fn retain_tool_summaries_for_turns(
    tool_summaries: &mut Vec<SessionTurnToolSummary>,
    turns: &[SessionTurn],
) {
    if turns.is_empty() {
        tool_summaries.clear();
        return;
    }
    let mut allowed = HashSet::new();
    for turn in turns {
        allowed.insert(turn.turn_id);
    }
    tool_summaries.retain(|tool| allowed.contains(&tool.turn_id));
}

fn compare_tool_summary_order(a: &SessionTurnToolSummary, b: &SessionTurnToolSummary) -> Ordering {
    a.order_seq
        .cmp(&b.order_seq)
        .then_with(|| a.created_at.cmp(&b.created_at))
        .then_with(|| a.tool_call_id.cmp(&b.tool_call_id))
}

fn trim_tool_summaries_for_limit(tool_summaries: &mut Vec<SessionTurnToolSummary>) -> bool {
    if tool_summaries.len() <= ACTIVE_HEAD_TOOL_SUMMARY_LIMIT {
        return false;
    }
    tool_summaries.sort_by(compare_tool_summary_order);
    let drop = tool_summaries.len() - ACTIVE_HEAD_TOOL_SUMMARY_LIMIT;
    tool_summaries.drain(0..drop);
    true
}

pub(super) fn compact_active_head_snapshot(head: &SessionHeadSnapshot) -> SessionHeadSnapshot {
    let mut out = head.clone();

    let keep_turns = ACTIVE_HEAD_TURN_LIMIT.min(out.turns.len());
    if keep_turns == 0 {
        out.turns.clear();
        out.tool_summaries.clear();
        out.events.clear();
        out.messages.clear();
    } else {
        out.turns = out.turns.split_off(out.turns.len() - keep_turns);
        retain_messages_for_turns(&mut out.messages, &out.turns);
        retain_tool_summaries_for_turns(&mut out.tool_summaries, &out.turns);
        out.events.clear();

        if out.tool_summaries.len() > ACTIVE_HEAD_TOOL_SUMMARY_LIMIT {
            out.tool_summaries.sort_by(compare_tool_summary_order);
            out.tool_summaries = out
                .tool_summaries
                .split_off(out.tool_summaries.len() - ACTIVE_HEAD_TOOL_SUMMARY_LIMIT);
        }

        if out.messages.len() > ACTIVE_HEAD_MESSAGE_LIMIT {
            out.messages.sort_by(compare_message_order);
            out.messages = out
                .messages
                .split_off(out.messages.len() - ACTIVE_HEAD_MESSAGE_LIMIT);
        }
    }

    trim_head_window(&mut out);

    out.events.clear();
    out.head_window.event_limit = 0;
    out.head_window.event_count = 0;
    out.head_window.bytes =
        head_window_bytes(&out.turns, &out.tool_summaries, &out.events, &out.messages) as i64;

    let dropped = head.head_window.truncated
        || head.has_more_turns
        || head.turns.len() > out.turns.len()
        || head.messages.len() > out.messages.len()
        || head.tool_summaries.len() > out.tool_summaries.len()
        || head.events.len() > out.events.len();
    out.head_window.truncated = out.head_window.truncated || dropped;
    out.has_more_turns =
        out.has_more_turns || head.has_more_turns || head.turns.len() > out.turns.len();
    out
}

pub(super) fn strip_snapshot_partials(turns: &mut [SessionTurn], events: &mut Vec<SessionEvent>) {
    for turn in turns.iter_mut() {
        turn.assistant_partial = None;
        turn.thought_partial = None;
    }
    events.retain(should_include_event);
}

pub(super) fn trim_head_window(head: &mut SessionHeadSnapshot) {
    let turn_limit = if head.head_window.turn_limit > 0 {
        head.head_window.turn_limit as usize
    } else {
        ACTIVE_HEAD_TURN_LIMIT
    };
    let message_limit = if head.head_window.message_limit > 0 {
        head.head_window.message_limit as usize
    } else {
        ACTIVE_HEAD_MESSAGE_LIMIT
    };
    let event_limit = if head.head_window.event_limit > 0 {
        head.head_window.event_limit as usize
    } else {
        ACTIVE_HEAD_EVENT_LIMIT
    };
    let byte_limit = if head.head_window.byte_limit > 0 {
        head.head_window.byte_limit as usize
    } else {
        ACTIVE_HEAD_BYTE_LIMIT
    };

    let mut truncated = false;
    while head.turns.len() > turn_limit {
        head.turns.remove(0);
        truncated = true;
        head.has_more_turns = true;
    }
    retain_messages_for_turns(&mut head.messages, &head.turns);
    retain_tool_summaries_for_turns(&mut head.tool_summaries, &head.turns);
    if trim_tool_summaries_for_limit(&mut head.tool_summaries) {
        truncated = true;
    }

    while head.messages.len() > message_limit && !head.turns.is_empty() {
        head.turns.remove(0);
        truncated = true;
        head.has_more_turns = true;
        retain_messages_for_turns(&mut head.messages, &head.turns);
        retain_tool_summaries_for_turns(&mut head.tool_summaries, &head.turns);
        if trim_tool_summaries_for_limit(&mut head.tool_summaries) {
            truncated = true;
        }
    }

    if head.events.len() > event_limit {
        let drop = head.events.len() - event_limit;
        head.events.drain(0..drop);
        truncated = true;
    }

    loop {
        let bytes = head_window_bytes(
            &head.turns,
            &head.tool_summaries,
            &head.events,
            &head.messages,
        );
        if bytes <= byte_limit || (head.turns.is_empty() && head.events.is_empty()) {
            break;
        }
        if !head.turns.is_empty() {
            head.turns.remove(0);
            truncated = true;
            head.has_more_turns = true;
            retain_messages_for_turns(&mut head.messages, &head.turns);
            retain_tool_summaries_for_turns(&mut head.tool_summaries, &head.turns);
            if trim_tool_summaries_for_limit(&mut head.tool_summaries) {
                truncated = true;
            }
            continue;
        }
        if !head.events.is_empty() {
            head.events.remove(0);
            truncated = true;
            continue;
        }
        break;
    }

    let bytes = head_window_bytes(
        &head.turns,
        &head.tool_summaries,
        &head.events,
        &head.messages,
    );
    head.head_window = ctx_core::models::SessionHeadWindow {
        turn_limit: turn_limit as i64,
        message_limit: message_limit as i64,
        event_limit: event_limit as i64,
        byte_limit: byte_limit as i64,
        turn_count: head.turns.len() as i64,
        message_count: head.messages.len() as i64,
        event_count: head.events.len() as i64,
        bytes: bytes as i64,
        truncated,
    };
}
