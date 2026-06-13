use serde_json::Value;

use ctx_core::models::SessionEventType;

use super::preview::{
    build_json_preview, build_output_preview, extract_patch_text_owned, extract_tool_input,
    is_edit_tool, tool_input_preview, tool_subtitle_from_preview, ToolJsonPreview, ToolTextPreview,
};

#[derive(Clone, Copy, Debug)]
struct InferredToolIdentity {
    kind: &'static str,
    name: &'static str,
}

#[derive(Debug, Clone)]
pub struct NormalizedToolEvent {
    pub tool_call_id: Option<String>,
    pub raw_tool_kind: Option<String>,
    pub tool_label: Option<String>,
    pub raw_title: Option<String>,
    pub status: String,
    pub input_preview: Option<Value>,
    pub input_meta: ToolJsonPreview,
    pub output_preview: Option<ToolTextPreview>,
    pub raw_output_text: Option<String>,
    pub tool_kind: Option<String>,
    pub provider_tool_name: Option<String>,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub cwd: Option<String>,
    pub crp_seq: Option<Value>,
    pub crp_channel: Option<Value>,
    pub raw_order_seq: Option<Value>,
}

pub fn normalize_tool_event(
    event_type: &SessionEventType,
    raw_payload: &Value,
) -> NormalizedToolEvent {
    let update = extract_tool_update(raw_payload);
    let tool_call_id = tool_call_id_from_payload(raw_payload);
    let raw_tool_kind = tool_kind_from_update(update);
    let raw_provider_tool_name = tool_name_from_update(update);
    let tool_label = tool_label_from_update(update);
    let raw_title = tool_title_from_update(update);
    let status = raw_status_from_update(update)
        .map(|status| normalize_tool_status(status, event_type))
        .unwrap_or_else(|| default_tool_status(event_type));

    let input = extract_tool_input(update);
    let input_preview = direct_input_preview(update).or_else(|| {
        tool_input_preview(
            input,
            update,
            raw_tool_kind.as_deref(),
            raw_title.as_deref(),
        )
    });
    let input_meta = build_json_preview(input, input_preview.clone());
    let raw_output_text = extract_tool_output_raw_text(update);
    let patch_preview = if is_edit_tool(raw_tool_kind.as_deref(), raw_title.as_deref()) {
        extract_patch_text_owned(input, update).map(|patch| build_output_preview(&patch))
    } else {
        None
    };
    let output_preview = extract_tool_output_text(update)
        .as_deref()
        .map(build_output_preview)
        .or(patch_preview)
        .filter(|preview| !preview.preview.trim().is_empty());
    let inferred = inferred_tool_identity(
        tool_call_id.as_deref().unwrap_or_default(),
        input_meta.preview.as_ref(),
        output_preview
            .as_ref()
            .map(|preview| preview.preview.as_str()),
    );
    let tool_kind = non_placeholder_tool_field(raw_tool_kind.clone())
        .or_else(|| inferred.map(|identity| identity.kind.to_string()));
    let provider_tool_name = non_placeholder_tool_field(raw_provider_tool_name.clone())
        .or_else(|| inferred.map(|identity| identity.name.to_string()));
    let title = non_placeholder_tool_field(raw_title.clone())
        .or_else(|| tool_label.clone())
        .or_else(|| inferred.map(|identity| identity.name.to_string()));
    let subtitle = tool_subtitle_from_preview(
        tool_kind.as_deref(),
        provider_tool_name.as_deref(),
        input_meta.preview.as_ref(),
    );
    let cwd = input_preview
        .as_ref()
        .and_then(|preview| preview.get("cwd"))
        .and_then(Value::as_str)
        .map(str::to_owned);

    NormalizedToolEvent {
        tool_call_id,
        raw_tool_kind,
        tool_label,
        raw_title,
        status,
        input_preview,
        input_meta,
        output_preview,
        raw_output_text,
        tool_kind,
        provider_tool_name,
        title,
        subtitle,
        cwd,
        crp_seq: raw_payload
            .get("crp_seq")
            .or_else(|| update.get("crp_seq"))
            .cloned(),
        crp_channel: raw_payload
            .get("crp_channel")
            .or_else(|| update.get("crp_channel"))
            .cloned(),
        raw_order_seq: raw_payload
            .get("order_seq")
            .or_else(|| raw_payload.get("orderSeq"))
            .or_else(|| update.get("order_seq"))
            .or_else(|| update.get("orderSeq"))
            .cloned(),
    }
}

