use std::{fmt, path::Path};

use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use serde_json::Value;

use crate::common::time::parse_rfc3339_utc;
use crate::provider::normalization::{
    provider_explicit_result_value_text, provider_role, provider_value_text,
};
use crate::MAX_PROVIDER_JSONL_LINE_BYTES;

#[derive(Debug, Clone)]
pub(crate) struct OpenHandsDecodedEvent {
    event_id: String,
    timestamp: DateTime<Utc>,
    entry_type: String,
    event_type: EventType,
    role: EventRole,
    text: String,
    value: Value,
}

impl OpenHandsDecodedEvent {
    pub(crate) fn event_id(&self) -> &str {
        &self.event_id
    }

    pub(crate) fn timestamp(&self) -> DateTime<Utc> {
        self.timestamp
    }

    pub(crate) fn entry_type(&self) -> &str {
        &self.entry_type
    }

    pub(crate) fn event_type(&self) -> EventType {
        self.event_type
    }

    pub(crate) fn role(&self) -> EventRole {
        self.role
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn value(&self) -> &Value {
        &self.value
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenHandsEventDecodeErrorKind {
    Invalid,
    TooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpenHandsEventDecodeError {
    kind: OpenHandsEventDecodeErrorKind,
    message: String,
}

impl OpenHandsEventDecodeError {
    fn invalid(message: String) -> Self {
        Self {
            kind: OpenHandsEventDecodeErrorKind::Invalid,
            message,
        }
    }

    fn too_large(observed_bytes: usize) -> Self {
        Self {
            kind: OpenHandsEventDecodeErrorKind::TooLarge,
            message: format!(
                "provider record exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit (observed {observed_bytes} bytes)"
            ),
        }
    }

    pub(crate) fn is_too_large(&self) -> bool {
        self.kind == OpenHandsEventDecodeErrorKind::TooLarge
    }
}

impl fmt::Display for OpenHandsEventDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for OpenHandsEventDecodeError {}

/// Decodes one bounded OpenHands event file into its authoritative semantics.
///
/// Import, compatibility matching, and complete-content recovery all consume
/// this result so event identity, type, role, and text cannot drift.
pub(crate) fn decode_openhands_event(
    path: &Path,
    bytes: &[u8],
) -> Result<OpenHandsDecodedEvent, OpenHandsEventDecodeError> {
    if bytes.len() > MAX_PROVIDER_JSONL_LINE_BYTES {
        return Err(OpenHandsEventDecodeError::too_large(bytes.len()));
    }
    let value = serde_json::from_slice::<Value>(bytes).map_err(|error| {
        OpenHandsEventDecodeError::invalid(format!("invalid OpenHands event JSON: {error}"))
    })?;
    decode_openhands_event_value(path, value)
}

/// Applies the authoritative OpenHands semantics to an already parsed event.
///
/// Complete-content recovery parses through its stricter shared JSON budget
/// before calling this entry point. Live import retains its existing byte-only
/// admission contract through [`decode_openhands_event`].
pub(crate) fn decode_openhands_event_value(
    path: &Path,
    value: Value,
) -> Result<OpenHandsDecodedEvent, OpenHandsEventDecodeError> {
    let event_id =
        super::openhands_bounded_derived_text(openhands_event_id(path, &value), "event id")
            .map_err(|error| OpenHandsEventDecodeError::invalid(error.to_string()))?;
    let timestamp = openhands_event_timestamp(&value).ok_or_else(|| {
        OpenHandsEventDecodeError::invalid(format!(
            "OpenHands event {event_id} missing valid timestamp"
        ))
    })?;
    let entry_type = openhands_entry_type(&value);
    let event_type = openhands_event_type(&value, &entry_type);
    let role = openhands_role(&value, &entry_type);
    let text = openhands_event_text(&value, &entry_type, event_type);
    Ok(OpenHandsDecodedEvent {
        event_id,
        timestamp,
        entry_type,
        event_type,
        role,
        text,
        value,
    })
}

fn openhands_event_id(path: &Path, value: &Value) -> String {
    value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| !stem.trim().is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| path.display().to_string())
}

fn openhands_event_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_utc)
}

fn openhands_entry_type(value: &Value) -> String {
    if let Some(entry_type) = value
        .get("kind")
        .or_else(|| value.get("type"))
        .and_then(Value::as_str)
    {
        return entry_type.to_owned();
    }
    if value.get("llm_message").is_some() {
        "MessageEvent".to_owned()
    } else if value.get("action").is_some() {
        "ActionEvent".to_owned()
    } else if value.get("observation").is_some() {
        "ObservationEvent".to_owned()
    } else {
        "OpenHandsEvent".to_owned()
    }
}

fn openhands_event_type(value: &Value, entry_type: &str) -> EventType {
    if value.get("llm_message").is_some() || entry_type == "MessageEvent" {
        return EventType::Message;
    }
    if value.get("action").is_some() || entry_type == "ActionEvent" {
        return match value.pointer("/action/kind").and_then(Value::as_str) {
            Some("FinishAction") => EventType::Message,
            Some("ThinkAction") => EventType::Summary,
            Some("FileEditorAction" | "StrReplaceEditorAction" | "PlanningFileEditorAction") => {
                EventType::ToolCall
            }
            _ => EventType::ToolCall,
        };
    }
    if value.get("observation").is_some() || entry_type == "ObservationEvent" {
        return match value.pointer("/observation/kind").and_then(Value::as_str) {
            Some(
                "FileEditorObservation"
                | "StrReplaceEditorObservation"
                | "PlanningFileEditorObservation",
            ) => EventType::FileTouched,
            Some("ExecuteBashObservation" | "TerminalObservation") => EventType::CommandOutput,
            _ => EventType::ToolOutput,
        };
    }
    match entry_type {
        "StreamingDeltaEvent" => EventType::Message,
        "CondensationSummaryEvent" | "CondensationEvent" => EventType::Summary,
        "AgentErrorEvent" | "ConversationErrorEvent" | "ServerErrorEvent" => EventType::ToolOutput,
        _ => EventType::Notice,
    }
}

fn openhands_role(value: &Value, entry_type: &str) -> EventRole {
    if let Some(role) = value.pointer("/llm_message/role").and_then(Value::as_str) {
        return provider_role(Some(role));
    }
    match value.get("source").and_then(Value::as_str) {
        Some("user") => EventRole::User,
        Some("agent") => EventRole::Assistant,
        Some("environment" | "hook") => EventRole::Tool,
        Some(source) => provider_role(Some(source)),
        None if entry_type == "ActionEvent" => EventRole::Assistant,
        None if entry_type == "ObservationEvent" => EventRole::Tool,
        _ => EventRole::Unknown,
    }
}

fn openhands_event_text(value: &Value, entry_type: &str, event_type: EventType) -> String {
    if let Some(text) = value
        .pointer("/llm_message/content")
        .and_then(provider_explicit_result_value_text)
    {
        return text;
    }
    if let Some(text) = value.get("content").and_then(provider_value_text) {
        return text;
    }
    if let Some(text) = value.pointer("/action/message").and_then(Value::as_str) {
        return text.to_owned();
    }
    if let Some(text) = value.pointer("/action/thought").and_then(Value::as_str) {
        return text.to_owned();
    }
    if let Some(command) = value.pointer("/action/command").and_then(Value::as_str) {
        return command.to_owned();
    }
    if let Some(path) = value.pointer("/action/path").and_then(Value::as_str) {
        let command = value
            .pointer("/action/command")
            .and_then(Value::as_str)
            .unwrap_or("file");
        return format!("{command} {path}");
    }
    if let Some(content) = value
        .pointer("/observation/content")
        .and_then(provider_value_text)
    {
        return content;
    }
    if let Some(output) = value.pointer("/observation/output").and_then(Value::as_str) {
        return output.to_owned();
    }
    if let Some(error) = value
        .pointer("/observation/error")
        .and_then(Value::as_str)
        .or_else(|| value.get("error").and_then(Value::as_str))
    {
        return error.to_owned();
    }
    if let Some(prompt) = value.pointer("/action/prompt").and_then(Value::as_str) {
        return prompt.to_owned();
    }
    if event_type == EventType::Notice {
        format!("OpenHands event: {entry_type}")
    } else {
        String::new()
    }
}

/// Returns the explicit result body selected by OpenHands' authoritative
/// decoded event. It never substitutes an event-kind label.
pub(crate) fn openhands_result_content(event: &OpenHandsDecodedEvent) -> Option<String> {
    if !matches!(
        event.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    ) {
        return None;
    }
    let value = &event.value;
    value
        .pointer("/observation/content")
        .and_then(provider_value_text)
        .or_else(|| {
            value
                .pointer("/observation/output")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .or_else(|| {
            value
                .pointer("/observation/error")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .or_else(|| {
            value
                .get("content")
                .and_then(provider_explicit_result_value_text)
        })
        .or_else(|| {
            value
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
}

#[cfg(test)]
mod tests {
    use ctx_history_core::{EventRole, EventType};
    use serde_json::json;

    use super::*;

    #[test]
    fn decoder_preserves_current_and_legacy_event_semantics_exactly() {
        let current_path = Path::new("/profile/v1_conversations/session/current.json");
        let current_bytes = serde_json::to_vec(&json!({
            "id": "current-id",
            "timestamp": "2026-07-22T12:00:00Z",
            "kind": "MessageEvent",
            "source": "agent",
            "llm_message": {
                "role": "assistant",
                "content": [{"type": "text", "text": "current exact text"}]
            }
        }))
        .unwrap();
        let current = decode_openhands_event(current_path, &current_bytes).unwrap();
        assert_eq!(current.event_id(), "current-id");
        assert_eq!(current.entry_type(), "MessageEvent");
        assert_eq!(current.event_type(), EventType::Message);
        assert_eq!(current.role(), EventRole::Assistant);
        assert_eq!(current.text(), "current exact text");
        assert_eq!(
            current.timestamp(),
            "2026-07-22T12:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );

        let legacy_path = Path::new("/profile/v1_conversations/session/0007-legacy.json");
        let legacy_bytes = serde_json::to_vec(&json!({
            "timestamp": "2026-07-22T12:00:01Z",
            "source": "agent",
            "action": {
                "kind": "ThinkAction",
                "thought": "legacy exact thought"
            }
        }))
        .unwrap();
        let legacy = decode_openhands_event(legacy_path, &legacy_bytes).unwrap();
        assert_eq!(legacy.event_id(), "0007-legacy");
        assert_eq!(legacy.entry_type(), "ActionEvent");
        assert_eq!(legacy.event_type(), EventType::Summary);
        assert_eq!(legacy.role(), EventRole::Assistant);
        assert_eq!(legacy.text(), "legacy exact thought");
    }

    #[test]
    fn decoder_fails_closed_for_malformed_and_oversized_events() {
        let path = Path::new("/profile/v1_conversations/session/malformed.json");
        let malformed = decode_openhands_event(path, b"{not-json").unwrap_err();
        assert!(!malformed.is_too_large());
        assert!(malformed
            .to_string()
            .starts_with("invalid OpenHands event JSON:"));

        let missing_timestamp = decode_openhands_event(
            path,
            br#"{"id":"missing-time","kind":"MessageEvent","content":"text"}"#,
        )
        .unwrap_err();
        assert_eq!(
            missing_timestamp.to_string(),
            "OpenHands event missing-time missing valid timestamp"
        );

        let oversized = vec![b'x'; MAX_PROVIDER_JSONL_LINE_BYTES + 1];
        let oversized = decode_openhands_event(path, &oversized).unwrap_err();
        assert!(oversized.is_too_large());
        assert_eq!(
            oversized.to_string(),
            format!(
                "provider record exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit (observed {} bytes)",
                MAX_PROVIDER_JSONL_LINE_BYTES + 1
            )
        );
    }

    #[test]
    fn result_profile_uses_explicit_observation_bytes_only() {
        let path = Path::new("/profile/v1_conversations/session/result.json");
        let decoded = decode_openhands_event_value(
            path,
            json!({
                "id": "result-id",
                "timestamp": "2026-07-22T12:00:00Z",
                "kind": "ObservationEvent",
                "source": "environment",
                "observation": {
                    "kind": "ExecuteBashObservation",
                    "content": "stdout\n"
                }
            }),
        )
        .unwrap();
        assert_eq!(
            openhands_result_content(&decoded).as_deref(),
            Some("stdout\n")
        );

        let no_body = decode_openhands_event_value(
            path,
            json!({
                "id": "empty-result",
                "timestamp": "2026-07-22T12:00:00Z",
                "kind": "ObservationEvent",
                "source": "environment",
                "observation": {"kind": "ExecuteBashObservation"}
            }),
        )
        .unwrap();
        assert_eq!(openhands_result_content(&no_body), None);
    }
}
