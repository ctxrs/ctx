use ctx_history_core::{EventRole, EventType};
use serde_json::{json, Value};

use crate::captured_batch::CapturedSqliteValue;
use crate::provider::normalization::{
    capped_text, provider_capped_json, provider_json_text, text_id_index,
};
use crate::{CaptureError, Result, PROVIDER_MAX_PREVIEW_CHARS, PROVIDER_MAX_TEXT_CHARS};

use super::{SHELLEY_CONVERSATION_VALUE_COUNT, SHELLEY_MESSAGE_VALUE_COUNT};

pub(crate) struct ShelleyConversationRow {
    pub(crate) rowid: i64,
    pub(crate) conversation_id: String,
    pub(crate) slug: Option<String>,
    pub(crate) user_initiated: bool,
    pub(crate) created_at: Option<String>,
    pub(crate) updated_at: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) archived: bool,
    pub(crate) parent_conversation_id: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) conversation_options: Option<String>,
    pub(crate) current_generation: Option<i64>,
    pub(crate) agent_working: bool,
    pub(crate) tags: Option<String>,
    pub(crate) is_draft: bool,
    pub(crate) draft: Option<String>,
    pub(crate) queued_messages: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ShelleyMessageRow {
    pub(crate) rowid: i64,
    pub(crate) message_id: String,
    pub(crate) conversation_id: String,
    pub(crate) sequence_id: i64,
    pub(crate) entry_type: String,
    pub(crate) llm_data: Option<String>,
    pub(crate) user_data: Option<String>,
    pub(crate) usage_data: Option<String>,
    pub(crate) created_at: Option<String>,
    pub(crate) display_data: Option<String>,
    pub(crate) excluded_from_context: bool,
    pub(crate) generation: Option<i64>,
    pub(crate) llm_api_url: Option<String>,
    pub(crate) model_name: Option<String>,
    pub(crate) forked_from_message_id: Option<String>,
}

#[cfg(test)]
pub(super) fn decode_shelley_message_record(
    values: &[CapturedSqliteValue],
) -> Result<(ShelleyMessageRow, ShelleyConversationRow)> {
    let conversation = decode_shelley_message_parent(values)?;
    let message = decode_shelley_message(values)?;
    if conversation.conversation_id != message.conversation_id {
        return Err(CaptureError::InvalidPayload(
            "Shelley parent-bearing message references a different conversation".to_owned(),
        ));
    }
    Ok((message, conversation))
}

pub(super) fn decode_shelley_message_parent(
    values: &[CapturedSqliteValue],
) -> Result<ShelleyConversationRow> {
    if values.len() != SHELLEY_MESSAGE_VALUE_COUNT + SHELLEY_CONVERSATION_VALUE_COUNT {
        return Err(CaptureError::InvalidPayload(
            "Shelley parent-bearing message logical row has an unexpected value count".to_owned(),
        ));
    }
    decode_shelley_conversation(&values[SHELLEY_MESSAGE_VALUE_COUNT..])
}

pub(super) fn decode_shelley_message_child_record(
    values: &[CapturedSqliteValue],
) -> Result<(ShelleyMessageRow, bool)> {
    if values.len() != SHELLEY_MESSAGE_VALUE_COUNT + 1 {
        return Err(CaptureError::InvalidPayload(
            "Shelley child message logical row has an unexpected value count".to_owned(),
        ));
    }
    let message = decode_shelley_message(values)?;
    let has_conversation = match values.get(SHELLEY_MESSAGE_VALUE_COUNT) {
        Some(CapturedSqliteValue::Integer(_)) => true,
        Some(CapturedSqliteValue::Null) => false,
        _ => {
            return Err(CaptureError::InvalidPayload(
                "Shelley child message conversation reference must be an integer or null"
                    .to_owned(),
            ));
        }
    };
    Ok((message, has_conversation))
}

