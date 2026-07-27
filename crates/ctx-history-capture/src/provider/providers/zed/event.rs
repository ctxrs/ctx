use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventRole, EventType, ProviderEventEnvelope};
use serde_json::{json, Value};

use crate::provider::normalization::{native_event, provider_value_text, NativeEventDraft};
use crate::{compute_payload_hash, CaptureError, Result, ZED_THREADS_SQLITE_SOURCE_FORMAT};

use super::thread::{zed_decode_thread_json, zed_required_timestamp, ZedThreadRow};

const ZED_EVENTS_PER_MESSAGE: u64 = 2;
const ZED_SPLIT_EVENT_IDENTITY_INDEX_OFFSET: u64 = 1_000_000;
const ZED_MAX_MESSAGES_PER_THREAD: usize = 65_536;
const ZED_MAX_MESSAGE_JSON_DEPTH: usize = 64;
const ZED_MAX_THREAD_MESSAGE_JSON_NODES: usize = 1_000_000;
const _: () = assert!(
    ZED_MAX_MESSAGES_PER_THREAD as u64 <= ZED_SPLIT_EVENT_IDENTITY_INDEX_OFFSET,
    "Zed message bound must keep split-event identity ranges disjoint"
);

pub(crate) struct ZedDecodedThread {
    thread: Value,
    row_updated_at: DateTime<Utc>,
    event_occurred_at: DateTime<Utc>,
}

impl ZedDecodedThread {
    pub(crate) fn thread(&self) -> &Value {
        &self.thread
    }

    pub(crate) fn messages(&self) -> &[Value] {
        self.thread
            .get("messages")
            .and_then(Value::as_array)
            .expect("validated Zed thread must retain its messages array")
    }

    pub(crate) fn row_updated_at(&self) -> DateTime<Utc> {
        self.row_updated_at
    }

    pub(crate) fn event_occurred_at(&self) -> DateTime<Utc> {
        self.event_occurred_at
    }

    pub(crate) fn events<'a>(&'a self, provider_session_id: &'a str) -> ZedDecodedEvents<'a> {
        ZedDecodedEvents {
            provider_session_id,
            messages: self.messages(),
            occurred_at: self.event_occurred_at,
            message_index: 0,
            next_split_index: 0,
        }
    }

    pub(crate) fn event_at<'a>(
        &'a self,
        provider_session_id: &'a str,
        event_index: usize,
    ) -> Result<Option<ZedDecodedEvent<'a>>> {
        self.events(provider_session_id)
            .nth(event_index)
            .transpose()
    }
}

pub(crate) struct ZedDecodedEvent<'a> {
    pub(crate) event: ProviderEventEnvelope,
    pub(crate) complete_text: String,
    pub(crate) message: &'a Value,
    pub(crate) first_for_message: bool,
}

pub(crate) struct ZedDecodedEvents<'a> {
    provider_session_id: &'a str,
    messages: &'a [Value],
    occurred_at: DateTime<Utc>,
    message_index: usize,
    next_split_index: u64,
}

impl<'a> Iterator for ZedDecodedEvents<'a> {
    type Item = Result<ZedDecodedEvent<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        let message = self.messages.get(self.message_index)?;
        let message_index = self.message_index;
        let kind = zed_message_kind(message).expect("validated Zed message must have a kind");
        let split = kind == "Agent" && zed_has_tool_use(message) && zed_has_tool_result(message);
        let (event_type, event_suffix, split_index) = if split && self.next_split_index == 0 {
            self.next_split_index = 1;
            (EventType::ToolCall, "tool_call", 0)
        } else if split {
            self.next_split_index = 0;
            self.message_index += 1;
            (EventType::ToolOutput, "tool_output", 1)
        } else {
            self.next_split_index = 0;
            self.message_index += 1;
            (zed_message_event_type(kind, message), "message", 0)
        };
        Some(
            zed_message_event(
                self.provider_session_id,
                message,
                message_index,
                self.occurred_at,
                event_type,
                event_suffix,
                split_index,
            )
            .map(|(event, complete_text)| ZedDecodedEvent {
                event,
                complete_text,
                message,
                first_for_message: split_index == 0,
            }),
        )
    }
}

