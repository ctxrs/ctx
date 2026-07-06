#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderEventDto {
    pub provider_event_index: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_event_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default)]
    pub event_type: EventType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<EventRole>,
    pub occurred_at: DateTime<Utc>,
    #[serde(default = "default_metadata")]
    pub payload: Value,
    #[serde(default = "default_metadata")]
    pub metadata: Value,
}

#[derive(Debug, Clone)]
pub struct NormalizedProviderImportOptions {
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
    pub persist_cursors: bool,
    pub wrap_transaction: bool,
    pub fast_event_inserts: bool,
}

impl Default for NormalizedProviderImportOptions {
    fn default() -> Self {
        Self {
            history_record_id: None,
            allow_partial_failures: false,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: false,
        }
    }
}

pub(crate) fn event_type_supports_structured_file_touches(event_type: EventType) -> bool {
    matches!(event_type, EventType::ToolCall | EventType::FileTouched)
}

pub(crate) fn provider_role(value: Option<&str>) -> EventRole {
    match value {
        Some("user") => EventRole::User,
        Some("assistant") => EventRole::Assistant,
        Some("system" | "developer") => EventRole::System,
        Some("tool" | "toolResult" | "bashExecution") => EventRole::Tool,
        _ => EventRole::Unknown,
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TaskJsonEventInput {
    pub(crate) source: &'static str,
    pub(crate) native_index: usize,
    pub(crate) raw: Value,
}

pub(crate) fn task_json_push_message_events(
    out: &mut Vec<TaskJsonEventInput>,
    value: &Value,
    source: &'static str,
) {
    match value {
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                out.push(TaskJsonEventInput {
                    source,
                    native_index: index,
                    raw: item.clone(),
                });
            }
        }
        Value::Object(object) => {
            if let Some(items) = object
                .get("messages")
                .or_else(|| object.get("history"))
                .and_then(Value::as_array)
            {
                for (index, item) in items.iter().enumerate() {
                    out.push(TaskJsonEventInput {
                        source,
                        native_index: index,
                        raw: item.clone(),
                    });
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn task_json_history_item_event(value: &Value) -> Option<Value> {
    let text = task_json_string_field(value, &["task", "title", "summary", "name"])?;
    let mut object = serde_json::Map::new();
    object.insert("role".to_owned(), Value::String("user".to_owned()));
    object.insert("content".to_owned(), Value::String(text));
    object.insert("type".to_owned(), Value::String("history_item".to_owned()));
    if let Some(ts) = value
        .get("ts")
        .or_else(|| value.get("timestamp"))
        .or_else(|| value.get("createdAt"))
    {
        object.insert("timestamp".to_owned(), ts.clone());
    }
    Some(Value::Object(object))
}

pub(crate) fn task_json_event(
    spec: TaskJsonProviderSpec,
    task_id: &str,
    input: TaskJsonEventInput,
    event_ordinal: usize,
    occurred_at: DateTime<Utc>,
) -> ProviderEventEnvelope {
    let event_type = task_json_event_type(&input.raw, input.source);
    let role = Some(task_json_event_role(&input.raw, input.source));
    let text = task_json_event_text(&input.raw, input.source, event_type);
    let (text, truncated) = provider_local_preview(&text, PROVIDER_MAX_TEXT_CHARS);
    let native_id = task_json_string_field(&input.raw, &["id", "uuid", "messageId"])
        .unwrap_or_else(|| format!("{}-{}", input.source, input.native_index));
    let event_id = format!("{task_id}:{}:{native_id}", input.source);

    ProviderEventEnvelope {
        provider_event_index: event_ordinal as u64,
        provider_event_hash: Some(event_id.clone()),
        cursor: Some(event_id.clone()),
        event_type,
        role,
        occurred_at,
        fidelity: Fidelity::Imported,
        redaction_state: RedactionState::LocalPreview,
        idempotency_key: Some(format!(
            "provider-event:{}:{}:{event_id}",
            spec.provider.as_str(),
            spec.source_format
        )),
        artifacts: Vec::new(),
        payload: json!({
            "entry_type": task_json_entry_type(&input.raw, input.source),
            "event_id": event_id,
            "native_index": input.native_index,
            "text": text,
            "truncated": truncated,
            "body": provider_capped_json(&input.raw, PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": input.source,
            "source_format": spec.source_format,
            "native_index": input.native_index,
            "role": task_json_string_field(&input.raw, &["role"]),
            "model": task_json_model(&input.raw),
            "usage": task_json_usage(&input.raw),
        }),
    }
}

pub(crate) fn task_json_event_type(value: &Value, source: &str) -> EventType {
    if task_json_content_has(value, "tool_result") {
        return EventType::ToolOutput;
    }
    if task_json_content_has(value, "tool_use") {
        return EventType::ToolCall;
    }
    match source {
        "ui_messages" => match task_json_string_field(value, &["type", "say", "ask"]).as_deref() {
            Some("ask" | "say" | "user" | "assistant" | "text") => EventType::Message,
            Some("command" | "execute_command" | "shell") => EventType::CommandOutput,
            Some("completion_result" | "summary") => EventType::Summary,
            _ => EventType::Notice,
        },
        _ => match task_json_string_field(value, &["type", "role"]).as_deref() {
            Some("user" | "assistant" | "system") => EventType::Message,
            Some("tool_result") => EventType::ToolOutput,
            Some("tool_use") => EventType::ToolCall,
            Some("history_item" | "summary") => EventType::Summary,
            _ => EventType::Message,
        },
    }
}

pub(crate) fn task_json_event_role(value: &Value, source: &str) -> EventRole {
    if let Some(role) = task_json_string_field(value, &["role"]) {
        return provider_role(Some(&role));
    }
    if source == "ui_messages" {
        match task_json_string_field(value, &["type"]).as_deref() {
            Some("ask") => EventRole::User,
            Some("say") => EventRole::Assistant,
            _ => EventRole::Unknown,
        }
    } else {
        EventRole::Unknown
    }
}

pub(crate) fn task_json_event_text(value: &Value, source: &str, event_type: EventType) -> String {
    value
        .get("content")
        .or_else(|| value.pointer("/message/content"))
        .and_then(provider_value_text)
        .or_else(|| value.get("text").and_then(Value::as_str).map(str::to_owned))
        .or_else(|| value.get("message").and_then(provider_value_text))
        .or_else(|| {
            value
                .get("summary")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| {
            if event_type == EventType::Notice {
                format!("Task JSON event: {}", task_json_entry_type(value, source))
            } else {
                serde_json::to_string(value).unwrap_or_else(|_| source.to_owned())
            }
        })
}

pub(crate) fn task_json_event_time(value: &Value) -> Option<DateTime<Utc>> {
    task_json_time_field(
        value,
        &["timestamp", "ts", "createdAt", "created_at", "time", "date"],
    )
}

pub(crate) fn native_event(draft: NativeEventDraft) -> ProviderEventEnvelope {
    let (text, truncated) = provider_local_preview(&draft.text, PROVIDER_MAX_TEXT_CHARS);
    ProviderEventEnvelope {
        provider_event_index: draft.provider_event_index,
        provider_event_hash: draft.provider_event_hash,
        cursor: Some(draft.cursor),
        event_type: draft.event_type,
        role: draft.role,
        occurred_at: draft.occurred_at,
        fidelity: Fidelity::Imported,
        redaction_state: RedactionState::LocalPreview,
        idempotency_key: Some(format!(
            "provider-event:{}:{}:{}",
            draft.provider.as_str(),
            draft.provider_session_id,
            draft.provider_event_index
        )),
        artifacts: Vec::new(),
        payload: json!({
            "text": text,
            "truncated": truncated,
            "source_format": draft.source_format,
            "body": provider_capped_json(&draft.body, PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: draft.metadata,
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DeepAgentsMessage {
    pub(crate) role: EventRole,
    pub(crate) message_type: String,
    pub(crate) message_class: Option<String>,
    pub(crate) message_id: Option<String>,
    pub(crate) text: String,
}

pub(crate) struct ForgeCodeCaptureContext<'a> {
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) raw_source_path: &'a str,
    pub(crate) user_version: i64,
    pub(crate) schema_fingerprint: &'a str,
    pub(crate) context_value: Option<&'a Value>,
    pub(crate) metrics_value: Option<&'a Value>,
    pub(crate) event: Option<ProviderEventEnvelope>,
}

pub(crate) fn provider_role_from_message(value: &Value, role_text: Option<&str>) -> EventRole {
    let role = role_text.or_else(|| value.get("kind").and_then(Value::as_str));
    match role {
        Some("user" | "human" | "user_prompt" | "user-prompt") => EventRole::User,
        Some("assistant" | "agent" | "ai" | "model") => EventRole::Assistant,
        Some("system" | "developer" | "system_prompt" | "system-prompt") => EventRole::System,
        Some("tool" | "tool_result" | "tool-result" | "tool_use_result") => EventRole::Tool,
        _ => EventRole::Unknown,
    }
}

pub(crate) fn provider_block_event_type(value: &Value, role_text: Option<&str>) -> EventType {
    let role = role_text.unwrap_or_default();
    if role.contains("tool_result")
        || role.contains("tool-result")
        || provider_message_has_part_kind(value, &["tool_result", "tool-result"])
    {
        EventType::ToolOutput
    } else if role.contains("tool_use")
        || role.contains("tool-use")
        || provider_message_has_part_kind(
            value,
            &["tool_use", "tool-use", "tool-call", "tool_call"],
        )
    {
        EventType::ToolCall
    } else if matches!(
        role,
        "system" | "developer" | "system_prompt" | "system-prompt"
    ) {
        EventType::Notice
    } else {
        EventType::Message
    }
}

pub(crate) fn effective_event_redaction_state(
    requested: RedactionState,
    sanitizer_redacted: bool,
) -> RedactionState {
    match requested {
        RedactionState::Withheld => RedactionState::Withheld,
        RedactionState::Redacted => RedactionState::Redacted,
        RedactionState::Raw if !sanitizer_redacted => RedactionState::Raw,
        _ if sanitizer_redacted => RedactionState::Redacted,
        _ => RedactionState::LocalPreview,
    }
}

#[derive(Clone)]
pub(crate) struct ProviderEventImportIdentity {
    pub(crate) id: Uuid,
    pub(crate) seq: u64,
    pub(crate) dedupe_key: String,
    pub(crate) run_source_id: Option<Uuid>,
}

pub(crate) fn provider_event_id_exists(store: &Store, id: Uuid) -> Result<bool> {
    match store.get_event(id) {
        Ok(_) => Ok(true),
        Err(StoreError::NotFound(_)) => Ok(false),
        Err(err) => Err(CaptureError::Store(err)),
    }
}