pub(crate) fn decode_shelley_message(values: &[CapturedSqliteValue]) -> Result<ShelleyMessageRow> {
    if values.len() < SHELLEY_MESSAGE_VALUE_COUNT {
        return Err(CaptureError::InvalidPayload(
            "Shelley message logical row has too few values".to_owned(),
        ));
    }
    Ok(ShelleyMessageRow {
        rowid: shelley_required_integer(values, 0, "message rowid")?,
        message_id: shelley_required_text(values, 1, "message_id")?,
        conversation_id: shelley_required_text(values, 2, "message conversation_id")?,
        sequence_id: shelley_required_integer(values, 3, "message sequence_id")?,
        entry_type: shelley_required_text(values, 4, "message type")?,
        llm_data: shelley_optional_text(values, 5, "message llm_data")?,
        user_data: shelley_optional_text(values, 6, "message user_data")?,
        usage_data: shelley_optional_text(values, 7, "message usage_data")?,
        created_at: shelley_optional_text(values, 8, "message created_at")?,
        display_data: shelley_optional_text(values, 9, "message display_data")?,
        excluded_from_context: shelley_optional_integer(
            values,
            10,
            "message excluded_from_context",
        )?
        .is_some_and(|value| value != 0),
        generation: shelley_optional_integer(values, 11, "message generation")?,
        llm_api_url: shelley_optional_text(values, 12, "message llm_api_url")?,
        model_name: shelley_optional_text(values, 13, "message model_name")?,
        forked_from_message_id: shelley_optional_text(
            values,
            14,
            "message forked_from_message_id",
        )?,
    })
}

pub(crate) fn decode_shelley_conversation(
    values: &[CapturedSqliteValue],
) -> Result<ShelleyConversationRow> {
    if values.len() != SHELLEY_CONVERSATION_VALUE_COUNT {
        return Err(CaptureError::InvalidPayload(
            "Shelley conversation logical row has an unexpected value count".to_owned(),
        ));
    }
    Ok(ShelleyConversationRow {
        rowid: shelley_required_integer(values, 0, "conversation rowid")?,
        conversation_id: shelley_required_text(values, 1, "conversation_id")?,
        slug: shelley_optional_text(values, 2, "conversation slug")?,
        user_initiated: shelley_optional_integer(values, 3, "conversation user_initiated")?
            .is_some_and(|value| value != 0),
        created_at: shelley_optional_text(values, 4, "conversation created_at")?,
        updated_at: shelley_optional_text(values, 5, "conversation updated_at")?,
        cwd: shelley_optional_text(values, 6, "conversation cwd")?,
        archived: shelley_optional_integer(values, 7, "conversation archived")?.unwrap_or(0) != 0,
        parent_conversation_id: shelley_optional_text(
            values,
            8,
            "conversation parent_conversation_id",
        )?,
        model: shelley_optional_text(values, 9, "conversation model")?,
        conversation_options: shelley_optional_text(values, 10, "conversation options")?,
        current_generation: shelley_optional_integer(
            values,
            11,
            "conversation current_generation",
        )?,
        agent_working: shelley_optional_integer(values, 12, "conversation agent_working")?
            .unwrap_or(0)
            != 0,
        tags: shelley_optional_text(values, 13, "conversation tags")?,
        is_draft: shelley_optional_integer(values, 14, "conversation is_draft")?.unwrap_or(0) != 0,
        draft: shelley_optional_text(values, 15, "conversation draft")?,
        queued_messages: shelley_optional_text(values, 16, "conversation queued_messages")?,
    })
}

fn shelley_value<'a>(
    values: &'a [CapturedSqliteValue],
    index: usize,
    field: &str,
) -> Result<&'a CapturedSqliteValue> {
    values.get(index).ok_or_else(|| {
        CaptureError::InvalidPayload(format!("Shelley logical row is missing {field}"))
    })
}

