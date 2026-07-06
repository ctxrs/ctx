use ctx_history_core::EventType;

pub(crate) fn text_has_real_content(text: Option<&str>) -> bool {
    text.is_some_and(|text| !text.trim().is_empty())
}

pub(crate) fn event_type_is_real_conversation(event_type: EventType) -> bool {
    matches!(event_type, EventType::Message)
}

pub(crate) fn event_has_real_conversation_content(
    event_type: EventType,
    text: Option<&str>,
) -> bool {
    event_type_is_real_conversation(event_type) && text_has_real_content(text)
}
