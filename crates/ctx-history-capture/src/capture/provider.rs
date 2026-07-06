#[allow(unused_imports)]
use super::*;

pub(crate) const MAX_PROVIDER_JSONL_LINE_BYTES: usize = 16 * 1024 * 1024;

pub(crate) const PROVIDER_MAX_TEXT_CHARS: usize = 16_000;

pub(crate) const PROVIDER_MAX_PREVIEW_CHARS: usize = 4_000;

pub(crate) fn read_provider_jsonl_line(
    reader: &mut impl BufRead,
    buffer: &mut Vec<u8>,
) -> Result<bool> {
    buffer.clear();
    let mut total = 0usize;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(total > 0);
        }
        if let Some(newline_index) = available.iter().position(|byte| *byte == b'\n') {
            let bytes_to_consume = newline_index + 1;
            if total.saturating_add(bytes_to_consume) > MAX_PROVIDER_JSONL_LINE_BYTES {
                reader.consume(bytes_to_consume);
                return Err(provider_jsonl_line_too_large());
            }
            buffer.extend_from_slice(&available[..bytes_to_consume]);
            reader.consume(bytes_to_consume);
            return Ok(true);
        }

        let bytes_to_consume = available.len();
        if total.saturating_add(bytes_to_consume) > MAX_PROVIDER_JSONL_LINE_BYTES {
            reader.consume(bytes_to_consume);
            discard_provider_jsonl_line(reader)?;
            return Err(provider_jsonl_line_too_large());
        }
        buffer.extend_from_slice(available);
        reader.consume(bytes_to_consume);
        total = total.saturating_add(bytes_to_consume);
    }
}

pub(crate) fn discard_provider_jsonl_line(reader: &mut impl BufRead) -> Result<()> {
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(());
        }
        let bytes_to_consume = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(available.len());
        let found_newline = available
            .get(bytes_to_consume.saturating_sub(1))
            .is_some_and(|byte| *byte == b'\n');
        reader.consume(bytes_to_consume);
        if found_newline {
            return Ok(());
        }
    }
}

pub(crate) fn provider_jsonl_line_too_large() -> CaptureError {
    CaptureError::InvalidPayload(format!(
        "provider JSONL line exceeds max bytes ({MAX_PROVIDER_JSONL_LINE_BYTES})"
    ))
}

pub(crate) fn provider_local_preview(value: &str, max_chars: usize) -> (String, bool) {
    capped_text(value, max_chars)
}

pub(crate) fn provider_file_touch_envelopes(
    context: ProviderFileTouchEnvelopeContext<'_>,
    drafts: Vec<FileTouchDraft>,
) -> Vec<(usize, ProviderFileTouchedEnvelope)> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for draft in drafts {
        let key = (
            draft.path.clone(),
            draft.old_path.clone(),
            draft.change_kind.map(|kind| kind.as_str().to_owned()),
        );
        if !seen.insert(key) {
            continue;
        }
        let provider_touch_index = context.provider_touch_base_index | (out.len() as u64);
        out.push((
            context.line_number,
            ProviderFileTouchedEnvelope {
                provider: context.provider,
                provider_session_id: context.provider_session_id.to_owned(),
                provider_touch_index,
                provider_event_index: context.provider_event_index,
                raw_source_path: context.raw_source_path.map(str::to_owned),
                path: draft.path,
                change_kind: draft.change_kind,
                old_path: draft.old_path,
                line_count_delta: None,
                confidence: draft.confidence,
                occurred_at: context.occurred_at,
                source_format: context.source_format.to_owned(),
                metadata: draft.metadata,
            },
        ));
    }
    out
}

pub(crate) fn provider_value_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                if let Some(text) = block
                    .get("text")
                    .or_else(|| block.get("content"))
                    .or_else(|| block.get("output"))
                    .or_else(|| block.get("summary"))
                    .and_then(Value::as_str)
                {
                    parts.push(text.to_owned());
                    continue;
                }
                if let Some(kind) = block.get("type").and_then(Value::as_str) {
                    if matches!(
                        kind,
                        "tool_use" | "tool" | "toolCall" | "function_call" | "agent"
                    ) {
                        let name = block
                            .get("name")
                            .or_else(|| block.get("tool"))
                            .and_then(Value::as_str)
                            .unwrap_or("tool");
                        parts.push(format!("tool call: {name}"));
                    } else if kind == "tool_result" {
                        parts.push("tool result".to_owned());
                    }
                }
            }
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        Value::Object(_) => serde_json::to_string(value).ok(),
        Value::Number(_) | Value::Bool(_) => Some(value.to_string()),
        Value::Null => None,
    }
}

pub(crate) fn provider_nonnegative_i64_to_u64(value: i64, field: &'static str) -> Result<u64> {
    u64::try_from(value).map_err(|_| {
        CaptureError::InvalidPayload(format!("{field} must be nonnegative, got {value}"))
    })
}