pub(crate) fn decode_zed_thread_events(row: &ZedThreadRow) -> Result<ZedDecodedThread> {
    let row_updated_at = zed_required_timestamp(&row.updated_at, "Zed thread updated_at")?;
    let thread = zed_decode_thread_json(row)?;
    let messages = thread
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            CaptureError::InvalidPayload(format!(
                "Zed thread {} is missing DbThread.messages array",
                row.id
            ))
        })?;
    if messages.len() > ZED_MAX_MESSAGES_PER_THREAD {
        return Err(CaptureError::InvalidPayload(format!(
            "Zed thread {} exceeds {ZED_MAX_MESSAGES_PER_THREAD} messages",
            row.id
        )));
    }
    let mut remaining_nodes = ZED_MAX_THREAD_MESSAGE_JSON_NODES;
    for (message_index, message) in messages.iter().enumerate() {
        validate_zed_message(message, message_index, &mut remaining_nodes)?;
    }
    let event_occurred_at = thread
        .get("updated_at")
        .and_then(Value::as_str)
        .and_then(crate::common::time::parse_rfc3339_utc)
        .unwrap_or(row_updated_at);
    Ok(ZedDecodedThread {
        thread,
        row_updated_at,
        event_occurred_at,
    })
}

fn validate_zed_message(
    message: &Value,
    message_index: usize,
    remaining_nodes: &mut usize,
) -> Result<()> {
    let kind = zed_message_kind(message)
        .filter(|kind| !kind.trim().is_empty())
        .ok_or_else(|| {
            CaptureError::InvalidPayload(format!(
                "Zed message {message_index} is not a nonempty externally tagged value"
            ))
        })?;
    if matches!(kind, "User" | "Agent") {
        let inner = zed_message_inner(message, kind)
            .and_then(Value::as_object)
            .ok_or_else(|| {
                CaptureError::InvalidPayload(format!(
                    "Zed {kind} message {message_index} must contain an object"
                ))
            })?;
        if inner
            .get("content")
            .is_some_and(|content| !content.is_array())
        {
            return Err(CaptureError::InvalidPayload(format!(
                "Zed {kind} message {message_index} content must be an array"
            )));
        }
        if kind == "Agent"
            && inner
                .get("tool_results")
                .is_some_and(|results| !results.is_object())
        {
            return Err(CaptureError::InvalidPayload(format!(
                "Zed Agent message {message_index} tool_results must be an object"
            )));
        }
    }

    let mut stack = vec![(message, 1_usize)];
    while let Some((value, depth)) = stack.pop() {
        if depth > ZED_MAX_MESSAGE_JSON_DEPTH {
            return Err(CaptureError::InvalidPayload(format!(
                "Zed message {message_index} exceeds JSON depth {ZED_MAX_MESSAGE_JSON_DEPTH}"
            )));
        }
        *remaining_nodes = remaining_nodes.checked_sub(1).ok_or_else(|| {
            CaptureError::InvalidPayload(format!(
                "Zed thread messages exceed {ZED_MAX_THREAD_MESSAGE_JSON_NODES} JSON nodes"
            ))
        })?;
        match value {
            Value::Array(items) => {
                stack.extend(items.iter().map(|item| (item, depth + 1)));
            }
            Value::Object(object) => {
                stack.extend(object.values().map(|item| (item, depth + 1)));
            }
            _ => {}
        }
    }
    Ok(())
}

