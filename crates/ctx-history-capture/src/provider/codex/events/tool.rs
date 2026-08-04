use std::borrow::Cow;

use serde_json::{Map, Value};

#[cfg(test)]
use super::retention::{codex_exit_code, codex_wall_time_ms};
#[cfg(test)]
use crate::provider::normalization::provider_output_event_is_failure;
#[cfg(test)]
use crate::{OutputOutcome, OutputOutcomeMetadata};

#[derive(Debug, Clone, Default)]
pub(crate) struct CodexToolCallContext {
    pub(crate) tool_name: String,
    pub(crate) command_preview: Option<String>,
    pub(crate) arguments_preview: Option<String>,
    pub(crate) exact_command: Option<String>,
    pub(crate) command_too_large: bool,
    pub(crate) session_cwd: Option<String>,
    pub(crate) declared_workdir: Option<String>,
    pub(crate) continuation_cell_id: Option<String>,
    pub(crate) origin_call_id: Option<String>,
    pub(crate) origin_event_sequence: Option<u64>,
    pub(crate) origin_occurred_at_unix_ms: Option<i64>,
    pub(crate) continuation_call_id_sha256: Vec<[u8; 32]>,
    pub(crate) continuation_capacity_exceeded: bool,
    pub(crate) correlation_ambiguous: bool,
}

#[cfg(test)]
pub(crate) fn codex_tool_output_outcome(payload: &Value) -> OutputOutcomeMetadata {
    let timed_out = codex_output_contains_timeout(payload);
    let exit_code = codex_output_exit_code(payload);
    let structured_failure = provider_output_event_is_failure(payload);
    let outcome = if timed_out {
        OutputOutcome::Timeout
    } else if exit_code.is_some_and(|code| code != 0) || structured_failure {
        OutputOutcome::Failure
    } else if exit_code == Some(0) || codex_output_indicates_success(payload) {
        OutputOutcome::Success
    } else {
        OutputOutcome::Unknown
    };
    OutputOutcomeMetadata {
        outcome,
        exit_code,
        duration_ms: codex_output_duration_ms(payload),
    }
}

#[cfg(test)]
fn codex_output_contains_timeout(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            object
                .get("timed_out")
                .or_else(|| object.get("timedOut"))
                .or_else(|| object.get("timeout"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || object.values().any(codex_output_contains_timeout)
        }
        Value::Array(items) => items.iter().any(codex_output_contains_timeout),
        Value::String(text) => {
            text.contains("timed out")
                || text.contains("Timed out")
                || text.contains("TIMED OUT")
                || text.contains("timed_out=true")
        }
        _ => false,
    }
}

#[cfg(test)]
fn codex_output_duration_ms(value: &Value) -> Option<u64> {
    match value {
        Value::Object(object) => {
            for key in ["duration_ms", "durationMs"] {
                if let Some(duration) = object.get(key).and_then(Value::as_u64) {
                    return Some(duration);
                }
            }
            object.values().find_map(codex_output_duration_ms)
        }
        Value::Array(items) => items.iter().find_map(codex_output_duration_ms),
        Value::String(text) => codex_wall_time_ms(text).and_then(|value| u64::try_from(value).ok()),
        _ => None,
    }
}

#[cfg(test)]
fn codex_output_indicates_success(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            object
                .get("success")
                .or_else(|| object.get("ok"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || ["status", "state", "outcome"].iter().any(|key| {
                    object
                        .get(*key)
                        .and_then(Value::as_str)
                        .is_some_and(|status| {
                            matches!(
                                status.trim().to_ascii_lowercase().as_str(),
                                "success"
                                    | "succeeded"
                                    | "complete"
                                    | "completed"
                                    | "ok"
                                    | "passed"
                            )
                        })
                })
                || object.values().any(codex_output_indicates_success)
        }
        Value::Array(items) => items.iter().any(codex_output_indicates_success),
        Value::String(text) => text.starts_with("Script completed\n") || text == "Script completed",
        _ => false,
    }
}

pub(crate) fn codex_result_content(payload: &Value) -> Option<Cow<'_, str>> {
    codex_result_value(payload).map(|result| codex_output_content(result).text)
}

pub(crate) fn codex_result_value(payload: &Value) -> Option<&Value> {
    let item_type = payload.get("type").and_then(Value::as_str)?;
    if !matches!(
        item_type,
        "function_call_output"
            | "custom_tool_call_output"
            | "tool_search_output"
            | "tool_result"
            | "tool_output"
    ) {
        return None;
    }
    payload
        .get("output")
        .or_else(|| payload.get("tools"))
        .or_else(|| payload.get("result"))
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CodexOutputContent<'a> {
    pub(crate) text: Cow<'a, str>,
    pub(crate) metadata: Option<Value>,
}

