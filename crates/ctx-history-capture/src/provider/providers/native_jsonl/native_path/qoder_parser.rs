use ctx_history_core::{EventRole, EventType};
use serde_json::Value;

use crate::{
    provider::normalization::{
        provider_output_event_is_failure, provider_role, provider_value_text,
    },
    OutputOutcome, OutputOutcomeMetadata,
};

use super::super::{
    normalization::native_jsonl_content_has,
    result_content::{NativeJsonlResultExtractionError, NativeJsonlResultSubrecord},
};

pub(super) fn qoder_event_identity(value: &Value) -> Option<&str> {
    value
        .get("uuid")
        .or_else(|| value.get("id"))
        .and_then(Value::as_str)
        .filter(|event_id| !event_id.trim().is_empty())
}

pub(super) fn qoder_header_session_id(value: &Value) -> Option<String> {
    value
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
}

pub(super) fn qoder_header_cwd(value: &Value) -> Option<String> {
    value
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.trim().is_empty())
        .map(str::to_owned)
}

pub(super) fn qoder_event_type(value: &Value) -> EventType {
    if qoder_record_is_structural_output(value) {
        return EventType::ToolOutput;
    }
    match value.get("type").and_then(Value::as_str) {
        Some("assistant") if native_jsonl_content_has(value, "tool_use") => EventType::ToolCall,
        Some("user" | "assistant") => EventType::Message,
        Some("progress" | "session_meta") => EventType::Notice,
        _ => EventType::Notice,
    }
}

pub(super) fn qoder_role(value: &Value) -> EventRole {
    if qoder_record_is_structural_output(value) {
        return EventRole::Tool;
    }
    provider_role(
        value
            .pointer("/message/role")
            .or_else(|| value.get("type"))
            .and_then(Value::as_str),
    )
}

pub(super) fn qoder_event_text(value: &Value, event_type: EventType) -> String {
    let primary = if event_type == EventType::ToolOutput {
        value
            .get("toolUseResult")
            .or_else(|| value.pointer("/message/content"))
    } else {
        value
            .pointer("/message/content")
            .or_else(|| value.get("toolUseResult"))
    };
    primary
        .or_else(|| value.pointer("/data/content"))
        .and_then(provider_value_text)
        .unwrap_or_default()
}

pub(super) fn qoder_model(value: &Value) -> Option<Value> {
    value
        .get("model")
        .cloned()
        .or_else(|| value.pointer("/message/model").cloned())
}

pub(super) fn enumerate_qoder_results(
    value: &Value,
) -> std::result::Result<Vec<NativeJsonlResultSubrecord<'_>>, NativeJsonlResultExtractionError> {
    if reject_redacted(value).is_err() {
        let count = if value.get("toolUseResult").is_some()
            || qoder_top_level_result(value).is_some()
            || value
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(qoder_result_token)
        {
            1
        } else if value.get("type").and_then(Value::as_str) == Some("user") {
            let blocks = result_block_count(value.pointer("/message/content"))?;
            if blocks != 0 {
                blocks
            } else {
                usize::from(value.pointer("/data/content").is_some())
            }
        } else {
            0
        };
        return (0..count)
            .map(|index| {
                Ok(NativeJsonlResultSubrecord {
                    subrecord_index: u32::try_from(index)
                        .map_err(|_| NativeJsonlResultExtractionError::InvalidShape)?,
                    content: None,
                    call_id: None,
                    tool_name: None,
                    outcome: unknown_result_outcome(),
                })
            })
            .collect();
    }
    if let Some(result) = value.get("toolUseResult") {
        reject_redacted(result)?;
        let related_block = first_content_result_block(value.pointer("/message/content"));
        return Ok(vec![NativeJsonlResultSubrecord {
            subrecord_index: 0,
            content: extract_result_ref(Some(result), &["content", "output", "text"])?,
            call_id: native_result_identity(result)
                .or_else(|| related_block.and_then(native_result_identity))
                .or_else(|| native_result_identity(value)),
            tool_name: native_result_tool_name(result)
                .or_else(|| related_block.and_then(native_result_tool_name))
                .or_else(|| native_result_tool_name(value)),
            outcome: native_result_outcome_with_record(result, value),
        }]);
    }
    if let Some(result) = qoder_top_level_result(value).or_else(|| {
        value
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(qoder_result_token)
            .then_some(value)
    }) {
        reject_redacted(result)?;
        return Ok(vec![NativeJsonlResultSubrecord {
            subrecord_index: 0,
            content: extract_result_ref(Some(result), &["content", "output", "result", "text"])?,
            call_id: native_result_identity(result).or_else(|| native_result_identity(value)),
            tool_name: native_result_tool_name(result).or_else(|| native_result_tool_name(value)),
            outcome: native_result_outcome_with_record(result, value),
        }]);
    }
    if value.get("type").and_then(Value::as_str) != Some("user") {
        return Ok(Vec::new());
    }
    let blocks = enumerate_content_block_results(value.pointer("/message/content"), value)?;
    if !blocks.is_empty() {
        return Ok(blocks);
    }
    let Some(content) = value.pointer("/data/content") else {
        return Ok(Vec::new());
    };
    if let Some(data) = value.get("data") {
        reject_redacted(data)?;
    }
    Ok(vec![NativeJsonlResultSubrecord {
        subrecord_index: 0,
        content: extract_result_ref(Some(content), &[])?,
        call_id: value
            .get("data")
            .and_then(native_result_identity)
            .or_else(|| native_result_identity(value)),
        tool_name: value
            .get("data")
            .and_then(native_result_tool_name)
            .or_else(|| native_result_tool_name(value)),
        outcome: native_result_outcome(value),
    }])
}

