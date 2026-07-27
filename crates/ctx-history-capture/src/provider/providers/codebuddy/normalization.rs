use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventRole, EventType};
use serde_json::{json, Value};

use crate::provider::normalization::{
    provider_capped_json, provider_policy_body, provider_policy_event_text,
    provider_result_identifier_evidence, provider_result_outcome_evidence, provider_role,
};
use crate::{CODEBUDDY_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS};

pub(super) fn codebuddy_title_from_text(text: &str) -> Option<String> {
    let title = text.replace('\n', " ").chars().take(50).collect::<String>();
    (!title.trim().is_empty()).then_some(title)
}

#[derive(Debug, Clone)]
pub(super) struct CodeBuddyEventInput {
    pub(super) provider_event_index: u64,
    pub(super) legacy_provider_event_index: u64,
    pub(super) native_message_id: String,
    pub(super) event_hash: String,
    pub(super) event_type: EventType,
    pub(super) role: Option<String>,
    pub(super) ref_type: Option<String>,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) text: String,
    pub(super) raw_message: Value,
    pub(super) decoded_message: Value,
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

#[derive(Clone, Copy)]
pub(super) enum CodeBuddyNativeShape {
    Extension,
    Cli,
}

impl CodeBuddyNativeShape {
    fn as_str(self) -> &'static str {
        match self {
            Self::Extension => "extension_json",
            Self::Cli => "cli_jsonl",
        }
    }

    fn event_source(self) -> &'static str {
        match self {
            Self::Extension => "codebuddy_messages_json",
            Self::Cli => "codebuddy_cli_jsonl",
        }
    }

    fn schema_proof(self) -> Option<&'static str> {
        match self {
            Self::Extension => Some("WayLog shayne-snap/WayLog@6939033b7a39326fbdc249e28e6aa12461db1f09 src/services/readers/codebuddy-reader.ts"),
            Self::Cli => None,
        }
    }

    fn limitations(self) -> &'static [&'static str] {
        match self {
            Self::Extension => &[
                "The original project path is represented by CodeBuddy's MD5 project directory when not available in the current IDE workspace",
                "Message file mtimes are used when native message timestamps are absent",
                "Non-text content blocks and binary attachments are preserved only in capped native JSON metadata",
            ],
            Self::Cli => &[
                "Non-message CLI JSONL rows are not imported; only their contribution to the source row count is recorded",
                "Non-text content blocks and binary attachments are preserved only in capped native JSON metadata",
            ],
        }
    }
}

pub(super) struct CodeBuddySessionInput<'a> {
    pub(super) provider_session_id: &'a str,
    pub(super) native_session_id: &'a str,
    pub(super) project_hash: &'a str,
    pub(super) started_at: DateTime<Utc>,
    pub(super) ended_at: Option<DateTime<Utc>>,
    pub(super) title: Option<&'a str>,
    pub(super) cwd: Option<&'a str>,
    pub(super) project_index: Option<&'a Value>,
    pub(super) conversation: Option<&'a Value>,
    pub(super) session_index: &'a Value,
    pub(super) file_names: &'a [&'a str],
    pub(super) shape: CodeBuddyNativeShape,
}

#[derive(Debug, Clone)]
pub(super) struct CodeBuddySessionDraft {
    pub(super) provider_session_id: String,
    pub(super) started_at: DateTime<Utc>,
    pub(super) ended_at: Option<DateTime<Utc>>,
    pub(super) cwd: Option<String>,
    pub(super) source_metadata: Value,
    pub(super) session_metadata: Value,
}

#[derive(Debug, Clone)]
pub(super) struct CodeBuddyEventDraft {
    pub(super) provider_event_index: u64,
    pub(super) legacy_provider_event_index: u64,
    pub(super) event_hash: String,
    pub(super) legacy_provider_event_hash: String,
    pub(super) event_type: EventType,
    pub(super) role: EventRole,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) payload: Value,
    pub(super) metadata: Value,
}

pub(super) fn codebuddy_normalized_rows(
    session: &CodeBuddySessionInput<'_>,
    event: CodeBuddyEventInput,
) -> (CodeBuddySessionDraft, CodeBuddyEventDraft) {
    let event = codebuddy_event(
        session.provider_session_id,
        session.project_hash,
        session.shape,
        &event,
    );
    (codebuddy_session_draft(session), event)
}

pub(super) fn codebuddy_session_draft(draft: &CodeBuddySessionInput<'_>) -> CodeBuddySessionDraft {
    CodeBuddySessionDraft {
        provider_session_id: draft.provider_session_id.to_owned(),
        started_at: draft.started_at,
        ended_at: draft.ended_at,
        cwd: draft.cwd.map(str::to_owned),
        source_metadata: json!({
            "adapter": CODEBUDDY_SOURCE_FORMAT,
            "native_shape": draft.shape.as_str(),
            "native_project_hash": draft.project_hash,
            "native_session_id": draft.native_session_id,
            "files": draft.file_names,
            "schema_proof": draft.shape.schema_proof(),
        }),
        session_metadata: json!({
            "source_format": CODEBUDDY_SOURCE_FORMAT,
            "provider": CaptureProvider::CodeBuddy.as_str(),
            "display_name": "CodeBuddy",
            "title": draft.title,
            "native_shape": draft.shape.as_str(),
            "native_project_hash": draft.project_hash,
            "native_session_id": draft.native_session_id,
            "project_index": draft.project_index.map(|value| provider_capped_json(value, PROVIDER_MAX_PREVIEW_CHARS)),
            "conversation": draft.conversation.map(|value| provider_capped_json(value, PROVIDER_MAX_PREVIEW_CHARS)),
            "session_index": provider_capped_json(draft.session_index, PROVIDER_MAX_PREVIEW_CHARS),
            "files": draft.file_names,
            "limitations": draft.shape.limitations(),
        }),
    }
}

fn codebuddy_event(
    provider_session_id: &str,
    project_hash: &str,
    shape: CodeBuddyNativeShape,
    event: &CodeBuddyEventInput,
) -> CodeBuddyEventDraft {
    let event_type = event.event_type;
    let retained_text = provider_policy_event_text(event_type, &event.text, &event.raw_message);
    let result_evidence =
        provider_result_identifier_evidence(event_type, &event.text, &event.raw_message);
    let result_outcome = provider_result_outcome_evidence(event_type, &event.raw_message);
    let event_id = format!("{provider_session_id}:{}", event.native_message_id);
    let role = provider_role(event.role.as_deref());
    CodeBuddyEventDraft {
        provider_event_index: event.provider_event_index,
        legacy_provider_event_index: event.legacy_provider_event_index,
        event_hash: event.event_hash.clone(),
        legacy_provider_event_hash: event_id.clone(),
        event_type,
        role,
        occurred_at: event.occurred_at,
        payload: json!({
            "entry_type": event.ref_type.as_deref().unwrap_or("message"),
            "event_id": event_id,
            "native_project_hash": project_hash,
            "native_message_id": event.native_message_id,
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": result_evidence,
            "result_outcome": result_outcome,
            "body": provider_capped_json(&provider_policy_body(event_type, &event.raw_message), PROVIDER_MAX_PREVIEW_CHARS),
            "decoded_body": provider_capped_json(&provider_policy_body(event_type, &event.decoded_message), PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": shape.event_source(),
            "source_format": CODEBUDDY_SOURCE_FORMAT,
            "native_message_id": event.native_message_id,
            "role": event.role,
            "ref_type": event.ref_type,
            "model": event.decoded_message.get("model").cloned().or_else(|| event.decoded_message.pointer("/providerData/model").cloned()),
        }),
    }
}