fn zed_message_event(
    provider_session_id: &str,
    message: &Value,
    message_index: usize,
    occurred_at: DateTime<Utc>,
    event_type: EventType,
    event_suffix: &str,
    split_index: u64,
) -> Result<(ProviderEventEnvelope, String)> {
    let kind = zed_message_kind(message).unwrap_or("Unknown");
    let text = zed_message_text_for_event_type(kind, message, event_type)
        .unwrap_or_else(|| format!("Zed {kind} message"));
    let role = zed_message_role(kind);
    let message_event_index = u64::try_from(message_index).map_err(|_| {
        CaptureError::InvalidPayload(format!("Zed message index is too large: {message_index}"))
    })?;
    let provider_event_index = message_event_index
        .saturating_mul(ZED_EVENTS_PER_MESSAGE)
        .saturating_add(split_index);
    let provider_event_identity_index = if split_index == 0 {
        message_event_index
    } else {
        message_event_index
            .saturating_add(ZED_SPLIT_EVENT_IDENTITY_INDEX_OFFSET.saturating_mul(split_index))
    };
    let message_hash = if split_index == 0 && event_suffix == "message" {
        compute_payload_hash(message)?
    } else {
        compute_payload_hash(&json!({
            "event_suffix": event_suffix,
            "message": message,
        }))?
    };
    let cursor = if split_index == 0 && event_suffix == "message" {
        format!("thread:{provider_session_id}:message:{message_index}")
    } else {
        format!("thread:{provider_session_id}:message:{message_index}:{event_suffix}")
    };
    let event = native_event(NativeEventDraft {
        provider: CaptureProvider::Zed,
        source_format: ZED_THREADS_SQLITE_SOURCE_FORMAT,
        provider_session_id: provider_session_id.to_owned(),
        provider_event_index,
        provider_event_hash: Some(format!("zed-message:{message_hash}")),
        cursor,
        event_type,
        role,
        occurred_at,
        text: text.clone(),
        body: zed_message_body(kind, message, event_type),
        metadata: json!({
            "source": "zed_threads_db",
            "source_format": ZED_THREADS_SQLITE_SOURCE_FORMAT,
            "message_index": message_index,
            "message_kind": kind,
            "event_suffix": event_suffix,
            "split_index": split_index,
            "provider_event_identity_index": provider_event_identity_index,
            "timestamp_source": "thread.updated_at",
        }),
    });
    Ok((event, text))
}

fn zed_message_kind(message: &Value) -> Option<&str> {
    match message {
        Value::String(kind) => Some(kind.as_str()),
        Value::Object(object) if object.len() == 1 => object.keys().next().map(String::as_str),
        _ => None,
    }
}

fn zed_message_inner<'a>(message: &'a Value, kind: &str) -> Option<&'a Value> {
    match message {
        Value::Object(object) => object.get(kind),
        _ => None,
    }
}

fn zed_message_role(kind: &str) -> Option<EventRole> {
    Some(match kind {
        "User" | "Resume" => EventRole::User,
        "Agent" => EventRole::Assistant,
        "Compaction" => EventRole::System,
        _ => EventRole::Unknown,
    })
}

fn zed_message_event_type(kind: &str, message: &Value) -> EventType {
    match kind {
        "Agent" if zed_has_tool_result(message) => EventType::ToolOutput,
        "Agent" if zed_has_tool_use(message) => EventType::ToolCall,
        "User" | "Agent" | "Resume" => EventType::Message,
        "Compaction" => EventType::Summary,
        _ => EventType::Notice,
    }
}

fn zed_message_text(message: &Value) -> Option<String> {
    let kind = zed_message_kind(message)?;
    let inner = zed_message_inner(message, kind);
    match kind {
        "User" => zed_user_message_text(inner?),
        "Agent" => zed_agent_message_text(inner?),
        "Resume" => Some("[resume]".to_owned()),
        "Compaction" => zed_compaction_text(inner.unwrap_or(message)),
        _ => provider_value_text(message),
    }
}

fn zed_message_text_for_event_type(
    kind: &str,
    message: &Value,
    event_type: EventType,
) -> Option<String> {
    if kind == "Agent" {
        let inner = zed_message_inner(message, kind)?;
        return match event_type {
            EventType::ToolCall => zed_content_array_text(inner.get("content")),
            EventType::ToolOutput => zed_tool_results_text(inner.get("tool_results")),
            _ => zed_agent_message_text(inner),
        };
    }
    zed_message_text(message)
}

