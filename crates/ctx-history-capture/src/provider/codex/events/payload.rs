pub(crate) fn codex_output_text(value: &Value) -> Cow<'_, str> {
    match value {
        Value::String(text) => Cow::Borrowed(text),
        Value::Null => Cow::Borrowed(""),
        other => Cow::Owned(serde_json::to_string(other).unwrap_or_else(|_| other.to_string())),
    }
}

fn codex_output_exit_code(value: &Value) -> Option<i32> {
    match value {
        Value::Object(object) => {
            for key in ["exit_code", "exitCode"] {
                if let Some(code) = object
                    .get(key)
                    .and_then(Value::as_i64)
                    .and_then(|code| i32::try_from(code).ok())
                {
                    return Some(code);
                }
            }
            object.values().find_map(codex_output_exit_code)
        }
        Value::Array(items) => items.iter().find_map(codex_output_exit_code),
        _ => None,
    }
}
pub(crate) fn codex_reasoning_event(
    payload: &Value,
    line_number: usize,
    occurred_at: DateTime<Utc>,
) -> Option<ProviderEventEnvelope> {
    let summary = payload
        .get("summary")
        .and_then(codex_content_text)
        .or_else(|| {
            payload
                .get("summary_text")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })?;
    let (summary, truncated) = codex_local_preview(&summary, PROVIDER_MAX_TEXT_CHARS);
    Some(codex_provider_event(
        line_number,
        occurred_at,
        EventType::Summary,
        Some(EventRole::Assistant),
        json!({
            "item_type": "reasoning",
            "summary": summary,
            "text": summary,
            "truncated": truncated,
            "encrypted_content_present": payload.get("encrypted_content").is_some(),
        }),
        json!({
            "source": "codex_session",
            "source_format": CODEX_SESSION_SOURCE_FORMAT,
            "line": line_number,
            "item_type": "reasoning",
        }),
    ))
}
pub(crate) fn codex_message_event(
    payload: &Value,
    line_number: usize,
    occurred_at: DateTime<Utc>,
) -> Option<ProviderEventEnvelope> {
    let role_text = payload
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if !matches!(role_text, "user" | "assistant" | "developer" | "system") {
        return None;
    }
    let text = payload.get("content").and_then(codex_content_text)?;
    let (text, truncated) = capped_text(&text, PROVIDER_MAX_TEXT_CHARS);
    Some(codex_provider_event(
        line_number,
        occurred_at,
        EventType::Message,
        Some(codex_event_role(role_text)),
        json!({
            "item_type": "message",
            "message_role": role_text,
            "phase": payload.get("phase").and_then(Value::as_str),
            "text": text,
            "truncated": truncated,
        }),
        json!({
            "source": "codex_session",
            "source_format": CODEX_SESSION_SOURCE_FORMAT,
            "import_scope": "fast_transcript_index",
            "line": line_number,
            "item_type": "message",
            "message_role": role_text,
        }),
    ))
}
pub(crate) fn codex_provider_event(
    line_number: usize,
    occurred_at: DateTime<Utc>,
    event_type: EventType,
    role: Option<EventRole>,
    payload: Value,
    metadata: Value,
) -> ProviderEventEnvelope {
    ProviderEventEnvelope {
        provider_event_index: (line_number - 1) as u64,
        provider_event_hash: None,
        cursor: Some(format!("line:{line_number}")),
        event_type,
        role,
        occurred_at,
        fidelity: Fidelity::Imported,
        idempotency_key: Some(format!("provider-event:codex-session:{line_number}")),
        artifacts: Vec::new(),
        payload,
        metadata,
    }
}
pub(crate) fn codex_lifecycle_body(payload: &Value, msg_type: &str) -> Value {
    let preview = payload
        .get("last_agent_message")
        .or_else(|| payload.get("message"))
        .or_else(|| payload.get("stdout"))
        .or_else(|| payload.get("stderr"))
        .and_then(codex_json_text)
        .unwrap_or_else(|| format!("Codex lifecycle: {msg_type}"));
    let (text, truncated) = codex_local_preview(&preview, PROVIDER_MAX_PREVIEW_CHARS);
    json!({
        "text": text,
        "event_msg_type": msg_type,
        "status": payload.get("status").and_then(Value::as_str),
        "success": payload.get("success").and_then(Value::as_bool),
        "duration_ms": payload.get("duration_ms").and_then(Value::as_i64),
        "time_to_first_token_ms": payload.get("time_to_first_token_ms").and_then(Value::as_i64),
        "truncated": truncated,
    })
}
pub(crate) fn codex_tool_name(payload: &Value, item_type: &str) -> String {
    payload
        .get("name")
        .or_else(|| payload.get("tool"))
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(item_type)
        .to_owned()
}
pub(crate) fn codex_is_command_tool(tool_name: &str) -> bool {
    matches!(tool_name, "exec_command" | "shell" | "bash" | "command")
}
pub(crate) fn codex_command_preview(
    tool_name: &str,
    argument_value: Option<&Value>,
) -> Option<String> {
    if !codex_is_command_tool(tool_name) {
        return None;
    }
    let value = argument_value?;
    let parsed = codex_parse_embedded_json(value).unwrap_or_else(|| value.clone());
    let command = parsed
        .get("cmd")
        .or_else(|| parsed.get("command"))
        .or_else(|| parsed.get("shell_command"))
        .and_then(Value::as_str)
        .or_else(|| value.as_str())?;
    Some(codex_local_preview(command, PROVIDER_MAX_PREVIEW_CHARS).0)
}
pub(crate) fn codex_value_preview(value: &Value, max_chars: usize) -> (String, bool) {
    let rendered = match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        _ => serde_json::to_string(value).unwrap_or_else(|_| value.to_string()),
    };
    codex_local_preview(&rendered, max_chars)
}
pub(crate) fn codex_tool_arguments_preview(value: &Value) -> (String, bool, bool) {
    let parsed = codex_parse_embedded_json(value).unwrap_or_else(|| value.clone());
    let mut file_touches = Vec::new();
    collect_patch_file_touches(&parsed, &mut file_touches);
    collect_structured_file_touches(&parsed, &mut file_touches);
    if !file_touches.is_empty() {
        return codex_file_touch_arguments_preview(&file_touches);
    }
    let (retained, fields_omitted) = codex_tool_argument_value_with_omissions(&parsed, None);
    let (preview, truncated) = codex_value_preview(&retained, PROVIDER_MAX_PREVIEW_CHARS);
    (preview, truncated, !fields_omitted)
}
pub(crate) fn codex_file_touch_arguments_preview(
    file_touches: &[crate::provider::file_touches::FileTouchDraft],
) -> (String, bool, bool) {
    let paths = file_touches
        .iter()
        .take(12)
        .map(|touch| match touch.change_kind {
            Some(kind) => format!("{}:{}", kind.as_str(), touch.path),
            None => touch.path.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let omitted = file_touches.len().saturating_sub(12);
    let suffix = if omitted == 0 {
        String::new()
    } else {
        format!(", +{omitted} more")
    };
    (format!("file touches: {paths}{suffix}"), omitted > 0, false)
}
pub(crate) fn codex_tool_argument_value_with_omissions(
    value: &Value,
    key: Option<&str>,
) -> (Value, bool) {
    if key.is_some_and(|key| codex_tool_argument_key_should_omit(key, value)) {
        return (codex_omitted_argument_value(value), true);
    }
    match value {
        Value::Array(items) => {
            let mut fields_omitted = false;
            let items = items
                .iter()
                .map(|item| {
                    let (item, item_omitted) = codex_tool_argument_value_with_omissions(item, key);
                    fields_omitted |= item_omitted;
                    item
                })
                .collect();
            (Value::Array(items), fields_omitted)
        }
        Value::Object(object) => {
            let mut fields_omitted = false;
            let object = object
                .iter()
                .map(|(key, value)| {
                    let (value, value_omitted) =
                        codex_tool_argument_value_with_omissions(value, Some(key));
                    fields_omitted |= value_omitted;
                    (key.clone(), value)
                })
                .collect();
            (Value::Object(object), fields_omitted)
        }
        _ => (value.clone(), false),
    }
}
pub(crate) fn codex_tool_argument_key_should_omit(key: &str, value: &Value) -> bool {
    let key = codex_normalized_key(key);
    matches!(
        key.as_str(),
        "content"
            | "text"
            | "body"
            | "diff"
            | "patch"
            | "oldstring"
            | "newstring"
            | "oldcontent"
            | "newcontent"
            | "beforecontent"
            | "aftercontent"
            | "beforetext"
            | "aftertext"
            | "replacement"
            | "oldstr"
            | "newstr"
            | "inputtext"
            | "outputtext"
    ) || (matches!(key.as_str(), "input" | "arguments" | "args" | "params")
        && codex_value_contains_patch_or_diff(value))
}
pub(crate) fn codex_omitted_argument_value(value: &Value) -> Value {
    json!({
        "field_retention": {
            "mode": "omitted",
            "original_bytes": codex_value_approx_bytes(value),
            "contained_patch_or_diff": codex_value_contains_patch_or_diff(value),
        },
    })
}
pub(crate) fn codex_normalized_key(key: &str) -> String {
    key.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
pub(crate) fn codex_value_approx_bytes(value: &Value) -> usize {
    match value {
        Value::String(text) => text.len(),
        _ => serde_json::to_string(value)
            .map(|text| text.len())
            .unwrap_or_default(),
    }
}
pub(crate) fn codex_value_contains_patch_or_diff(value: &Value) -> bool {
    match value {
        Value::String(text) => codex_text_contains_patch_or_diff(text),
        Value::Array(items) => items.iter().any(codex_value_contains_patch_or_diff),
        Value::Object(object) => object.values().any(codex_value_contains_patch_or_diff),
        _ => false,
    }
}
pub(crate) fn codex_text_contains_patch_or_diff(text: &str) -> bool {
    text.contains("*** Begin Patch")
        || text.contains("diff --git ")
        || text.starts_with("@@")
        || text.starts_with("+++ ")
        || text.starts_with("--- ")
        || text.contains("\n@@")
        || text.contains("\n+++ ")
        || text.contains("\n--- ")
}
pub(crate) fn codex_local_preview(value: &str, max_chars: usize) -> (String, bool) {
    capped_text(value, max_chars)
}
pub(crate) fn codex_parse_embedded_json(value: &Value) -> Option<Value> {
    match value {
        Value::String(text) => serde_json::from_str::<Value>(text).ok(),
        Value::Object(_) | Value::Array(_) => Some(value.clone()),
        _ => None,
    }
}
pub(crate) fn codex_timed_out(payload: &Value) -> Option<bool> {
    payload
        .get("timed_out")
        .and_then(Value::as_bool)
        .or_else(|| {
            payload
                .get("output")
                .and_then(codex_parse_embedded_json)
                .and_then(|value| {
                    value
                        .get("timed_out")
                        .and_then(Value::as_bool)
                        .or_else(|| value.pointer("/status/timed_out").and_then(Value::as_bool))
                })
        })
}
pub(crate) fn codex_exit_code(text: &str) -> Option<i32> {
    let marker = "Process exited with code ";
    let index = text.find(marker)? + marker.len();
    let tail = &text[index..];
    let digits = tail
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '-')
        .collect::<String>();
    digits.parse().ok()
}
pub(crate) fn codex_wall_time_ms(text: &str) -> Option<i64> {
    let marker = "Wall time: ";
    let index = text.find(marker)? + marker.len();
    let tail = &text[index..];
    let seconds_text = tail
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '.')
        .collect::<String>();
    let seconds = seconds_text.parse::<f64>().ok()?;
    Some((seconds * 1000.0).round() as i64)
}
pub(crate) fn codex_event_role(role: &str) -> EventRole {
    match role {
        "user" => EventRole::User,
        "assistant" => EventRole::Assistant,
        "tool" => EventRole::Tool,
        "system" | "developer" => EventRole::System,
        _ => EventRole::Unknown,
    }
}
pub(crate) fn codex_content_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                if let Some(text) = block
                    .get("text")
                    .or_else(|| block.get("input_text"))
                    .or_else(|| block.get("output_text"))
                    .or_else(|| block.get("summary_text"))
                    .and_then(Value::as_str)
                {
                    parts.push(text.to_owned());
                    continue;
                }
                if let Some(text) = block.get("content").and_then(Value::as_str) {
                    parts.push(text.to_owned());
                    continue;
                }
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        Value::Object(object) => {
            for key in [
                "text",
                "input_text",
                "output_text",
                "summary_text",
                "content",
            ] {
                if let Some(text) = object.get(key).and_then(Value::as_str) {
                    return Some(text.to_owned());
                }
                if let Some(text) = object.get(key).and_then(codex_content_text) {
                    return Some(text);
                }
            }
            None
        }
        _ => None,
    }
}
pub(crate) fn codex_json_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(value).ok(),
        _ => Some(value.to_string()),
    }
}
pub(crate) fn codex_capped_json(value: &Value, max_chars: usize) -> Value {
    match value {
        Value::String(text) => {
            let (text, truncated) = capped_text(text, max_chars);
            json!({ "text": text, "truncated": truncated })
        }
        _ => {
            let rendered = serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned());
            let (text, truncated) = capped_text(&rendered, max_chars);
            json!({ "json": text, "truncated": truncated })
        }
    }
}