fn enumerate_content_block_results<'a>(
    content: Option<&'a Value>,
    record: &'a Value,
) -> std::result::Result<Vec<NativeJsonlResultSubrecord<'a>>, NativeJsonlResultExtractionError> {
    let Some(content) = content else {
        return Ok(Vec::new());
    };
    content
        .as_array()
        .ok_or(NativeJsonlResultExtractionError::InvalidShape)?
        .iter()
        .filter(|block| qoder_content_block_is_result(block))
        .enumerate()
        .map(|(index, block)| {
            let (content, redacted) =
                match extract_result_ref(Some(block), &["content", "output", "result", "text"]) {
                    Ok(content) => (content, false),
                    Err(NativeJsonlResultExtractionError::Redacted) => (None, true),
                    Err(error) => return Err(error),
                };
            Ok(NativeJsonlResultSubrecord {
                subrecord_index: u32::try_from(index)
                    .map_err(|_| NativeJsonlResultExtractionError::InvalidShape)?,
                content,
                call_id: (!redacted).then(|| native_result_identity(block)).flatten(),
                tool_name: (!redacted)
                    .then(|| native_result_tool_name(block))
                    .flatten(),
                outcome: if redacted {
                    unknown_result_outcome()
                } else {
                    native_result_outcome_with_record(block, record)
                },
            })
        })
        .collect()
}

fn result_block_count(
    content: Option<&Value>,
) -> std::result::Result<usize, NativeJsonlResultExtractionError> {
    let Some(content) = content else {
        return Ok(0);
    };
    Ok(content
        .as_array()
        .ok_or(NativeJsonlResultExtractionError::InvalidShape)?
        .iter()
        .filter(|block| qoder_content_block_is_result(block))
        .count())
}

fn first_content_result_block(content: Option<&Value>) -> Option<&Value> {
    content.and_then(Value::as_array).and_then(|blocks| {
        blocks
            .iter()
            .find(|block| qoder_content_block_is_result(block))
    })
}

fn qoder_record_is_structural_output(value: &Value) -> bool {
    value.get("toolUseResult").is_some()
        || qoder_top_level_result(value).is_some()
        || value
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(qoder_result_token)
        || value
            .pointer("/message/content")
            .and_then(Value::as_array)
            .is_some_and(|blocks| blocks.iter().any(qoder_content_block_is_result))
}

fn qoder_top_level_result(value: &Value) -> Option<&Value> {
    value.as_object().and_then(|object| {
        object
            .iter()
            .find_map(|(key, value)| qoder_result_token(key).then_some(value))
    })
}

fn qoder_content_block_is_result(block: &Value) -> bool {
    block
        .get("type")
        .or_else(|| block.get("kind"))
        .and_then(Value::as_str)
        .is_some_and(qoder_result_token)
        || block
            .as_object()
            .is_some_and(|object| object.keys().any(|key| qoder_result_token(key)))
}

fn qoder_result_token(value: &str) -> bool {
    let value = normalized_result_key(value);
    matches!(
        value.as_str(),
        "result"
            | "output"
            | "toolresponse"
            | "functioncalloutput"
            | "functionoutput"
            | "commandoutput"
    ) || value.ends_with("result")
        || value.ends_with("output")
}