pub(crate) fn provider_line_from_index(index: u64) -> usize {
    index.min(usize::MAX as u64) as usize
}

pub(crate) fn continue_history_item_text(item: &Value) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(text) = item
        .pointer("/message/content")
        .and_then(provider_value_text)
        .or_else(|| item.get("editorState").and_then(provider_value_text))
    {
        parts.push(text);
    }
    if let Some(text) = item
        .get("contextItems")
        .and_then(continue_context_items_text)
    {
        parts.push(text);
    }
    if let Some(text) = item
        .get("toolCallStates")
        .and_then(continue_tool_states_text)
    {
        parts.push(text);
    }
    if let Some(text) = item.get("conversationSummary").and_then(Value::as_str) {
        parts.push(text.to_owned());
    }
    let text = parts
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then_some(text)
}

pub(crate) fn continue_context_items_text(value: &Value) -> Option<String> {
    let items = value.as_array()?;
    let mut parts = Vec::new();
    for item in items {
        if let Some(content) = item.get("content").and_then(provider_value_text) {
            parts.push(content);
        } else if let Some(name) = item.get("name").and_then(Value::as_str) {
            parts.push(name.to_owned());
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

pub(crate) fn provider_string_field(value: &Value, fields: &[&str]) -> Option<String> {
    fields.iter().find_map(|field| {
        value
            .get(*field)
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .map(str::to_owned)
    })
}

pub(crate) fn provider_message_id(value: &Value, fallback_index: u64) -> String {
    value
        .get("id")
        .or_else(|| value.get("message_id"))
        .or_else(|| value.get("messageId"))
        .or_else(|| value.get("request_id"))
        .or_else(|| value.get("requestId"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("message-{fallback_index}"))
}

pub(crate) fn provider_message_has_part_kind(value: &Value, kinds: &[&str]) -> bool {
    provider_message_parts(value)
        .map(|parts| {
            parts.iter().any(|part| {
                part.get("type")
                    .or_else(|| part.get("kind"))
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kinds.contains(&kind))
            })
        })
        .unwrap_or(false)
}

pub(crate) fn provider_block_text(value: &Value) -> Option<String> {
    for key in [
        "text", "content", "message", "prompt", "response", "output", "summary",
    ] {
        if let Some(text) = value.get(key).and_then(provider_value_text) {
            if !text.trim().is_empty() {
                return Some(text);
            }
        }
    }
    let parts = provider_message_parts(value)?;
    let mut rendered = Vec::new();
    for part in parts {
        if let Some(text) = provider_part_text(part) {
            rendered.push(text);
        }
    }
    (!rendered.is_empty()).then(|| rendered.join("\n"))
}

pub(crate) fn provider_message_parts(value: &Value) -> Option<&Vec<Value>> {
    value
        .get("parts")
        .or_else(|| value.get("content"))
        .or_else(|| value.get("blocks"))
        .and_then(Value::as_array)
}

pub(crate) fn provider_part_text(part: &Value) -> Option<String> {
    let kind = part
        .get("type")
        .or_else(|| part.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if matches!(
        kind,
        "tool_use" | "tool-use" | "tool_call" | "tool-call" | "function_call"
    ) {
        let name = part
            .get("name")
            .or_else(|| part.get("tool"))
            .or_else(|| part.get("tool_name"))
            .or_else(|| part.get("toolName"))
            .and_then(Value::as_str)
            .unwrap_or("tool");
        return Some(format!("tool call: {name}"));
    }
    if matches!(
        kind,
        "tool_result" | "tool-result" | "tool_use_result" | "function_result"
    ) {
        return part
            .get("content")
            .or_else(|| part.get("result"))
            .or_else(|| part.get("output"))
            .and_then(provider_value_text)
            .or_else(|| Some("tool result".to_owned()));
    }
    part.get("text")
        .or_else(|| part.get("content"))
        .or_else(|| part.get("thinking"))
        .or_else(|| part.get("summary"))
        .and_then(provider_value_text)
}

pub(crate) fn provider_command_duration_ms(payload: &Value) -> Result<Option<i64>> {
    let Some(value) = payload.get("duration_ms") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let duration = value
        .as_i64()
        .ok_or_else(|| CaptureError::InvalidPayload("duration_ms must be an integer".to_owned()))?;
    if duration < 0 {
        return Err(CaptureError::InvalidPayload(format!(
            "duration_ms must be nonnegative, got {duration}"
        )));
    }
    Ok(Some(duration))
}

pub(crate) fn provider_sync_metadata(fidelity: Fidelity, metadata: Value) -> SyncMetadata {
    SyncMetadata {
        visibility: Visibility::default(),
        fidelity,
        sync_state: SyncState::default(),
        sync_version: 0,
        deleted_at: None,
        metadata,
    }
}