fn shelley_required_text(
    values: &[CapturedSqliteValue],
    index: usize,
    field: &str,
) -> Result<String> {
    match shelley_value(values, index, field)? {
        CapturedSqliteValue::Text(value) => Ok(value.clone()),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Shelley logical row {field} must be text"
        ))),
    }
}

fn shelley_optional_text(
    values: &[CapturedSqliteValue],
    index: usize,
    field: &str,
) -> Result<Option<String>> {
    match shelley_value(values, index, field)? {
        CapturedSqliteValue::Null => Ok(None),
        CapturedSqliteValue::Text(value) => Ok(Some(value.clone())),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Shelley logical row {field} must be text or null"
        ))),
    }
}

fn shelley_required_integer(
    values: &[CapturedSqliteValue],
    index: usize,
    field: &str,
) -> Result<i64> {
    match shelley_value(values, index, field)? {
        CapturedSqliteValue::Integer(value) => Ok(*value),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Shelley logical row {field} must be an integer"
        ))),
    }
}

fn shelley_optional_integer(
    values: &[CapturedSqliteValue],
    index: usize,
    field: &str,
) -> Result<Option<i64>> {
    match shelley_value(values, index, field)? {
        CapturedSqliteValue::Null => Ok(None),
        CapturedSqliteValue::Integer(value) => Ok(Some(*value)),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Shelley logical row {field} must be an integer or null"
        ))),
    }
}

pub(super) fn shelley_message_body(message: &ShelleyMessageRow) -> Value {
    json!({
        "message_id": message.message_id,
        "conversation_id": message.conversation_id,
        "sequence_id": message.sequence_id,
        "type": message.entry_type,
        "llm_data": message.llm_data.as_deref().map(provider_json_text),
        "user_data": message.user_data.as_deref().map(provider_json_text),
        "display_data": message.display_data.as_deref().map(provider_json_text),
        "usage_data": message.usage_data.as_deref().map(provider_json_text),
    })
}