fn extract_result_ref<'a>(
    value: Option<&'a Value>,
    object_fields: &[&str],
) -> std::result::Result<Option<&'a str>, NativeJsonlResultExtractionError> {
    let Some(value) = value else {
        return Ok(None);
    };
    reject_redacted(value)?;
    match value {
        Value::String(text) => Ok(Some(text)),
        Value::Null => Ok(None),
        Value::Object(object) => {
            for field in object_fields {
                if let Some(selected) = object.get(*field) {
                    return match selected {
                        Value::String(text) => Ok(Some(text)),
                        Value::Null => Ok(None),
                        _ => Err(NativeJsonlResultExtractionError::InvalidShape),
                    };
                }
            }
            Ok(None)
        }
        Value::Array(_) | Value::Bool(_) | Value::Number(_) => {
            Err(NativeJsonlResultExtractionError::InvalidShape)
        }
    }
}

fn native_result_identity(value: &Value) -> Option<&str> {
    [
        "call_id",
        "callId",
        "tool_call_id",
        "toolCallId",
        "tool_use_id",
        "toolUseId",
        "id",
    ]
    .into_iter()
    .find_map(|key| value.get(key).and_then(Value::as_str))
}

fn native_result_tool_name(value: &Value) -> Option<&str> {
    ["tool_name", "toolName", "name", "tool"]
        .into_iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
}

fn native_result_outcome_with_record(subrecord: &Value, record: &Value) -> OutputOutcomeMetadata {
    let mut outcome = native_result_outcome(subrecord);
    if outcome.outcome == OutputOutcome::Unknown {
        outcome = native_result_outcome(record);
    }
    outcome
}

fn native_result_outcome(value: &Value) -> OutputOutcomeMetadata {
    let timeout = native_result_has_timeout(value);
    let failure = provider_output_event_is_failure(value);
    let success = native_result_has_success(value);
    OutputOutcomeMetadata {
        outcome: if timeout {
            OutputOutcome::Timeout
        } else if failure {
            OutputOutcome::Failure
        } else if success {
            OutputOutcome::Success
        } else {
            OutputOutcome::Unknown
        },
        exit_code: native_result_i64(value, &["exit_code", "exitCode"])
            .and_then(|code| i32::try_from(code).ok()),
        duration_ms: native_result_u64(value, &["duration_ms", "durationMs", "duration"]),
    }
}

fn unknown_result_outcome() -> OutputOutcomeMetadata {
    OutputOutcomeMetadata {
        outcome: OutputOutcome::Unknown,
        exit_code: None,
        duration_ms: None,
    }
}

