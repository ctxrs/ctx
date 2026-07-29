mod complete_content;
mod event;
pub(crate) mod nativepath;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) fn forgecode_text_message_text(
    body: &serde_json::Value,
    event_type: ctx_history_core::EventType,
) -> String {
    event::forgecode_text_message_text(body, event_type)
}

pub(crate) use complete_content::{forgecode_complete_message, load_forgecode_conversation_values};
