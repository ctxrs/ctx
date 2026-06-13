use ctx_core::models::{
    Message, MessageAttachment, MessageDelivery, MessageRole, Session, SessionEvent,
    SessionEventType,
};

pub fn message_from_event(event: &SessionEvent, session: &Session) -> Option<Message> {
    let message_id = event
        .payload_json
        .get("message_id")
        .and_then(|value| value.as_str())
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .map(ctx_core::ids::MessageId)?;
    let content = event
        .payload_json
        .get("content")
        .and_then(|value| value.as_str())
        .map(|value| value.to_string())?;
    let delivery = event
        .payload_json
        .get("delivery")
        .and_then(|value| serde_json::from_value::<MessageDelivery>(value.clone()).ok())
        .unwrap_or(MessageDelivery::Immediate);
    let attachments = event
        .payload_json
        .get("attachments")
        .and_then(|value| serde_json::from_value::<Vec<MessageAttachment>>(value.clone()).ok())
        .unwrap_or_default();
    let order_seq = event
        .payload_json
        .get("order_seq")
        .or_else(|| event.payload_json.get("orderSeq"))
        .and_then(|value| value.as_i64());
    let role = match event.event_type {
        SessionEventType::UserMessage => MessageRole::User,
        SessionEventType::AssistantMessageInserted => MessageRole::Assistant,
        _ => return None,
    };
    let delivered_at = match role {
        MessageRole::Assistant => Some(event.created_at),
        _ => None,
    };
    Some(Message {
        id: message_id,
        session_id: event.session_id,
        task_id: session.task_id,
        run_id: event.run_id,
        turn_id: event.turn_id,
        turn_sequence: event
            .payload_json
            .get("turn_sequence")
            .and_then(|value| value.as_i64()),
        order_seq,
        role,
        content,
        attachments,
        delivery,
        delivered_at,
        created_at: event.created_at,
    })
}

pub fn derive_message_preview(content: &str) -> String {
    let trimmed = content.trim();
    let line = trimmed.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        return String::new();
    }
    const MAX_CHARS: usize = 160;
    let mut out: String = line.chars().take(MAX_CHARS).collect();
    if line.chars().count() > MAX_CHARS {
        out.push_str("...");
    }
    out
}