fn native_result_has_timeout(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(native_result_has_timeout),
        Value::Object(values) => {
            values.iter().any(|(key, value)| {
                matches!(normalized_result_key(key).as_str(), "timeout" | "timedout")
                    && value.as_bool().unwrap_or(false)
            }) || values.values().any(native_result_has_timeout)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn native_result_has_success(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(native_result_has_success),
        Value::Object(values) => {
            values.iter().any(|(key, value)| {
                let key = normalized_result_key(key);
                (matches!(key.as_str(), "success" | "ok") && value.as_bool() == Some(true))
                    || (key == "exitcode" && value.as_i64() == Some(0))
                    || (key == "statuscode"
                        && value
                            .as_i64()
                            .is_some_and(|code| (200..400).contains(&code)))
                    || (matches!(key.as_str(), "iserror" | "timedout" | "timeout")
                        && value.as_bool() == Some(false))
                    || (matches!(key.as_str(), "status" | "state" | "outcome")
                        && value.as_str().is_some_and(|status| {
                            matches!(
                                status.trim().to_ascii_lowercase().as_str(),
                                "success"
                                    | "succeeded"
                                    | "complete"
                                    | "completed"
                                    | "ok"
                                    | "passed"
                            )
                        }))
            }) || values.values().any(native_result_has_success)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn native_result_i64(value: &Value, expected_keys: &[&str]) -> Option<i64> {
    match value {
        Value::Array(values) => values
            .iter()
            .find_map(|value| native_result_i64(value, expected_keys)),
        Value::Object(values) => values
            .iter()
            .find_map(|(key, value)| {
                expected_keys
                    .iter()
                    .any(|expected| key == expected)
                    .then(|| value.as_i64())
                    .flatten()
            })
            .or_else(|| {
                values
                    .values()
                    .find_map(|value| native_result_i64(value, expected_keys))
            }),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}

fn native_result_u64(value: &Value, expected_keys: &[&str]) -> Option<u64> {
    match value {
        Value::Array(values) => values
            .iter()
            .find_map(|value| native_result_u64(value, expected_keys)),
        Value::Object(values) => values
            .iter()
            .find_map(|(key, value)| {
                expected_keys
                    .iter()
                    .any(|expected| key == expected)
                    .then(|| value.as_u64())
                    .flatten()
            })
            .or_else(|| {
                values
                    .values()
                    .find_map(|value| native_result_u64(value, expected_keys))
            }),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}

fn normalized_result_key(key: &str) -> String {
    key.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn reject_redacted(value: &Value) -> std::result::Result<(), NativeJsonlResultExtractionError> {
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    let flag_is_redacted = ["redacted", "is_redacted", "isRedacted"]
        .iter()
        .filter_map(|field| object.get(*field))
        .any(|flag| flag.as_bool() != Some(false));
    let state_is_redacted = ["status", "state"]
        .iter()
        .filter_map(|field| object.get(*field).and_then(Value::as_str))
        .any(|state| matches!(state, "redacted" | "output-redacted"));
    if flag_is_redacted || state_is_redacted {
        Err(NativeJsonlResultExtractionError::Redacted)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use ctx_history_core::{EventRole, EventType};
    use serde_json::json;

    use super::*;

    #[test]
    fn top_level_tool_use_result_preempts_generic_user_message() {
        let value = json!({
            "type": "user",
            "uuid": "top-level-result",
            "message": {
                "role": "user",
                "content": "message-shaped secret"
            },
            "toolUseResult": {
                "content": "top-level secret",
                "callId": "call-top",
                "toolName": "read_file",
                "exitCode": 0,
                "durationMs": 17
            }
        });

        assert_eq!(qoder_event_type(&value), EventType::ToolOutput);
        assert_eq!(qoder_role(&value), EventRole::Tool);
        let results = enumerate_qoder_results(&value).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, Some("top-level secret"));
        assert_eq!(results[0].call_id, Some("call-top"));
        assert_eq!(results[0].tool_name, Some("read_file"));
        assert_eq!(results[0].outcome.outcome, OutputOutcome::Success);
        assert_eq!(results[0].outcome.exit_code, Some(0));
        assert_eq!(results[0].outcome.duration_ms, Some(17));
    }

    #[test]
    fn mixed_content_result_preempts_safe_looking_text() {
        let value = json!({
            "type": "user",
            "uuid": "mixed-result",
            "message": {
                "role": "user",
                "content": [
                    {"type": "text", "text": "safe-looking message"},
                    {
                        "type": "tool_result",
                        "tool_use_id": "call-mixed",
                        "name": "shell",
                        "content": "mixed secret",
                        "is_error": false
                    }
                ]
            }
        });

        assert_eq!(qoder_event_type(&value), EventType::ToolOutput);
        assert_eq!(qoder_role(&value), EventRole::Tool);
        let results = enumerate_qoder_results(&value).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, Some("mixed secret"));
        assert_eq!(results[0].call_id, Some("call-mixed"));
        assert_eq!(results[0].tool_name, Some("shell"));
        assert_eq!(results[0].outcome.outcome, OutputOutcome::Success);
    }

    #[test]
    fn future_result_shape_is_output_with_typed_metadata_only() {
        let value = json!({
            "type": "user",
            "uuid": "future-result",
            "message": {
                "role": "user",
                "content": [{
                    "type": "mcp_tool_future_result",
                    "callId": "call-future",
                    "toolName": "future_tool",
                    "payload": {"opaque": "future secret"},
                    "status": "failed",
                    "exitCode": 23,
                    "durationMs": 41
                }]
            }
        });

        assert_eq!(qoder_event_type(&value), EventType::ToolOutput);
        assert_eq!(qoder_role(&value), EventRole::Tool);
        let results = enumerate_qoder_results(&value).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, None);
        assert_eq!(results[0].call_id, Some("call-future"));
        assert_eq!(results[0].tool_name, Some("future_tool"));
        assert_eq!(results[0].outcome.outcome, OutputOutcome::Failure);
        assert_eq!(results[0].outcome.exit_code, Some(23));
        assert_eq!(results[0].outcome.duration_ms, Some(41));
    }
}
