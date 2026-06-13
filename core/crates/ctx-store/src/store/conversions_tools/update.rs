pub(super) fn normalize_tool_status(status: &str, event_type: SessionEventType) -> String {
    let s = status.trim().to_lowercase();
    if s == "inprogress" || s == "in_progress" || s == "running" {
        return "in_progress".to_string();
    }
    if s == "pending" || s == "queued" {
        return "pending".to_string();
    }
    if s == "completed" || s == "complete" || s == "ok" || s == "succeeded" {
        return "completed".to_string();
    }
    if s == "failed" || s == "error" {
        return "failed".to_string();
    }
    if matches!(event_type, SessionEventType::ToolResult) {
        return "completed".to_string();
    }
    if s.is_empty() {
        return "pending".to_string();
    }
    s
}

pub(super) fn merge_tool_status(existing: Option<&str>, next: &str) -> String {
    match (existing, next) {
        (Some(current @ ("completed" | "failed")), "pending" | "in_progress") => {
            current.to_string()
        }
        _ => next.to_string(),
    }
}

pub(super) fn extract_tool_update(payload: &Value) -> &Value {
    payload
}

fn tool_kind_from_update(update: &Value) -> Option<String> {
    update
        .get("kind")
        .and_then(Value::as_str)
        .or_else(|| update.pointer("/toolCall/kind").and_then(Value::as_str))
        .map(str::to_owned)
}

fn string_from_value(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_owned)
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