/// Separates complete result content from its structured metadata.
///
/// Core stores `text` as the exact normalized body. `metadata` retains the
/// native container shape and non-body fields without cloning the selected
/// text a second time. Unknown structured values are serialized completely
/// into `text`, so no content is dropped merely because its shape is newer.
pub(crate) fn codex_output_content(value: &Value) -> CodexOutputContent<'_> {
    match value {
        Value::String(text) => CodexOutputContent {
            text: Cow::Borrowed(text),
            metadata: None,
        },
        Value::Null => CodexOutputContent {
            text: Cow::Borrowed(""),
            metadata: None,
        },
        Value::Array(items) => {
            codex_array_output_content(items).unwrap_or_else(|| serialized_output_content(value))
        }
        Value::Object(object) => {
            codex_object_output_content(object).unwrap_or_else(|| serialized_output_content(value))
        }
        Value::Bool(_) | Value::Number(_) => serialized_output_content(value),
    }
}

fn codex_array_output_content(items: &[Value]) -> Option<CodexOutputContent<'static>> {
    if items.is_empty() {
        return None;
    }
    let mut parts = Vec::with_capacity(items.len());
    let mut metadata = Vec::with_capacity(items.len());
    for item in items {
        let Value::Object(object) = item else {
            return None;
        };
        let projected = codex_object_output_content(object)?;
        parts.push(projected.text.into_owned());
        metadata.push(projected.metadata.unwrap_or(Value::Null));
    }
    Some(CodexOutputContent {
        text: Cow::Owned(parts.join("\n")),
        metadata: metadata
            .iter()
            .any(|value| !value.is_null())
            .then_some(Value::Array(metadata)),
    })
}

fn codex_object_output_content(object: &Map<String, Value>) -> Option<CodexOutputContent<'static>> {
    for key in [
        "text",
        "input_text",
        "output_text",
        "summary_text",
        "content",
    ] {
        let Some(value) = object.get(key) else {
            continue;
        };
        if !matches!(value, Value::String(_) | Value::Array(_) | Value::Object(_)) {
            continue;
        }
        let projected = codex_output_content(value);
        let mut metadata = object.clone();
        match projected.metadata {
            Some(child) => {
                metadata.insert(key.to_owned(), child);
            }
            None => {
                metadata.remove(key);
            }
        }
        return Some(CodexOutputContent {
            text: Cow::Owned(projected.text.into_owned()),
            metadata: (!metadata.is_empty()).then_some(Value::Object(metadata)),
        });
    }
    None
}

fn serialized_output_content(value: &Value) -> CodexOutputContent<'static> {
    CodexOutputContent {
        text: Cow::Owned(serde_json::to_string(value).unwrap_or_else(|_| value.to_string())),
        metadata: None,
    }
}

#[cfg(test)]
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
        Value::String(text) => codex_exit_code(text),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn output_content_keeps_exact_text_once_and_retains_block_metadata() {
        let value = json!({
            "content": [
                {"type": "text", "text": "first"},
                {"type": "text", "text": "second", "annotations": {"audience": ["user"]}}
            ],
            "isError": false,
            "_meta": {"surface": "browser"}
        });

        let projected = codex_output_content(&value);
        assert_eq!(projected.text, "first\nsecond");
        let metadata = projected.metadata.unwrap();
        assert_eq!(metadata["content"][0]["type"], "text");
        assert_eq!(metadata["content"][1]["annotations"]["audience"][0], "user");
        assert_eq!(metadata["isError"], false);
        assert_eq!(metadata["_meta"]["surface"], "browser");
        assert!(!serde_json::to_string(&metadata).unwrap().contains("first"));
        assert!(!serde_json::to_string(&metadata).unwrap().contains("second"));
    }

    #[test]
    fn unknown_mixed_content_is_serialized_completely_without_a_second_copy() {
        let value = json!({
            "content": [
                {"type": "text", "text": "caption"},
                {"type": "image", "mimeType": "image/png", "data": "complete-image-data"}
            ],
            "isError": false
        });

        let projected = codex_output_content(&value);
        assert!(projected.text.contains("caption"));
        assert!(projected.text.contains("complete-image-data"));
        assert_eq!(projected.metadata.unwrap()["isError"], false);
    }
}
