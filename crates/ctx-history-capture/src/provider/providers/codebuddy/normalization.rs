use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use serde_json::{json, Value};

use crate::provider::normalization::provider_role;

pub(super) fn codebuddy_title_from_text(text: &str) -> Option<String> {
    let title = text.replace('\n', " ").chars().take(50).collect::<String>();
    (!title.trim().is_empty()).then_some(title)
}

#[derive(Debug, Clone)]
pub(super) struct CodeBuddyEventInput {
    pub(super) native_message_id: String,
    pub(super) event_type: EventType,
    pub(super) role: Option<String>,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) text: String,
}

pub(crate) fn codebuddy_decoded_message(raw_message: &Value) -> Value {
    match raw_message.get("message") {
        Some(Value::String(text)) => {
            serde_json::from_str(text).unwrap_or_else(|_| json!({ "content": text }))
        }
        Some(value) => value.clone(),
        None => raw_message.clone(),
    }
}

pub(crate) fn codebuddy_message_text(decoded: &Value, raw_message: &Value) -> String {
    let text = decoded
        .get("content")
        .and_then(codebuddy_content_text)
        .or_else(|| {
            decoded
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .or_else(|| decoded.as_str().map(str::to_owned))
        .or_else(|| raw_message.get("content").and_then(codebuddy_content_text))
        .or_else(|| {
            raw_message
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default();
    codebuddy_clean_content(&text)
}

fn codebuddy_content_text(content: &Value) -> Option<String> {
    if let Some(text) = content.as_str() {
        return Some(text.to_owned());
    }
    let blocks = content.as_array()?;
    let parts = blocks
        .iter()
        .filter_map(|block| {
            let block_type = block.get("type").and_then(Value::as_str);
            if block_type.is_some_and(|kind| kind != "text") {
                return None;
            }
            block
                .get("text")
                .or_else(|| block.get("content"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join("\n"))
}

pub(super) fn codebuddy_clean_content(content: &str) -> String {
    let mut cleaned = content.to_owned();
    for tag in [
        "user_info",
        "project_context",
        "project_layout",
        "system_reminder",
        "additional_data",
        "currently_opened_file",
    ] {
        cleaned = remove_xml_like_block(&cleaned, tag);
    }
    cleaned = cleaned.replace("<user_query>", "");
    cleaned = cleaned.replace("</user_query>", "");
    cleaned.trim().to_owned()
}

fn remove_xml_like_block(input: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut output = input.to_owned();
    while let Some(start) = output.find(&open) {
        let Some(relative_end) = output[start + open.len()..].find(&close) else {
            output.replace_range(start..start + open.len(), "");
            continue;
        };
        let end = start + open.len() + relative_end + close.len();
        output.replace_range(start..end, "");
    }
    output
}

pub(super) struct CodeBuddySessionInput<'a> {
    pub(super) provider_session_id: &'a str,
    pub(super) cwd: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub(super) struct CodeBuddySessionDraft {
    pub(super) provider_session_id: String,
    pub(super) cwd: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct CodeBuddyEventDraft {
    pub(super) native_message_id: String,
    pub(super) legacy_provider_event_hash: String,
    pub(super) event_type: EventType,
    pub(super) role: EventRole,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) text: String,
}

pub(super) fn codebuddy_normalized_rows(
    session: &CodeBuddySessionInput<'_>,
    event: CodeBuddyEventInput,
) -> (CodeBuddySessionDraft, CodeBuddyEventDraft) {
    let event = codebuddy_event(session.provider_session_id, event);
    (codebuddy_session_draft(session), event)
}

pub(super) fn codebuddy_session_draft(draft: &CodeBuddySessionInput<'_>) -> CodeBuddySessionDraft {
    CodeBuddySessionDraft {
        provider_session_id: draft.provider_session_id.to_owned(),
        cwd: draft.cwd.map(str::to_owned),
    }
}

fn codebuddy_event(provider_session_id: &str, event: CodeBuddyEventInput) -> CodeBuddyEventDraft {
    let event_id = format!("{provider_session_id}:{}", event.native_message_id);
    let role = provider_role(event.role.as_deref());
    CodeBuddyEventDraft {
        native_message_id: event.native_message_id,
        legacy_provider_event_hash: event_id,
        event_type: event.event_type,
        role,
        occurred_at: event.occurred_at,
        text: event.text,
    }
}
