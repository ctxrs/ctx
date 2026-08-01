use std::collections::{BTreeMap, BTreeSet};

use ctx_history_core::{EventRole, EventType};
use rusqlite::{params_from_iter, types::Value as SqlValue, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::native_source::NativeSqliteValue;
use crate::provider::normalization::{
    provider_explicit_result_value_text, provider_json_text, text_id_index,
};
use crate::{CaptureError, Result};

use super::{SHELLEY_CONVERSATION_VALUE_COUNT, SHELLEY_MESSAGE_VALUE_COUNT};

#[derive(Debug, Clone)]
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

pub(crate) fn decode_shelley_message(values: &[NativeSqliteValue]) -> Result<ShelleyMessageRow> {
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
    values: &[NativeSqliteValue],
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
    values: &'a [NativeSqliteValue],
    index: usize,
    field: &str,
) -> Result<&'a NativeSqliteValue> {
    values.get(index).ok_or_else(|| {
        CaptureError::InvalidPayload(format!("Shelley logical row is missing {field}"))
    })
}

fn shelley_required_text(
    values: &[NativeSqliteValue],
    index: usize,
    field: &str,
) -> Result<String> {
    match shelley_value(values, index, field)? {
        NativeSqliteValue::Text(value) => Ok(value.clone()),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Shelley logical row {field} must be text"
        ))),
    }
}

fn shelley_optional_text(
    values: &[NativeSqliteValue],
    index: usize,
    field: &str,
) -> Result<Option<String>> {
    match shelley_value(values, index, field)? {
        NativeSqliteValue::Null => Ok(None),
        NativeSqliteValue::Text(value) => Ok(Some(value.clone())),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Shelley logical row {field} must be text or null"
        ))),
    }
}

fn shelley_required_integer(
    values: &[NativeSqliteValue],
    index: usize,
    field: &str,
) -> Result<i64> {
    match shelley_value(values, index, field)? {
        NativeSqliteValue::Integer(value) => Ok(*value),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Shelley logical row {field} must be an integer"
        ))),
    }
}

