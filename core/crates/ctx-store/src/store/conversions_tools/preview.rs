pub(super) const TOOL_PREVIEW_MAX_LINES: usize = 5;
pub(super) const TOOL_PREVIEW_MAX_LINE_CHARS: usize = 80;

pub(super) struct ToolTextPreview {
    preview: String,
    truncated: bool,
    original_bytes: usize,
}

pub(super) struct ToolJsonPreview {
    preview: Option<Value>,
    truncated: Option<bool>,
    original_bytes: Option<i64>,
}

pub(super) fn tool_input_preview_from_value(input: Option<&Value>) -> Option<Value> {
    let input = input?;
    let obj = input.as_object()?;
    let mut out = serde_json::Map::new();
    for key in [
        "command",
        "query",
        "pattern",
        "text",
        "path",
        "file",
        "filename",
        "file_path",
        "filePath",
        "filepath",
        "paths",
        "paths_total",
        "files",
        "file_paths",
        "filePaths",
        "target",
        "glob",
        "parsed_cmd",
        "cwd",
        "root",
        "url",
        "uri",
        "href",
        "method",
        "regex",
        "diff_stats",
        "description",
    ] {
        if let Some(value) = obj.get(key) {
            if value.is_string() || value.is_number() || value.is_array() || value.is_object() {
                out.insert(key.to_string(), value.clone());
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        let value = Value::Object(out);
        let mut truncated = false;
        Some(truncate_preview_value(&value, &mut truncated))
    }
}

pub(super) fn truncate_preview_value(value: &Value, truncated: &mut bool) -> Value {
    match value {
        Value::String(value) => {
            let preview = build_text_preview(value);
            if preview.truncated {
                *truncated = true;
            }
            Value::String(preview.preview)
        }
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|value| truncate_preview_value(value, truncated))
                .collect(),
        ),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, value) in map {
                out.insert(key.clone(), truncate_preview_value(value, truncated));
            }
            Value::Object(out)
        }
        _ => value.clone(),
    }
}

pub(super) fn push_text_preview_line(out: &mut Vec<String>, truncated: &mut bool, line: &str) {
    if line.chars().count() > TOOL_PREVIEW_MAX_LINE_CHARS {
        *truncated = true;
        out.push(line.chars().take(TOOL_PREVIEW_MAX_LINE_CHARS).collect());
    } else {
        out.push(line.to_string());
    }
}

pub(super) fn build_text_preview(text: &str) -> ToolTextPreview {
    let original_bytes = text.len();
    let mut truncated = false;
    let lines: Vec<&str> = text.lines().collect();
    let total_lines = lines.len();

    let mut out = Vec::new();

    if total_lines <= TOOL_PREVIEW_MAX_LINES {
        for line in lines {
            push_text_preview_line(&mut out, &mut truncated, line);
        }
    } else {
        truncated = true;
        let head_count = TOOL_PREVIEW_MAX_LINES / 2;
        let tail_count = TOOL_PREVIEW_MAX_LINES.saturating_sub(head_count + 1);
        for line in lines.iter().take(head_count) {
            push_text_preview_line(&mut out, &mut truncated, line);
        }
        let omitted = total_lines.saturating_sub(head_count + tail_count);
        out.push(format!("... +{omitted} lines"));
        for line in lines.iter().skip(total_lines - tail_count) {
            push_text_preview_line(&mut out, &mut truncated, line);
        }
    }

    ToolTextPreview {
        preview: out.join("\n"),
        truncated,
        original_bytes,
    }
}

pub(super) fn build_json_preview(input: Option<&Value>, preview: Option<Value>) -> ToolJsonPreview {
    let original_bytes = input
        .and_then(|value| serde_json::to_string(value).ok())
        .map(|value| value.len() as i64);
    let mut preview_truncated = false;
    let preview = preview.map(|value| truncate_preview_value(&value, &mut preview_truncated));
    let preview_bytes = preview
        .as_ref()
        .and_then(|value| serde_json::to_string(value).ok())
        .map(|value| value.len() as i64);
    let mut truncated = preview_truncated;
    if let (Some(original), Some(preview_bytes)) = (original_bytes, preview_bytes) {
        if original > preview_bytes {
            truncated = true;
        }
    } else if original_bytes.is_some() && preview.is_none() {
        truncated = true;
    }
    let truncated = if original_bytes.is_some() || preview.is_some() {
        Some(truncated)
    } else {
        None
    };
    ToolJsonPreview {
        preview,
        truncated,
        original_bytes,
    }
}

pub(super) fn build_output_preview(text: &str) -> ToolTextPreview {
    build_text_preview(text)
}
