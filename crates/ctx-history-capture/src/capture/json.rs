#[allow(unused_imports)]
use super::*;

pub(crate) const CONTINUE_CLI_SOURCE_FORMAT: &str = "continue_cli_sessions_json";

pub(crate) fn collect_structured_file_touch_object(
    object: &serde_json::Map<String, Value>,
    out: &mut Vec<FileTouchDraft>,
    inherited_kind: Option<FileChangeKind>,
) {
    let inferred_kind = inferred_file_change_kind(object);
    let change_kind = inherited_kind.unwrap_or(inferred_kind);
    let old_path = object.iter().find_map(|(key, value)| {
        is_old_file_path_key(key)
            .then(|| value.as_str())
            .flatten()
            .and_then(normalize_file_path)
    });
    for (key, value) in object {
        if !is_file_path_key(key) {
            continue;
        }
        let Some(raw_path) = value.as_str() else {
            continue;
        };
        if normalized_key(key) == "uri" && !raw_path.trim().starts_with("file://") {
            continue;
        }
        let Some(path) = normalize_file_path(raw_path) else {
            continue;
        };
        out.push(FileTouchDraft {
            path,
            old_path: old_path.clone(),
            change_kind: Some(change_kind),
            confidence: Confidence::High,
            metadata: json!({
                "source": "structured_provider_payload",
                "path_key": key,
            }),
        });
    }
}

pub(crate) fn object_operation_hint_kind(
    object: &serde_json::Map<String, Value>,
) -> Option<FileChangeKind> {
    object
        .iter()
        .any(|(key, value)| {
            matches!(
                normalized_key(key).as_str(),
                "tool" | "name" | "action" | "command" | "operation" | "type"
            ) && value.as_str().is_some_and(|text| !text.trim().is_empty())
        })
        .then(|| inferred_file_change_kind(object))
        .filter(|kind| *kind != FileChangeKind::Unknown)
}

pub(crate) fn inferred_file_change_kind(object: &serde_json::Map<String, Value>) -> FileChangeKind {
    let mut haystack = String::new();
    for (key, value) in object {
        haystack.push_str(&key.to_ascii_lowercase());
        haystack.push(' ');
        if matches!(
            key.to_ascii_lowercase().as_str(),
            "tool" | "name" | "action" | "command" | "operation" | "type"
        ) {
            if let Some(text) = value.as_str() {
                haystack.push_str(&text.to_ascii_lowercase());
                haystack.push(' ');
            }
        }
    }
    if haystack.contains("rename") || haystack.contains("move") {
        FileChangeKind::Renamed
    } else if haystack.contains("delete") || haystack.contains("remove") {
        FileChangeKind::Deleted
    } else if haystack.contains("create") || haystack.contains("write") || haystack.contains("add")
    {
        FileChangeKind::Created
    } else if haystack.contains("read") || haystack.contains("view") || haystack.contains("open") {
        FileChangeKind::Read
    } else if object.values().any(value_looks_like_file_content)
        || haystack.contains("edit")
        || haystack.contains("patch")
        || haystack.contains("replace")
        || haystack.contains("update")
    {
        FileChangeKind::Modified
    } else {
        FileChangeKind::Unknown
    }
}

pub(crate) fn task_json_entry_type(value: &Value, source: &str) -> String {
    task_json_string_field(value, &["type", "say", "ask", "role"])
        .unwrap_or_else(|| source.to_owned())
}

pub(crate) fn task_json_content_has(value: &Value, expected: &str) -> bool {
    value
        .get("content")
        .or_else(|| value.pointer("/message/content"))
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some(expected))
        })
        .unwrap_or(false)
}

pub(crate) fn task_json_model(value: &Value) -> Option<Value> {
    value
        .get("model")
        .or_else(|| value.pointer("/modelInfo/id"))
        .or_else(|| value.pointer("/metadata/model"))
        .cloned()
}

pub(crate) fn task_json_usage(value: &Value) -> Option<Value> {
    value
        .get("usage")
        .or_else(|| value.get("tokensUsed"))
        .or_else(|| value.pointer("/modelInfo/usage"))
        .cloned()
}

pub(crate) fn task_json_string_field(value: &Value, fields: &[&str]) -> Option<String> {
    fields
        .iter()
        .find_map(|field| value.get(*field).and_then(Value::as_str))
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
}

pub(crate) fn provider_capped_json(value: &Value, max_chars: usize) -> Value {
    match value {
        Value::Null => Value::Null,
        Value::String(text) => {
            let (text, truncated) = provider_local_preview(text, max_chars);
            json!({ "text": text, "truncated": truncated })
        }
        _ => {
            let rendered = serde_json::to_string(value).unwrap_or_else(|_| value.to_string());
            let (json_text, truncated) = provider_local_preview(&rendered, max_chars);
            json!({ "json": json_text, "truncated": truncated })
        }
    }
}

pub(crate) fn provider_capped_json_value(value: &Value, max_string_chars: usize) -> Value {
    match value {
        Value::String(text) => {
            let (text, truncated) = provider_local_preview(text, max_string_chars);
            if truncated {
                json!({ "text": text, "truncated": true })
            } else {
                Value::String(text)
            }
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| provider_capped_json_value(item, max_string_chars))
                .collect(),
        ),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        provider_capped_json_value(value, max_string_chars),
                    )
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}

pub(crate) fn provider_json_text(raw: &str) -> Value {
    serde_json::from_str::<Value>(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
}

pub(crate) fn parse_json_object_string(value: Option<&str>) -> Value {
    value
        .and_then(|value| serde_json::from_str::<Value>(value).ok())
        .unwrap_or(Value::Null)
}

pub(crate) fn push_json_text(parts: &mut Vec<String>, value: &Value) {
    if let Some(text) = provider_value_text(value).filter(|text| !text.trim().is_empty()) {
        parts.push(text);
    }
}

pub(crate) fn provider_json_without_keys(value: &Value, keys: &[&str]) -> Value {
    let Value::Object(object) = value else {
        return value.clone();
    };
    let mut object = object.clone();
    for key in keys {
        object.remove(*key);
    }
    Value::Object(object)
}

pub fn compute_payload_hash(payload: &Value) -> Result<String> {
    let bytes = serde_json::to_vec(payload)?;
    Ok(format!("fnv1a64:{:016x}", fnv1a64(&bytes)))
}

pub(crate) fn default_metadata() -> Value {
    json!({})
}