fn shelley_optional_integer(
    values: &[NativeSqliteValue],
    index: usize,
    field: &str,
) -> Result<Option<i64>> {
    match shelley_value(values, index, field)? {
        NativeSqliteValue::Null => Ok(None),
        NativeSqliteValue::Integer(value) => Ok(Some(*value)),
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

/// Renders the exact source-backed text for direct Core projection.
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShelleyCompleteResult {
    pub(crate) text: String,
    pub(crate) call_ids: Vec<String>,
    pub(crate) tool_names: Vec<String>,
}

/// Selects the native payload of a Shelley result block without retaining a
/// second display representation of the same result.
pub(crate) fn shelley_message_complete_result(
    message: &ShelleyMessageRow,
) -> std::result::Result<Option<ShelleyCompleteResult>, String> {
    let body = shelley_message_body(message);
    let mut populated_sources = Vec::new();
    for pointer in ["/user_data", "/llm_data", "/display_data"] {
        let Some(value) = body.pointer(pointer).filter(|value| !value.is_null()) else {
            continue;
        };
        let mut blocks = Vec::new();
        shelley_collect_result_blocks(value, &mut blocks)?;
        if !blocks.is_empty() {
            populated_sources.push(blocks);
        }
    }
    if populated_sources.len() > 1 {
        return Err("Shelley result is represented in multiple native payload columns".to_owned());
    }

    let blocks = populated_sources.pop().unwrap_or_default();
    if !blocks.is_empty() {
        let mut text = Vec::with_capacity(blocks.len());
        let mut call_ids = Vec::new();
        let mut tool_names = Vec::new();
        for block in blocks {
            if !block.text.trim().is_empty() {
                text.push(block.text);
            }
            push_bounded_linkage(&mut call_ids, block.call_id);
            push_bounded_linkage(&mut tool_names, block.tool_name);
        }
        return Ok((!text.is_empty()).then(|| ShelleyCompleteResult {
            text: text.join("\n"),
            call_ids,
            tool_names,
        }));
    }

    if message.entry_type != "tool" {
        return Ok(None);
    }
    let direct = ["/user_data", "/llm_data", "/display_data"]
        .into_iter()
        .filter_map(|pointer| body.pointer(pointer))
        .filter(|value| !value.is_null())
        .collect::<Vec<_>>();
    let [value] = direct.as_slice() else {
        return if direct.is_empty() {
            Ok(None)
        } else {
            Err("Shelley tool result has multiple native payload candidates".to_owned())
        };
    };
    Ok(provider_explicit_result_value_text(value)
        .filter(|text| !text.trim().is_empty())
        .map(|text| ShelleyCompleteResult {
            text,
            call_ids: Vec::new(),
            tool_names: Vec::new(),
        }))
}

#[derive(Debug)]
struct ShelleyResultBlock {
    text: String,
    call_id: Option<String>,
    tool_name: Option<String>,
}

fn shelley_collect_result_blocks(
    value: &Value,
    blocks: &mut Vec<ShelleyResultBlock>,
) -> std::result::Result<(), String> {
    match value {
        Value::Array(values) => {
            for value in values {
                shelley_collect_result_blocks(value, blocks)?;
            }
        }
        Value::Object(object) => {
            if matches!(
                shelley_content_type(value).as_deref(),
                Some("tool_result" | "web_search_tool_result" | "web_search_result")
            ) {
                let primary = [
                    "ToolResult",
                    "Output",
                    "output",
                    "Result",
                    "result",
                    "Content",
                    "content",
                    "Text",
                    "text",
                ]
                .into_iter()
                .filter_map(|key| object.get(key).filter(|value| !value.is_null()))
                .collect::<Vec<_>>();
                let selected = match primary.as_slice() {
                    [selected] => *selected,
                    [] => match ["Display", "Results", "WebSearchResult"]
                        .into_iter()
                        .filter_map(|key| object.get(key).filter(|value| !value.is_null()))
                        .collect::<Vec<_>>()
                        .as_slice()
                    {
                        [selected] => *selected,
                        [] => {
                            return Err("Shelley typed result block has no supported payload field"
                                .to_owned())
                        }
                        _ => {
                            return Err(
                                "Shelley typed result block has multiple fallback payload fields"
                                    .to_owned(),
                            )
                        }
                    },
                    _ => {
                        return Err(
                            "Shelley typed result block has multiple payload fields".to_owned()
                        )
                    }
                };
                let text = provider_explicit_result_value_text(selected).ok_or_else(|| {
                    "Shelley typed result block has no meaningful payload".to_owned()
                })?;
                let call_id = unique_bounded_linkage(
                    object,
                    &[
                        "ToolUseID",
                        "ToolUseId",
                        "toolUseId",
                        "tool_use_id",
                        "call_id",
                    ],
                    "call ID",
                )?;
                let tool_name = unique_bounded_linkage(
                    object,
                    &["ToolName", "toolName", "tool_name", "name"],
                    "tool name",
                )?;
                blocks.push(ShelleyResultBlock {
                    text,
                    call_id,
                    tool_name,
                });
                return Ok(());
            }
            for child in object.values() {
                shelley_collect_result_blocks(child, blocks)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

fn unique_bounded_linkage(
    object: &serde_json::Map<String, Value>,
    keys: &[&str],
    label: &str,
) -> std::result::Result<Option<String>, String> {
    let values = keys
        .iter()
        .filter_map(|key| object.get(*key).and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>();
    if values.len() > 1 {
        return Err(format!("Shelley typed result block has ambiguous {label}"));
    }
    Ok(values
        .into_iter()
        .next()
        .filter(|value| value.len() <= 4 * 1024)
        .map(str::to_owned))
}

fn push_bounded_linkage(values: &mut Vec<String>, value: Option<String>) {
    if let Some(value) = value {
        if values.len() < 64 && !values.contains(&value) {
            values.push(value);
        }
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
                        if let Some(results) = object.get("ToolResult") {
                            shelley_collect_complete_text(results, parts);
                        } else if let Some(display) = object.get("Display") {
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
            let mut selected_child = false;
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
                    selected_child = true;
                    shelley_collect_complete_text(child, parts);
                }
            }
            if !selected_child {
                for child in object
                    .values()
                    .filter(|child| matches!(child, Value::Array(_) | Value::Object(_)))
                {
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
) -> Vec<NativeSqliteValue> {
    let optional_text = |value: &Option<String>| {
        value
            .clone()
            .map_or(NativeSqliteValue::Null, NativeSqliteValue::Text)
    };
    let optional_integer =
        |value: Option<i64>| value.map_or(NativeSqliteValue::Null, NativeSqliteValue::Integer);
    vec![
        NativeSqliteValue::Integer(i64::from(parent_bearing)),
        NativeSqliteValue::Integer(message.rowid),
        NativeSqliteValue::Text(message.message_id.clone()),
        NativeSqliteValue::Text(message.conversation_id.clone()),
        NativeSqliteValue::Integer(message.sequence_id),
        NativeSqliteValue::Text(message.entry_type.clone()),
        optional_text(&message.llm_data),
        optional_text(&message.user_data),
        optional_text(&message.usage_data),
        optional_text(&message.created_at),
        optional_text(&message.display_data),
        NativeSqliteValue::Integer(i64::from(message.excluded_from_context)),
        optional_integer(message.generation),
        optional_text(&message.llm_api_url),
        optional_text(&message.model_name),
        optional_text(&message.forked_from_message_id),
        NativeSqliteValue::Integer(conversation.rowid),
        NativeSqliteValue::Text(conversation.conversation_id.clone()),
        optional_text(&conversation.slug),
        NativeSqliteValue::Integer(i64::from(conversation.user_initiated)),
        optional_text(&conversation.created_at),
        optional_text(&conversation.updated_at),
        optional_text(&conversation.cwd),
        NativeSqliteValue::Integer(i64::from(conversation.archived)),
        optional_text(&conversation.parent_conversation_id),
        optional_text(&conversation.model),
        optional_text(&conversation.conversation_options),
        optional_integer(conversation.current_generation),
        NativeSqliteValue::Integer(i64::from(conversation.agent_working)),
        optional_text(&conversation.tags),
        NativeSqliteValue::Integer(i64::from(conversation.is_draft)),
        optional_text(&conversation.draft),
        optional_text(&conversation.queued_messages),
    ]
}

/// Hashes the complete logical row consumed by Shelley's exact-content route.
///
/// Source-backed projection and later hydration must use this one digest
/// contract so a changed message, parent conversation, or parent-bearing
/// relationship fails closed.
pub(crate) fn shelley_logical_record_digest(values: &[NativeSqliteValue]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx-complete-content-sqlite-logical-row-v1\0");
    digest.update((values.len() as u64).to_be_bytes());
    for value in values {
        match value {
            NativeSqliteValue::Null => digest.update([0]),
            NativeSqliteValue::Integer(value) => {
                digest.update([1]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::RealBits(value) => {
                digest.update([2]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::Text(value) => {
                digest.update([3]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value.as_bytes());
            }
            NativeSqliteValue::Blob(value) => {
                digest.update([4]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value);
            }
        }
    }
    digest.finalize().into()
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

/// Resolves the released compact event index, using a deterministic full-tuple
/// alternate only when two distinct native messages actually collide.
///
/// The lexicographically first complete native tuple retains the released
/// index on a fresh import. Every other collider gets an alternate that is
/// independent of rowid and scan order. Publication separately honors an
/// already-released event at the compact index.
pub(crate) fn shelley_stable_event_index(
    conn: &Connection,
    message: &ShelleyMessageRow,
    has_sequence_id: bool,
) -> Result<u64> {
    shelley_stable_event_indices(conn, std::slice::from_ref(message), has_sequence_id)?
        .remove(&message.rowid)
        .ok_or(CaptureError::SystemInvariant(
            "Shelley event-index set omitted its requested row",
        ))
}

/// Resolves collision-safe event indexes for one bounded message set.
///
/// Every collision domain is joined through one VALUES table, avoiding a
/// separate messages-table query for every projected row.
pub(crate) fn shelley_stable_event_indices(
    conn: &Connection,
    messages: &[ShelleyMessageRow],
    has_sequence_id: bool,
) -> Result<BTreeMap<i64, u64>> {
    let mut resolved = messages
        .iter()
        .map(|message| (message.rowid, shelley_event_index(message)))
        .collect::<BTreeMap<_, _>>();
    if !has_sequence_id {
        return Ok(resolved);
    }
    if messages.is_empty() {
        return Ok(resolved);
    }

    let saturation_threshold = u64::MAX / 4_096;
    let saturation_bound = i64::try_from(saturation_threshold).unwrap_or(i64::MAX);
    let values = std::iter::repeat_n("(?, ?, ?, ?)", messages.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "with targets(target_rowid, conversation_id, category, sequence_bound) as
             (values {values})
         select targets.target_rowid, messages.sequence_id, cast(messages.message_id as blob)
           from targets
           join messages
             on typeof(messages.conversation_id) = 'text'
            and messages.conversation_id = targets.conversation_id
            and typeof(messages.sequence_id) = 'integer'
            and typeof(messages.message_id) = 'text'
            and (
                 (targets.category = 0 and messages.sequence_id = targets.sequence_bound)
              or (targets.category = 1 and messages.sequence_id <= 0)
              or (targets.category = 2 and messages.sequence_id >= targets.sequence_bound)
            )
          order by targets.target_rowid, messages.sequence_id,
                   cast(messages.message_id as blob), messages.rowid"
    );
    let parameters = messages.iter().flat_map(|message| {
        let normalized_sequence = message.sequence_id.max(0) as u64;
        let (category, bound) = if message.sequence_id <= 0 {
            (1_i64, 0_i64)
        } else if normalized_sequence >= saturation_threshold {
            (2_i64, saturation_bound)
        } else {
            (0_i64, message.sequence_id)
        };
        [
            SqlValue::Integer(message.rowid),
            SqlValue::Text(message.conversation_id.clone()),
            SqlValue::Integer(category),
            SqlValue::Integer(bound),
        ]
    });
    let mut statement = conn.prepare(&sql)?;
    let candidates = statement.query_map(params_from_iter(parameters), |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, Vec<u8>>(2)?,
        ))
    })?;
    let by_rowid = messages
        .iter()
        .map(|message| (message.rowid, message))
        .collect::<BTreeMap<_, _>>();
    let mut colliders = BTreeMap::<i64, BTreeSet<Vec<u8>>>::new();
    for candidate in candidates {
        let (target_rowid, sequence_id, message_id) = candidate?;
        let Ok(message_id) = std::str::from_utf8(&message_id) else {
            // The scanner rejects this independently addressable row locally.
            // It cannot claim a valid native identity in another row's group.
            continue;
        };
        let message = by_rowid
            .get(&target_rowid)
            .ok_or(CaptureError::SystemInvariant(
                "Shelley collision query returned an unknown target row",
            ))?;
        let released = resolved[&target_rowid];
        if shelley_released_event_index(&message.conversation_id, sequence_id, message_id)
            == released
        {
            colliders
                .entry(target_rowid)
                .or_default()
                .insert(shelley_native_identity_bytes(
                    &message.conversation_id,
                    sequence_id,
                    message_id,
                ));
        }
    }
    for message in messages {
        let Some(colliders) = colliders.get(&message.rowid) else {
            continue;
        };
        let released = resolved[&message.rowid];
        let current = shelley_native_identity_bytes(
            &message.conversation_id,
            message.sequence_id,
            &message.message_id,
        );
        if colliders.len() > 1 && colliders.first() != Some(&current) {
            resolved.insert(
                message.rowid,
                shelley_collision_event_index(message, released),
            );
        }
    }
    Ok(resolved)
}

pub(crate) fn shelley_collision_event_index(message: &ShelleyMessageRow, released: u64) -> u64 {
    let mut digest = Sha256::new();
    digest.update(b"ctx-shelley-native-message-collision-v1\0");
    digest.update(shelley_native_identity_bytes(
        &message.conversation_id,
        message.sequence_id,
        &message.message_id,
    ));
    let output = digest.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&output[..8]);
    let mut alternate = u64::from_be_bytes(bytes) | (1_u64 << 63);
    if alternate == released {
        alternate ^= 1_u64 << 62;
    }
    alternate
}

fn shelley_released_event_index(conversation_id: &str, sequence_id: i64, message_id: &str) -> u64 {
    let sequence = sequence_id.max(0) as u64;
    let bucket = text_id_index(&format!("{conversation_id}:{message_id}"), 4_096);
    sequence.saturating_mul(4_096).saturating_add(bucket)
}

fn shelley_native_identity_bytes(
    conversation_id: &str,
    sequence_id: i64,
    message_id: &str,
) -> Vec<u8> {
    let mut identity = Vec::with_capacity(
        conversation_id
            .len()
            .saturating_add(message_id.len())
            .saturating_add(24),
    );
    identity.extend_from_slice(&(conversation_id.len() as u64).to_be_bytes());
    identity.extend_from_slice(conversation_id.as_bytes());
    identity.extend_from_slice(&sequence_id.to_be_bytes());
    identity.extend_from_slice(&(message_id.len() as u64).to_be_bytes());
    identity.extend_from_slice(message_id.as_bytes());
    identity
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