fn preview_string(input: &Value, key: &str) -> Option<String> {
    input.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn preview_command(input: &Value) -> Option<String> {
    match input.get("command") {
        Some(Value::String(value)) => {
            Some(value.trim().to_owned()).filter(|value| !value.is_empty())
        }
        Some(Value::Array(parts)) => {
            let joined = parts
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            Some(joined).filter(|value| !value.is_empty())
        }
        _ => None,
    }
}

fn preview_path_summary(input: &Value) -> Option<String> {
    let direct = [
        "path",
        "file",
        "filename",
        "file_path",
        "filePath",
        "filepath",
        "target",
    ]
    .into_iter()
    .find_map(|key| preview_string(input, key));
    if direct.is_some() {
        return direct;
    }
    for key in ["paths", "files", "file_paths", "filePaths"] {
        if let Some(Value::Array(values)) = input.get(key) {
            let paths = values
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>();
            if let Some(first) = paths.first() {
                let more = paths.len().saturating_sub(1);
                return Some(if more > 0 {
                    format!("{first} +{more} more")
                } else {
                    (*first).to_owned()
                });
            }
        }
    }
    None
}

fn format_diff_stats(input: &Value) -> Option<String> {
    let stats = input.get("diff_stats")?.as_object()?;
    let added = stats.get("added").and_then(Value::as_i64).unwrap_or(0);
    let removed = stats.get("removed").and_then(Value::as_i64).unwrap_or(0);
    let files = stats.get("files").and_then(Value::as_i64).unwrap_or(0);
    let mut parts = Vec::new();
    if added > 0 {
        parts.push(format!("+{added}"));
    }
    if removed > 0 {
        parts.push(format!("-{removed}"));
    }
    if parts.is_empty() && files > 0 {
        parts.push(format!("{files} files"));
    }
    if parts.is_empty() {
        None
    } else {
        Some(format!("({})", parts.join(" ")))
    }
}

fn tool_subtitle_from_preview(
    tool_kind: Option<&str>,
    provider_tool_name: Option<&str>,
    input: Option<&Value>,
) -> Option<String> {
    let input = input?;
    input.as_object()?;
    if let Some(description) = preview_string(input, "description") {
        return Some(description);
    }
    let kind_hint = tool_kind
        .or(provider_tool_name)
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let path = preview_path_summary(input);
    let query = preview_string(input, "query")
        .or_else(|| preview_string(input, "pattern"))
        .or_else(|| preview_string(input, "regex"))
        .or_else(|| preview_string(input, "text"));
    let command = preview_command(input);
    let glob = preview_string(input, "glob").or_else(|| preview_string(input, "pattern"));
    let url = preview_string(input, "url")
        .or_else(|| preview_string(input, "uri"))
        .or_else(|| preview_string(input, "href"));

    let combine_query_path = |q: String, path: Option<String>| match path {
        Some(path) => format!("{q} in {path}"),
        None => q,
    };
    let combine_path_stats = |path: Option<String>, stats: Option<String>| match (path, stats) {
        (Some(path), Some(stats)) => Some(format!("{path} {stats}")),
        (Some(path), None) => Some(path),
        (None, Some(stats)) => Some(stats),
        (None, None) => None,
    };

    match kind_hint.as_str() {
        "execute" | "exec" | "shell" | "bash" => command,
        "search" | "web_search" | "grep" => {
            query.map(|q| combine_query_path(q, path.clone())).or(path)
        }
        "glob" => glob.or(path),
        "list" | "list_files" => path,
        "read" | "read_file" => path,
        "edit" | "write" | "apply_patch" | "patch" => {
            combine_path_stats(path, format_diff_stats(input))
        }
        "fetch" | "http" | "curl" => {
            let method = preview_string(input, "method").unwrap_or_else(|| "GET".to_owned());
            url.map(|url| format!("{} {}", method.trim().to_uppercase(), url))
                .or(Some(method.trim().to_uppercase()))
        }
        _ => path.or(query).or(command).or(glob).or(url),
    }
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

fn tool_status_from_update(update: &Value) -> Option<&str> {
    update
        .get("status")
        .and_then(Value::as_str)
        .or_else(|| update.pointer("/toolCall/status").and_then(Value::as_str))
}

pub(super) fn sanitize_tool_event_payload(
    event_type: &SessionEventType,
    raw_payload: &Value,
) -> Value {
    let update = extract_tool_update(raw_payload);
    let tool_call_id = tool_call_id_from_payload(raw_payload).unwrap_or_default();

    let tool_kind = tool_kind_from_update(update);
    let tool_label = tool_label_from_update(update);
    let tool_name = tool_name_from_update(update);
    let title = tool_title_from_update(update);
    let raw_status = tool_status_from_update(update);
    let status = if let Some(raw_status) = raw_status {
        normalize_tool_status(raw_status, event_type.clone())
    } else if matches!(event_type, SessionEventType::ToolResult) {
        "completed".to_string()
    } else {
        "pending".to_string()
    };

    let input = update
        .pointer("/rawInput")
        .or_else(|| update.pointer("/toolCall/rawInput"))
        .or_else(|| update.pointer("/toolCall/input"))
        .or_else(|| update.pointer("/input"))
        .or_else(|| update.pointer("/args"));
    let input_preview = tool_input_preview_from_value(input);
    let input_preview = input_preview.or_else(|| update.get("input_preview").cloned());
    let input_meta = build_json_preview(input, input_preview);
    let subtitle = tool_subtitle_from_preview(
        tool_kind.as_deref(),
        tool_name.as_deref(),
        input_meta.preview.as_ref(),
    );

    let output_meta = extract_tool_output_text(update)
        .map(|output| build_output_preview(&output))
        .filter(|preview| !preview.preview.trim().is_empty());
    let output_artifact = update
        .get("output_artifact")
        .filter(|value| value.is_object())
        .cloned()
        .or_else(|| {
            raw_payload
                .get("output_artifact")
                .filter(|value| value.is_object())
                .cloned()
        });

    let mut obj = serde_json::Map::new();
    if !tool_call_id.trim().is_empty() {
        obj.insert("tool_call_id".to_string(), Value::String(tool_call_id));
    }
    if let Some(v) = tool_kind {
        obj.insert("kind".to_string(), Value::String(v));
    }
    if let Some(v) = tool_label {
        obj.insert("tool_label".to_string(), Value::String(v));
    }
    if let Some(v) = tool_name {
        obj.insert("tool_name".to_string(), Value::String(v));
    }
    if let Some(v) = title {
        obj.insert("title".to_string(), Value::String(v));
    }
    if let Some(v) = subtitle {
        obj.insert("subtitle".to_string(), Value::String(v));
    }
    obj.insert("status".to_string(), Value::String(status));

    if let Some(v) = input_meta.preview {
        obj.insert("input_preview".to_string(), v);
    }
    let input_truncated = update
        .get("input_truncated")
        .and_then(|v| v.as_bool())
        .or(input_meta.truncated);
    let input_original_bytes = update
        .get("input_original_bytes")
        .and_then(|v| v.as_i64())
        .or(input_meta.original_bytes);
    if let Some(truncated) = input_truncated {
        obj.insert("input_truncated".to_string(), Value::Bool(truncated));
    }
    if let Some(bytes) = input_original_bytes {
        obj.insert(
            "input_original_bytes".to_string(),
            Value::Number(serde_json::Number::from(bytes)),
        );
    }

    if let Some(output_meta) = output_meta {
        obj.insert(
            "output_preview".to_string(),
            Value::String(output_meta.preview),
        );
        let output_truncated = update
            .get("output_truncated")
            .and_then(|v| v.as_bool())
            .unwrap_or(output_meta.truncated);
        let output_original_bytes = update
            .get("output_original_bytes")
            .and_then(|v| v.as_i64())
            .unwrap_or(output_meta.original_bytes as i64);
        obj.insert(
            "output_truncated".to_string(),
            Value::Bool(output_truncated),
        );
        obj.insert(
            "output_original_bytes".to_string(),
            Value::Number(serde_json::Number::from(output_original_bytes)),
        );
    }
    if let Some(artifact) = output_artifact {
        obj.insert("output_artifact".to_string(), artifact);
    }

    if let Some(value) = raw_payload
        .get("crp_seq")
        .or_else(|| raw_payload.get("crpSeq"))
        .or_else(|| update.get("crp_seq"))
        .or_else(|| update.get("crpSeq"))
    {
        obj.insert("crp_seq".to_string(), value.clone());
    }
    if let Some(value) = raw_payload
        .get("order_seq")
        .or_else(|| raw_payload.get("orderSeq"))
        .or_else(|| update.get("order_seq"))
        .or_else(|| update.get("orderSeq"))
    {
        obj.insert("order_seq".to_string(), value.clone());
    }
    if let Some(value) = raw_payload
        .get("crp_channel")
        .or_else(|| raw_payload.get("crpChannel"))
        .or_else(|| update.get("crp_channel"))
        .or_else(|| update.get("crpChannel"))
    {
        obj.insert("crp_channel".to_string(), value.clone());
    }

    Value::Object(obj)
}
