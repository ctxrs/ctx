use std::borrow::Cow;

use serde::Deserialize;
use serde_json::{Map, Value};

#[cfg(test)]
use super::retention::{codex_exit_code, codex_wall_time_ms};
#[cfg(test)]
use crate::provider::normalization::provider_output_event_is_failure;
#[cfg(test)]
use crate::{OutputOutcome, OutputOutcomeMetadata};

const MAX_CODEX_EXEC_RESULT_ENVELOPE_BYTES: usize = 1024 * 1024;

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

/// Proves the exact native `function_call_output` shape and Codex's successful
/// exec envelope without deriving control evidence from normalized prose.
/// Unknown or duplicate members are rejected by the typed envelope.
pub(crate) fn codex_exact_successful_function_output(
    record: &[u8],
    expected_call_id: &str,
) -> bool {
    let Ok(envelope) = serde_json::from_slice::<ExactFunctionOutputEnvelope<'_>>(record) else {
        return false;
    };
    envelope.record_type == "response_item"
        && envelope.payload.item_type == "function_call_output"
        && !envelope.payload.call_id.is_empty()
        && envelope.payload.call_id == expected_call_id
        && envelope
            .timestamp
            .as_deref()
            .is_none_or(|timestamp| !timestamp.is_empty())
        && exact_codex_exec_result_body(&envelope.payload.output).is_ok_and(|body| body.is_some())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactFunctionOutputEnvelope<'a> {
    #[serde(default, borrow)]
    timestamp: Option<Cow<'a, str>>,
    #[serde(rename = "type", borrow)]
    record_type: Cow<'a, str>,
    #[serde(borrow)]
    payload: ExactFunctionOutputPayload<'a>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactFunctionOutputPayload<'a> {
    #[serde(rename = "type", borrow)]
    item_type: Cow<'a, str>,
    #[serde(borrow)]
    call_id: Cow<'a, str>,
    #[serde(borrow)]
    output: Cow<'a, str>,
}

/// Parses the two bounded Codex exec result envelope revisions observed in
/// native rollout records. The returned body is evidence only; callers retain
/// the original complete provider-normalized body.
pub(crate) fn exact_codex_exec_result_body(output: &str) -> Result<Option<&str>, ()> {
    if !output.starts_with("Chunk ID: ") {
        return if output
            .lines()
            .any(|line| line.trim().starts_with("Chunk ID: "))
        {
            Err(())
        } else {
            Ok(None)
        };
    }
    if output.is_empty()
        || output.len() > MAX_CODEX_EXEC_RESULT_ENVELOPE_BYTES
        || output.contains('\0')
    {
        return Err(());
    }
    let (chunk_id, remainder) = output
        .strip_prefix("Chunk ID: ")
        .and_then(|value| value.split_once('\n'))
        .ok_or(())?;
    if chunk_id.len() != 6
        || !chunk_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(());
    }
    let (wall_time, remainder) = remainder
        .strip_prefix("Wall time: ")
        .and_then(|value| value.split_once(" seconds\n"))
        .ok_or(())?;
    if wall_time.is_empty() || wall_time.len() > 32 {
        return Err(());
    }
    let mut wall_time_components = wall_time.split('.');
    let whole = wall_time_components.next().ok_or(())?;
    let fractional = wall_time_components.next();
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || fractional.is_some_and(|value| {
            value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit())
        })
        || wall_time_components.next().is_some()
        || wall_time
            .parse::<f64>()
            .ok()
            .is_none_or(|seconds| !seconds.is_finite())
    {
        return Err(());
    }
    let remainder = remainder
        .strip_prefix("Process exited with code 0\n")
        .ok_or(())?;
    let body = if let Some(remainder) = remainder.strip_prefix("Original token count: ") {
        let (token_count, remainder) = remainder.split_once('\n').ok_or(())?;
        if token_count.is_empty()
            || token_count.len() > 20
            || !token_count.bytes().all(|byte| byte.is_ascii_digit())
            || token_count.parse::<u64>().is_err()
        {
            return Err(());
        }
        remainder.strip_prefix("Output:\n").ok_or(())?
    } else {
        remainder.strip_prefix("Final output:\n").ok_or(())?
    };
    if body.is_empty()
        || body.len() > MAX_CODEX_EXEC_RESULT_ENVELOPE_BYTES
        || body.lines().any(|line| {
            let line = line.trim();
            line.starts_with("Chunk ID: ")
                || line.starts_with("Wall time: ")
                || line.starts_with("Process exited with code ")
                || line.starts_with("Original token count: ")
                || line == "Output:"
                || line == "Final output:"
                || line.starts_with("Warning: truncated output (original token count: ")
                || line.starts_with("Warning: truncated output (original char count: ")
        })
    {
        return Err(());
    }
    Ok(Some(body))
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
    fn exact_function_output_envelope_is_structural_and_fail_open() {
        let output = concat!(
            "Chunk ID: abc123\n",
            "Wall time: 0.125 seconds\n",
            "Process exited with code 0\n",
            "Final output:\n",
            "{\"results\":[]}"
        );
        let exact = serde_json::to_vec(&json!({
            "timestamp": "2026-08-05T12:00:00Z",
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": "call-exact",
                "output": output
            }
        }))
        .unwrap();
        assert!(codex_exact_successful_function_output(&exact, "call-exact"));
        assert_eq!(
            exact_codex_exec_result_body(output),
            Ok(Some("{\"results\":[]}"))
        );

        let with_stderr = serde_json::to_vec(&json!({
            "timestamp": "2026-08-05T12:00:00Z",
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": "call-exact",
                "output": output,
                "stderr": "diagnostic"
            }
        }))
        .unwrap();
        assert!(!codex_exact_successful_function_output(
            &with_stderr,
            "call-exact"
        ));

        let duplicate_output = br#"{"timestamp":"2026-08-05T12:00:00Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-exact","output":"first","output":"Chunk ID: abc123\nWall time: 0.125 seconds\nProcess exited with code 0\nFinal output:\n{}"}}"#;
        assert!(!codex_exact_successful_function_output(
            duplicate_output,
            "call-exact"
        ));
        for malformed in [
            "Chunk ID: abc123\nWall time: 0.125 seconds\nProcess exited with code 0\n{}",
            "Chunk ID: abc123\nWall time: 0.125 seconds\nProcess exited with code 7\nFinal output:\n{}",
            "Chunk ID: abc123\nWall time: 0.125 seconds\nProcess exited with code 0\nFinal output:\nWarning: truncated output (original token count: 7)\n{}",
        ] {
            assert_eq!(exact_codex_exec_result_body(malformed), Err(()));
        }
    }

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