fn zed_message_body(kind: &str, message: &Value, event_type: EventType) -> Value {
    match event_type {
        EventType::ToolCall => json!({
            "message_kind": kind,
            "raw_message_retention": "metadata_only",
            "tool_uses": zed_tool_use_summaries(message),
        }),
        EventType::ToolOutput => json!({
            "message_kind": kind,
            "raw_message_retention": "metadata_only",
            "tool_results": zed_tool_result_summaries(message),
        }),
        _ => json!({
            "message_kind": kind,
            "message": message,
        }),
    }
}

fn zed_user_message_text(value: &Value) -> Option<String> {
    zed_content_array_text(value.get("content"))
}

fn zed_agent_message_text(value: &Value) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(text) = zed_content_array_text(value.get("content")) {
        parts.push(text);
    }
    if let Some(text) = zed_tool_results_text(value.get("tool_results")) {
        parts.push(text);
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn zed_compaction_text(value: &Value) -> Option<String> {
    if let Some(summary) = value.get("Summary").and_then(Value::as_str) {
        return Some(summary.to_owned());
    }
    if let Some(native) = value.get("ProviderNative") {
        return provider_value_text(native);
    }
    provider_value_text(value)
}

fn zed_content_array_text(value: Option<&Value>) -> Option<String> {
    let items = value?.as_array()?;
    let mut parts = Vec::new();
    for item in items {
        if let Some(text) = zed_content_item_text(item) {
            parts.push(text);
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn zed_content_item_text(value: &Value) -> Option<String> {
    let (kind, body) = zed_external_tag(value)?;
    match kind {
        "Text" => body.as_str().map(str::to_owned),
        "Thinking" => body
            .get("text")
            .and_then(Value::as_str)
            .map(|text| format!("<think>{text}</think>")),
        "RedactedThinking" => Some("<redacted_thinking />".to_owned()),
        "ToolUse" => Some(zed_tool_use_text(body)),
        "Mention" => zed_mention_text(body),
        "Image" => Some("<image />".to_owned()),
        other => provider_value_text(body).map(|text| format!("{other}: {text}")),
    }
}

fn zed_tool_use_text(value: &Value) -> String {
    let name = value.get("name").and_then(Value::as_str).unwrap_or("tool");
    let mut parts = vec![format!("tool call: {name}")];
    if value.get("input").is_some_and(|input| !input.is_null())
        || value
            .get("raw_input")
            .and_then(Value::as_str)
            .is_some_and(|raw_input| !raw_input.trim().is_empty())
    {
        parts.push("tool input: present".to_owned());
    }
    parts.join("\n")
}

fn zed_tool_use_summaries(value: &Value) -> Vec<Value> {
    let mut summaries = Vec::new();
    zed_collect_tool_use_summaries(value, &mut summaries);
    summaries
}

fn zed_collect_tool_use_summaries(value: &Value, summaries: &mut Vec<Value>) {
    match value {
        Value::Array(items) => {
            for item in items {
                zed_collect_tool_use_summaries(item, summaries);
            }
        }
        Value::Object(object) => {
            if let Some(tool_use) = object.get("ToolUse") {
                summaries.push(zed_tool_use_summary(tool_use));
            }
            for nested in object.values() {
                zed_collect_tool_use_summaries(nested, summaries);
            }
        }
        _ => {}
    }
}

fn zed_tool_use_summary(value: &Value) -> Value {
    let input = value.get("input").filter(|input| !input.is_null());
    json!({
        "id": value.get("id").and_then(Value::as_str),
        "name": value.get("name").and_then(Value::as_str),
        "input_present": input.is_some(),
        "input_kind": input.map(zed_value_kind),
        "raw_input_present": value
            .get("raw_input")
            .and_then(Value::as_str)
            .is_some_and(|raw_input| !raw_input.trim().is_empty()),
    })
}

fn zed_tool_result_summaries(value: &Value) -> Vec<Value> {
    let mut summaries = Vec::new();
    zed_collect_tool_result_summaries(value, &mut summaries);
    summaries
}

fn zed_collect_tool_result_summaries(value: &Value, summaries: &mut Vec<Value>) {
    match value {
        Value::Array(items) => {
            for item in items {
                zed_collect_tool_result_summaries(item, summaries);
            }
        }
        Value::Object(object) => {
            if let Some(tool_result) = object.get("ToolResult") {
                summaries.push(zed_tool_result_summary(tool_result));
            }
            if let Some(results) = object.get("tool_results").and_then(Value::as_object) {
                for result in results.values() {
                    summaries.push(zed_tool_result_summary(result));
                }
            }
            for nested in object.values() {
                zed_collect_tool_result_summaries(nested, summaries);
            }
        }
        _ => {}
    }
}

fn zed_tool_result_summary(value: &Value) -> Value {
    json!({
        "id": value.get("id").and_then(Value::as_str),
        "tool_name": value.get("tool_name").and_then(Value::as_str),
        "is_error": value.get("is_error").and_then(Value::as_bool).unwrap_or(false),
        "content_present": value.get("content").is_some_and(|content| !content.is_null()),
        "output_present": value.get("output").is_some_and(|output| !output.is_null()),
    })
}

fn zed_value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn zed_mention_text(value: &Value) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(uri) = value.get("uri") {
        if let Some(uri_text) = provider_value_text(uri) {
            parts.push(uri_text);
        }
    }
    if let Some(content) = value.get("content").and_then(Value::as_str) {
        parts.push(content.to_owned());
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn zed_tool_results_text(value: Option<&Value>) -> Option<String> {
    let object = value?.as_object()?;
    let mut parts = Vec::new();
    for result in object.values() {
        let name = result
            .get("tool_name")
            .and_then(Value::as_str)
            .unwrap_or("tool");
        parts.push(format!("tool result: {name}"));
        if result
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            parts.push("tool error".to_owned());
        }
        if let Some(content) = zed_tool_result_content_text(result.get("content")) {
            parts.push(content);
        }
        if let Some(output) = result.get("output").and_then(provider_value_text) {
            parts.push(output);
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn zed_tool_result_content_text(value: Option<&Value>) -> Option<String> {
    let value = value?;
    if let Some(text) = value.as_str() {
        return Some(text.to_owned());
    }
    if let Some(items) = value.as_array() {
        let mut parts = Vec::new();
        for item in items {
            if let Some((kind, body)) = zed_external_tag(item) {
                match kind {
                    "Text" => {
                        if let Some(text) = body.as_str() {
                            parts.push(text.to_owned());
                        }
                    }
                    "Image" => parts.push("<image />".to_owned()),
                    _ => {
                        if let Some(text) = provider_value_text(body) {
                            parts.push(text);
                        }
                    }
                }
            } else if let Some(text) = provider_value_text(item) {
                parts.push(text);
            }
        }
        return (!parts.is_empty()).then(|| parts.join("\n"));
    }
    provider_value_text(value)
}

fn zed_external_tag(value: &Value) -> Option<(&str, &Value)> {
    let object = value.as_object()?;
    if object.len() != 1 {
        return None;
    }
    object
        .iter()
        .next()
        .map(|(key, value)| (key.as_str(), value))
}

fn zed_has_tool_use(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(zed_has_tool_use),
        Value::Object(object) => {
            object.contains_key("ToolUse")
                || object.get("content").is_some_and(zed_has_tool_use)
                || object.values().any(zed_has_tool_use)
        }
        _ => false,
    }
}

fn zed_has_tool_result(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(zed_has_tool_result),
        Value::Object(object) => {
            object
                .get("tool_results")
                .and_then(Value::as_object)
                .is_some_and(|results| !results.is_empty())
                || object.contains_key("ToolResult")
                || object.values().any(zed_has_tool_result)
        }
        _ => false,
    }
}

#[cfg(test)]
#[path = "event/tests.rs"]
mod tests;