pub(super) fn shelley_message_text(message: &ShelleyMessageRow, body: &Value) -> Option<String> {
    let mut parts = Vec::new();
    for pointer in ["/user_data", "/llm_data", "/display_data"] {
        if let Some(text) = body.pointer(pointer).and_then(shelley_value_text) {
            parts.push(text);
        }
    }
    if parts.is_empty() && message.entry_type == "system" {
        Some("Shelley system message".to_owned())
    } else if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

/// Renders the exact source-backed text for message and result hydration.
///
/// The ordinary importer deliberately bounds its indexed preview; this path
/// preserves every selected source string and is never persisted in SQLite.
pub(crate) fn shelley_message_complete_text(message: &ShelleyMessageRow) -> Option<String> {
    let body = shelley_message_body(message);
    let mut parts = Vec::new();
    for pointer in ["/user_data", "/llm_data", "/display_data"] {
        if let Some(value) = body.pointer(pointer) {
            shelley_collect_complete_text(value, &mut parts);
        }
    }
    if parts.is_empty() && message.entry_type == "system" {
        Some("Shelley system message".to_owned())
    } else if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

fn shelley_collect_complete_text(value: &Value, parts: &mut Vec<String>) {
    match value {
        Value::String(text) => shelley_push_complete_text(parts, text),
        Value::Array(items) => {
            for item in items {
                shelley_collect_complete_text(item, parts);
            }
        }
        Value::Object(object) => {
            if let Some(kind) = shelley_content_type(value) {
                let handled = match kind.as_str() {
                    "text" => {
                        if let Some(text) = object.get("Text").and_then(Value::as_str) {
                            shelley_push_complete_text(parts, text);
                        }
                        true
                    }
                    "thinking" | "redacted_thinking" => {
                        if let Some(text) = object.get("Thinking").and_then(Value::as_str) {
                            shelley_push_complete_text(parts, text);
                        }
                        true
                    }
                    "tool_use" | "server_tool_use" => {
                        let name = object
                            .get("ToolName")
                            .and_then(Value::as_str)
                            .unwrap_or("tool");
                        shelley_push_complete_text(parts, &format!("tool call: {name}"));
                        if let Some(input) =
                            object.get("ToolInput").filter(|input| !input.is_null())
                        {
                            let input =
                                serde_json::to_string(input).unwrap_or_else(|_| input.to_string());
                            shelley_push_complete_text(parts, &format!("tool input: {input}"));
                        }
                        true
                    }
                    "tool_result" | "web_search_tool_result" => {
                        shelley_push_complete_text(parts, "tool result");
                        if let Some(results) = object.get("ToolResult") {
                            shelley_collect_complete_text(results, parts);
                        }
                        if let Some(display) = object.get("Display") {
                            shelley_collect_complete_text(display, parts);
                        }
                        true
                    }
                    "web_search_result" => {
                        for key in ["Title", "URL", "PageAge"] {
                            if let Some(text) = object.get(key).and_then(Value::as_str) {
                                shelley_push_complete_text(parts, text);
                            }
                        }
                        true
                    }
                    _ => false,
                };
                if handled {
                    return;
                }
            }
            for key in [
                "Text",
                "text",
                "Thinking",
                "thinking",
                "content",
                "Content",
                "output",
                "Output",
                "summary",
                "Summary",
                "message",
                "Message",
                "error",
                "Error",
                "LLMContent",
                "ToolResult",
                "Display",
            ] {
                if let Some(child) = object.get(key) {
                    shelley_collect_complete_text(child, parts);
                }
            }
        }
        Value::Number(_) | Value::Bool(_) | Value::Null => {}
    }
}

fn shelley_push_complete_text(parts: &mut Vec<String>, text: &str) {
    let text = text.trim();
    if !text.is_empty() {
        parts.push(text.to_owned());
    }
}

/// Canonical compound row used only for source-record verification. It binds
/// the logical shape and every message/parent field that can affect extraction.
pub(crate) fn shelley_verified_record_values(
    message: &ShelleyMessageRow,
    conversation: &ShelleyConversationRow,
    parent_bearing: bool,
) -> Vec<CapturedSqliteValue> {
    let optional_text = |value: &Option<String>| {
        value
            .clone()
            .map_or(CapturedSqliteValue::Null, CapturedSqliteValue::Text)
    };
    let optional_integer =
        |value: Option<i64>| value.map_or(CapturedSqliteValue::Null, CapturedSqliteValue::Integer);
    vec![
        CapturedSqliteValue::Integer(i64::from(parent_bearing)),
        CapturedSqliteValue::Integer(message.rowid),
        CapturedSqliteValue::Text(message.message_id.clone()),
        CapturedSqliteValue::Text(message.conversation_id.clone()),
        CapturedSqliteValue::Integer(message.sequence_id),
        CapturedSqliteValue::Text(message.entry_type.clone()),
        optional_text(&message.llm_data),
        optional_text(&message.user_data),
        optional_text(&message.usage_data),
        optional_text(&message.created_at),
        optional_text(&message.display_data),
        CapturedSqliteValue::Integer(i64::from(message.excluded_from_context)),
        optional_integer(message.generation),
        optional_text(&message.llm_api_url),
        optional_text(&message.model_name),
        optional_text(&message.forked_from_message_id),
        CapturedSqliteValue::Integer(conversation.rowid),
        CapturedSqliteValue::Text(conversation.conversation_id.clone()),
        optional_text(&conversation.slug),
        CapturedSqliteValue::Integer(i64::from(conversation.user_initiated)),
        optional_text(&conversation.created_at),
        optional_text(&conversation.updated_at),
        optional_text(&conversation.cwd),
        CapturedSqliteValue::Integer(i64::from(conversation.archived)),
        optional_text(&conversation.parent_conversation_id),
        optional_text(&conversation.model),
        optional_text(&conversation.conversation_options),
        optional_integer(conversation.current_generation),
        CapturedSqliteValue::Integer(i64::from(conversation.agent_working)),
        optional_text(&conversation.tags),
        CapturedSqliteValue::Integer(i64::from(conversation.is_draft)),
        optional_text(&conversation.draft),
        optional_text(&conversation.queued_messages),
    ]
}

pub(super) fn shelley_event_role(entry_type: &str) -> Option<EventRole> {
    Some(match entry_type {
        "user" => EventRole::User,
        "agent" | "assistant" => EventRole::Assistant,
        "tool" => EventRole::Tool,
        "system" | "error" | "gitinfo" | "warning" | "modelchange" => EventRole::System,
        _ => EventRole::Unknown,
    })
}

pub(super) fn shelley_event_type(message: &ShelleyMessageRow, body: &Value) -> EventType {
    match message.entry_type.as_str() {
        "tool" => EventType::ToolOutput,
        "gitinfo" => EventType::VcsChange,
        "system" | "error" | "warning" | "modelchange" => EventType::Notice,
        "agent" | "assistant" if shelley_value_has_tool_use(body) => EventType::ToolCall,
        "user" | "agent" | "assistant" if shelley_value_has_tool_result(body) => {
            EventType::ToolOutput
        }
        "user" | "agent" | "assistant" => EventType::Message,
        _ => EventType::Notice,
    }
}

pub(crate) fn shelley_event_index(message: &ShelleyMessageRow) -> u64 {
    let sequence = message.sequence_id.max(0) as u64;
    let bucket = text_id_index(
        &format!("{}:{}", message.conversation_id, message.message_id),
        4_096,
    );
    sequence.saturating_mul(4_096).saturating_add(bucket)
}

fn shelley_value_has_tool_use(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(shelley_value_has_tool_use),
        Value::Object(object) => {
            let content_type = shelley_content_type(value);
            matches!(
                content_type.as_deref(),
                Some("tool_use" | "server_tool_use")
            ) || object.values().any(shelley_value_has_tool_use)
        }
        _ => false,
    }
}

fn shelley_value_has_tool_result(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(shelley_value_has_tool_result),
        Value::Object(object) => {
            let content_type = shelley_content_type(value);
            matches!(
                content_type.as_deref(),
                Some("tool_result" | "web_search_tool_result" | "web_search_result")
            ) || object.values().any(shelley_value_has_tool_result)
        }
        _ => false,
    }
}

