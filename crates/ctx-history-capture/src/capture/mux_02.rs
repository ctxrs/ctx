#[allow(unused_imports)]
use super::*;

pub(crate) fn mux_event_text(value: &Value, event_type: EventType) -> String {
    let mut rendered = Vec::new();
    if let Some(parts) = value.get("parts").and_then(Value::as_array) {
        for part in parts {
            match part.get("type").and_then(Value::as_str) {
                Some("text" | "reasoning") => {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        rendered.push(text.to_owned());
                    }
                }
                Some("dynamic-tool") => rendered.push(mux_tool_part_text(part)),
                Some("file") => {
                    if let Some(text) = mux_file_part_text(part) {
                        rendered.push(text);
                    }
                }
                _ => {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        rendered.push(text.to_owned());
                    }
                }
            }
        }
    }
    if !rendered.is_empty() {
        return rendered.join("\n");
    }
    if let Some(text) = value
        .get("content")
        .or_else(|| value.get("message"))
        .and_then(provider_value_text)
    {
        return text;
    }
    match event_type {
        EventType::ToolOutput => "Mux tool output".to_owned(),
        EventType::ToolCall => "Mux tool call".to_owned(),
        EventType::Summary => "Mux summary".to_owned(),
        EventType::Notice => "Mux notice".to_owned(),
        _ => "Mux message".to_owned(),
    }
}

pub(crate) fn mux_tool_part_text(part: &Value) -> String {
    let name = part
        .get("toolName")
        .or_else(|| part.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let state = part.get("state").and_then(Value::as_str);
    let prefix = if matches!(state, Some("output-available" | "output-redacted"))
        || part.get("output").is_some()
    {
        "tool output"
    } else {
        "tool call"
    };
    let mut text = format!("{prefix}: {name}");
    if let Some(input) = part.get("input") {
        text.push('\n');
        text.push_str("input: ");
        text.push_str(&mux_value_preview(input));
    }
    if let Some(output) = part.get("output") {
        text.push('\n');
        text.push_str("output: ");
        text.push_str(&mux_value_preview(output));
    }
    if let Some(nested) = part.get("nestedCalls").and_then(Value::as_array) {
        let names = nested
            .iter()
            .filter_map(|call| {
                call.get("toolName")
                    .or_else(|| call.get("name"))
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>();
        if !names.is_empty() {
            text.push('\n');
            text.push_str("nested tools: ");
            text.push_str(&names.join(", "));
        }
    }
    text
}

pub(crate) fn mux_file_part_text(part: &Value) -> Option<String> {
    let label = part
        .get("filename")
        .or_else(|| part.get("name"))
        .or_else(|| part.get("mediaType"))
        .or_else(|| part.get("mimeType"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            part.get("url")
                .and_then(Value::as_str)
                .filter(|url| !url.starts_with("data:") && url.len() < 256)
                .map(str::to_owned)
        })?;
    Some(format!("file: {label}"))
}

pub(crate) fn mux_value_preview(value: &Value) -> String {
    let raw = provider_value_text(value)
        .or_else(|| serde_json::to_string(value).ok())
        .unwrap_or_else(|| value.to_string());
    provider_local_preview(&raw, PROVIDER_MAX_PREVIEW_CHARS).0
}

pub(crate) fn mux_event_id(
    value: &Value,
    line_number: usize,
    role: &str,
    is_partial: bool,
) -> String {
    let prefix = if is_partial { "partial:" } else { "" };
    value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(|id| format!("{prefix}{id}"))
        .or_else(|| {
            mux_history_sequence(value)
                .map(|sequence| format!("{prefix}historySequence:{sequence}"))
        })
        .unwrap_or_else(|| format!("{prefix}{role}:line-{line_number}"))
}

pub(crate) fn mux_history_sequence(value: &Value) -> Option<i64> {
    match value.pointer("/metadata/historySequence") {
        Some(Value::Number(number)) => number
            .as_i64()
            .or_else(|| number.as_u64().and_then(|value| i64::try_from(value).ok())),
        Some(Value::String(raw)) => raw.parse::<i64>().ok(),
        _ => None,
    }
}

pub(crate) fn mux_parts_len(value: &Value) -> usize {
    value
        .get("parts")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0)
}

pub(crate) fn mux_session_id_from_rows(rows: &[MuxMessageRow]) -> Option<String> {
    rows.iter().find_map(|row| {
        row.value
            .get("workspaceId")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(str::to_owned)
    })
}

pub(crate) fn mux_message_model(value: &Value) -> Option<String> {
    mux_string_pointer(value, &["/metadata/model", "/model"])
}

pub(crate) fn mux_message_timestamp_opt(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("createdAt")
        .and_then(mux_value_timestamp)
        .or_else(|| {
            value
                .pointer("/metadata/timestamp")
                .and_then(mux_value_timestamp)
        })
        .or_else(|| {
            value
                .get("parts")
                .and_then(Value::as_array)
                .and_then(|parts| {
                    parts
                        .iter()
                        .find_map(|part| part.get("timestamp").and_then(mux_value_timestamp))
                })
        })
}

pub(crate) fn mux_metadata_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    ["/createdAt", "/createdAtMs", "/updatedAt", "/updatedAtMs"]
        .iter()
        .find_map(|pointer| value.pointer(pointer).and_then(mux_value_timestamp))
}

pub(crate) fn mux_value_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    match value {
        Value::String(raw) => parse_rfc3339_utc(raw).or_else(|| {
            raw.parse::<f64>()
                .ok()
                .and_then(provider_timestamp_seconds_to_datetime)
        }),
        Value::Number(number) => number
            .as_f64()
            .and_then(provider_timestamp_seconds_to_datetime),
        _ => None,
    }
}

pub(crate) fn read_mux_metadata(path: Option<&Path>, summary: &mut ProviderImportSummary) -> Value {
    let Some(path) = path else {
        return Value::Null;
    };
    match read_text_file_limited(path, MAX_PROVIDER_JSONL_LINE_BYTES, "Mux metadata.json") {
        Ok(raw) => match serde_json::from_str::<Value>(&raw) {
            Ok(value) if value.is_object() => value,
            Ok(_) => {
                push_provider_import_failure(
                    summary,
                    0,
                    "Mux metadata.json must contain a JSON object".to_owned(),
                );
                Value::Null
            }
            Err(err) => {
                push_provider_import_failure(
                    summary,
                    0,
                    format!("invalid Mux metadata.json: {err}"),
                );
                Value::Null
            }
        },
        Err(err) => {
            push_provider_import_failure(
                summary,
                0,
                format!("could not read Mux metadata.json: {err}"),
            );
            Value::Null
        }
    }
}

pub(crate) fn mux_string_pointer(value: &Value, pointers: &[&str]) -> Option<String> {
    pointers.iter().find_map(|pointer| {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .filter(|raw| !raw.trim().is_empty())
            .map(str::to_owned)
    })
}

pub(crate) fn mux_source_primary_path(source: &MuxSessionSource) -> &Path {
    source
        .chat_path
        .as_deref()
        .or(source.partial_path.as_deref())
        .unwrap_or(source.session_dir.as_path())
}