fn string_from_value(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_owned)
}

fn normalize_placeholder_tool_label(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(c, ' ' | '.' | '_' | '-'))
        .collect()
}

fn is_placeholder_tool_label(value: Option<&str>) -> bool {
    let normalized = normalize_placeholder_tool_label(value.unwrap_or_default());
    normalized.is_empty()
        || normalized == "unknown"
        || normalized == "tool"
        || normalized == "unknowntool"
}

fn non_placeholder_tool_field(raw: Option<String>) -> Option<String> {
    raw.filter(|value| !is_placeholder_tool_label(Some(value.as_str())))
}

fn direct_input_preview(update: &Value) -> Option<Value> {
    update.get("input_preview").and_then(|value| {
        if value.is_null() {
            None
        } else {
            Some(value.clone())
        }
    })
}

fn inferred_tool_identity(
    tool_call_id: &str,
    input_preview: Option<&Value>,
    output_preview: Option<&str>,
) -> Option<InferredToolIdentity> {
    let input = input_preview.and_then(Value::as_object);
    if let Some(input) = input {
        if input.contains_key("command") || input.contains_key("parsed_cmd") {
            return Some(InferredToolIdentity {
                kind: "execute",
                name: "Bash",
            });
        }
        if input.contains_key("query")
            || input.contains_key("pattern")
            || input.contains_key("regex")
        {
            return Some(InferredToolIdentity {
                kind: "search",
                name: "Grep",
            });
        }
        if input.contains_key("file_path")
            || input.contains_key("filePath")
            || input.contains_key("filepath")
            || input.contains_key("filename")
            || input.contains_key("file")
            || input.contains_key("path")
        {
            let is_edit = input.contains_key("changes")
                || input.contains_key("edits")
                || input.contains_key("new_string")
                || input.contains_key("newText")
                || input.contains_key("replacement")
                || input.contains_key("diff_stats");
            return Some(if is_edit {
                InferredToolIdentity {
                    kind: "edit",
                    name: "Edit",
                }
            } else {
                InferredToolIdentity {
                    kind: "read",
                    name: "Read",
                }
            });
        }
    }

    let output = output_preview
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let read_like = output.lines().take(3).any(|line| {
        let trimmed = line.trim_start();
        let mut chars = trimmed.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        first.is_ascii_digit() && chars.as_str().starts_with('→')
    });
    if read_like {
        return Some(InferredToolIdentity {
            kind: "read",
            name: "Read",
        });
    }

    if tool_call_id.starts_with("toolu_") {
        return Some(InferredToolIdentity {
            kind: "execute",
            name: "Bash",
        });
    }

    None
}

fn tool_label_from_update(update: &Value) -> Option<String> {
    string_from_value(update.get("tool_label"))
        .or_else(|| string_from_value(update.get("toolLabel")))
        .or_else(|| string_from_value(update.pointer("/toolCall/tool_label")))
        .or_else(|| string_from_value(update.pointer("/toolCall/toolLabel")))
}

fn tool_name_from_update(update: &Value) -> Option<String> {
    string_from_value(update.get("tool_name"))
        .or_else(|| string_from_value(update.get("toolName")))
        .or_else(|| string_from_value(update.get("name")))
        .or_else(|| string_from_value(update.pointer("/toolCall/name")))
}

fn tool_kind_from_update(update: &Value) -> Option<String> {
    string_from_value(update.get("kind"))
        .or_else(|| string_from_value(update.pointer("/toolCall/kind")))
}

fn tool_title_from_update(update: &Value) -> Option<String> {
    string_from_value(update.get("title"))
        .or_else(|| string_from_value(update.get("tool_label")))
        .or_else(|| string_from_value(update.get("toolLabel")))
        .or_else(|| string_from_value(update.pointer("/toolCall/title")))
        .or_else(|| string_from_value(update.pointer("/toolCall/tool_label")))
        .or_else(|| string_from_value(update.pointer("/toolCall/toolLabel")))
        .or_else(|| tool_name_from_update(update))
}

fn raw_status_from_update(update: &Value) -> Option<&str> {
    update
        .get("status")
        .and_then(Value::as_str)
        .or_else(|| update.pointer("/toolCall/status").and_then(Value::as_str))
}