pub(crate) fn shelley_value_text(value: &Value) -> Option<String> {
    let mut parts = Vec::new();
    shelley_collect_text(value, &mut parts);
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn shelley_collect_text(value: &Value, parts: &mut Vec<String>) {
    match value {
        Value::String(text) => shelley_push_text(parts, text),
        Value::Array(items) => {
            for item in items {
                if shelley_text_budget_remaining(parts) == 0 {
                    break;
                }
                shelley_collect_text(item, parts);
            }
        }
        Value::Object(object) => {
            if let Some(kind) = shelley_content_type(value) {
                let handled = match kind.as_str() {
                    "text" => {
                        if let Some(text) = object.get("Text").and_then(Value::as_str) {
                            shelley_push_text(parts, text);
                        }
                        true
                    }
                    "thinking" | "redacted_thinking" => {
                        if let Some(text) = object.get("Thinking").and_then(Value::as_str) {
                            shelley_push_text(parts, text);
                        }
                        true
                    }
                    "tool_use" | "server_tool_use" => {
                        let name = object
                            .get("ToolName")
                            .and_then(Value::as_str)
                            .unwrap_or("tool");
                        shelley_push_text(parts, &format!("tool call: {name}"));
                        if let Some(input) = object.get("ToolInput") {
                            if !input.is_null() {
                                let input = provider_capped_json(input, PROVIDER_MAX_PREVIEW_CHARS);
                                shelley_push_text(parts, &format!("tool input: {input}"));
                            }
                        }
                        true
                    }
                    "tool_result" | "web_search_tool_result" => {
                        shelley_push_text(parts, "tool result");
                        if let Some(results) = object.get("ToolResult") {
                            shelley_collect_text(results, parts);
                        }
                        if let Some(display) = object.get("Display") {
                            shelley_collect_text(display, parts);
                        }
                        true
                    }
                    "web_search_result" => {
                        for key in ["Title", "URL", "PageAge"] {
                            if let Some(text) = object.get(key).and_then(Value::as_str) {
                                shelley_push_text(parts, text);
                            }
                        }
                        true
                    }
                    _ => false,
                };
                if handled {
                    return;
                }
            }

            for key in [
                "Text",
                "text",
                "Thinking",
                "thinking",
                "content",
                "Content",
                "output",
                "Output",
                "summary",
                "Summary",
                "message",
                "Message",
                "error",
                "Error",
                "LLMContent",
                "ToolResult",
                "Display",
            ] {
                if shelley_text_budget_remaining(parts) == 0 {
                    break;
                }
                if let Some(child) = object.get(key) {
                    shelley_collect_text(child, parts);
                }
            }
        }
        Value::Number(_) | Value::Bool(_) | Value::Null => {}
    }
}

fn shelley_push_text(parts: &mut Vec<String>, text: &str) {
    let text = text.trim();
    if !text.is_empty() {
        let remaining = shelley_text_budget_remaining(parts);
        if remaining == 0 {
            return;
        }
        let separator_budget = usize::from(!parts.is_empty());
        if remaining <= separator_budget {
            return;
        }
        let (text, _) = capped_text(text, remaining - separator_budget);
        parts.push(text);
    }
}

fn shelley_text_budget_remaining(parts: &[String]) -> usize {
    let used = parts.iter().map(|part| part.chars().count()).sum::<usize>()
        + parts.len().saturating_sub(1);
    (PROVIDER_MAX_TEXT_CHARS + 1).saturating_sub(used)
}

fn shelley_content_type(value: &Value) -> Option<String> {
    let raw = value.get("Type")?;
    if let Some(text) = raw.as_str() {
        let normalized = text.trim().to_ascii_lowercase();
        return match normalized.as_str() {
            "contenttypetext" => Some("text".to_owned()),
            "contenttypethinking" => Some("thinking".to_owned()),
            "contenttyperedactedthinking" => Some("redacted_thinking".to_owned()),
            "contenttypetooluse" => Some("tool_use".to_owned()),
            "contenttypetoolresult" => Some("tool_result".to_owned()),
            "contenttypeservertooluse" => Some("server_tool_use".to_owned()),
            "contenttypewebsearchtoolresult" => Some("web_search_tool_result".to_owned()),
            "contenttypewebsearchresult" => Some("web_search_result".to_owned()),
            _ => Some(normalized),
        };
    }
    raw.as_i64().and_then(|kind| {
        match kind {
            2 => Some("text"),
            3 => Some("thinking"),
            4 => Some("redacted_thinking"),
            5 => Some("tool_use"),
            6 => Some("tool_result"),
            7 => Some("server_tool_use"),
            8 => Some("web_search_tool_result"),
            9 => Some("web_search_result"),
            _ => None,
        }
        .map(str::to_owned)
    })
}

#[derive(Default)]
pub(super) struct ShelleyRelationshipState {
    active_conversation: Option<ShelleyConversationRow>,
}

impl ShelleyRelationshipState {
    pub(super) fn clear_active_conversation(&mut self) {
        self.active_conversation = None;
    }

    pub(super) fn replace_active_conversation(&mut self, conversation: ShelleyConversationRow) {
        self.active_conversation = Some(conversation);
    }

    pub(super) fn active_conversation(&self) -> Option<&ShelleyConversationRow> {
        self.active_conversation.as_ref()
    }
}