fn default_tool_status(event_type: &SessionEventType) -> String {
    if matches!(event_type, SessionEventType::ToolResult) {
        "completed".to_string()
    } else {
        "pending".to_string()
    }
}

pub(super) fn normalize_tool_status(status: &str, event_type: &SessionEventType) -> String {
    let normalized = status.trim().to_lowercase();
    if matches!(
        normalized.as_str(),
        "inprogress" | "in_progress" | "running"
    ) {
        return "in_progress".to_string();
    }
    if matches!(normalized.as_str(), "pending" | "queued") {
        return "pending".to_string();
    }
    if matches!(
        normalized.as_str(),
        "completed" | "complete" | "ok" | "succeeded"
    ) {
        return "completed".to_string();
    }
    if matches!(normalized.as_str(), "failed" | "error") {
        return "failed".to_string();
    }
    if matches!(event_type, SessionEventType::ToolResult) {
        return "completed".to_string();
    }
    if normalized.is_empty() {
        return "pending".to_string();
    }
    normalized
}

fn extract_tool_update(payload: &Value) -> &Value {
    payload
}

pub(super) fn tool_call_id_from_payload(payload: &Value) -> Option<String> {
    if let Some(value) = payload.get("tool_call_id").and_then(Value::as_str) {
        return Some(value.to_string());
    }
    let update = extract_tool_update(payload);
    let direct = update
        .get("toolCallId")
        .and_then(Value::as_str)
        .or_else(|| update.get("tool_call_id").and_then(Value::as_str));
    if let Some(value) = direct {
        return Some(value.to_string());
    }
    update
        .pointer("/rawInput/call_id")
        .and_then(Value::as_str)
        .or_else(|| update.pointer("/raw_input/call_id").and_then(Value::as_str))
        .map(str::to_owned)
}

fn extract_tool_output_raw_text(update: &Value) -> Option<String> {
    let direct = update
        .get("outputText")
        .and_then(Value::as_str)
        .or_else(|| update.get("output_text").and_then(Value::as_str))
        .or_else(|| {
            update
                .pointer("/toolCall/outputText")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            update
                .pointer("/toolCall/output_text")
                .and_then(Value::as_str)
        })
        .or_else(|| update.get("result").and_then(Value::as_str))
        .or_else(|| {
            update
                .pointer("/rawOutput/aggregated_output")
                .and_then(Value::as_str)
        })
        .or_else(|| update.pointer("/rawOutput/output").and_then(Value::as_str));
    if let Some(value) = direct {
        if !value.trim().is_empty() {
            return Some(value.to_string());
        }
    }

    let blocks = update.get("content").and_then(Value::as_array)?;
    let mut output = String::new();
    for block in blocks {
        if let Some(text) = block
            .get("content")
            .and_then(|content| content.get("text"))
            .and_then(Value::as_str)
        {
            output.push_str(text);
        } else if let Some(text) = block.get("text").and_then(Value::as_str) {
            output.push_str(text);
        }
    }
    if output.trim().is_empty() {
        None
    } else {
        Some(output)
    }
}

fn extract_tool_output_text(update: &Value) -> Option<String> {
    let direct = update
        .get("outputText")
        .and_then(Value::as_str)
        .or_else(|| update.get("output_text").and_then(Value::as_str))
        .or_else(|| update.get("output_preview").and_then(Value::as_str))
        .or_else(|| {
            update
                .pointer("/toolCall/outputText")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            update
                .pointer("/toolCall/output_text")
                .and_then(Value::as_str)
        })
        .or_else(|| update.get("result").and_then(Value::as_str))
        .or_else(|| {
            update
                .pointer("/rawOutput/aggregated_output")
                .and_then(Value::as_str)
        })
        .or_else(|| update.pointer("/rawOutput/output").and_then(Value::as_str));
    if let Some(value) = direct {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    let blocks = update.get("content").and_then(Value::as_array)?;
    let mut output = String::new();
    for block in blocks {
        if let Some(text) = block
            .get("content")
            .and_then(|content| content.get("text"))
            .and_then(Value::as_str)
        {
            output.push_str(text);
        } else if let Some(text) = block.get("text").and_then(Value::as_str) {
            output.push_str(text);
        }
    }
    if output.trim().is_empty() {
        None
    } else {
        Some(output.trim().to_string())
    }
}
