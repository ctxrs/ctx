use super::*;

pub(crate) fn factory_droid_file_is_selected(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("jsonl")
}

pub(crate) fn factory_droid_event_identity(value: &Value) -> Option<&str> {
    value
        .get("id")
        .or_else(|| value.get("uuid"))
        .and_then(Value::as_str)
        .filter(|event_id| !event_id.trim().is_empty())
}

pub(crate) fn factory_droid_header_session_id(value: &Value) -> Option<String> {
    (value.get("type").and_then(Value::as_str) == Some("session_start"))
        .then(|| {
            value
                .get("sessionId")
                .or_else(|| value.get("id"))
                .and_then(Value::as_str)
        })
        .flatten()
        .filter(|session_id| !session_id.trim().is_empty())
        .map(str::to_owned)
}

pub(crate) fn factory_droid_header_cwd(value: &Value) -> Option<String> {
    value
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.trim().is_empty())
        .map(str::to_owned)
}

pub(crate) fn factory_droid_session_relationships(
    header: &Value,
    native_session_id: &str,
) -> (String, Option<String>, Option<String>, AgentType) {
    let parent = header
        .get("parent")
        .or_else(|| header.get("callingSessionId"))
        .and_then(Value::as_str)
        .filter(|session_id| !session_id.trim().is_empty())
        .map(str::to_owned);
    let agent_type = if parent.is_some()
        || header.get("decompSessionType").and_then(Value::as_str) == Some("worker")
    {
        AgentType::Subagent
    } else {
        AgentType::Primary
    };
    (
        native_session_id.to_owned(),
        parent,
        header
            .get("decompMissionId")
            .and_then(Value::as_str)
            .filter(|mission_id| !mission_id.trim().is_empty())
            .map(str::to_owned),
        agent_type,
    )
}

pub(crate) fn factory_droid_event_type(value: &Value) -> EventType {
    match value.get("type").and_then(Value::as_str) {
        Some("message") if factory_droid_content_has(value, "tool_use") => EventType::ToolCall,
        Some("message") if factory_droid_content_has(value, "tool_result") => EventType::ToolOutput,
        Some("message") => EventType::Message,
        Some("compaction_state") => EventType::Summary,
        Some("todo_state" | "session_start") => EventType::Notice,
        _ => EventType::Notice,
    }
}

pub(crate) fn factory_droid_role(value: &Value) -> EventRole {
    provider_role(
        value
            .get("role")
            .or_else(|| value.pointer("/message/role"))
            .and_then(Value::as_str),
    )
}

pub(crate) fn factory_droid_event_text(value: &Value) -> String {
    value
        .get("content")
        .or_else(|| value.pointer("/message/content"))
        .and_then(provider_value_text)
        .or_else(|| {
            value
                .get("summary")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .or_else(|| value.get("items").and_then(provider_value_text))
        .unwrap_or_default()
}

pub(crate) fn factory_droid_model(value: &Value) -> Option<Value> {
    value
        .get("model")
        .cloned()
        .or_else(|| value.pointer("/message/model").cloned())
        .or_else(|| value.pointer("/metadata/model").cloned())
}

pub(crate) fn enumerate_factory_droid_results(
    value: &Value,
) -> std::result::Result<Vec<NativeJsonlResultSubrecord<'_>>, NativeJsonlResultExtractionError> {
    if value.get("type").and_then(Value::as_str) != Some("message") {
        return Ok(Vec::new());
    }
    let content = value
        .get("content")
        .or_else(|| value.pointer("/message/content"));
    if reject_redacted(value).is_err() {
        return placeholder_results(content);
    }
    enumerate_content_results(content, value)
}

fn factory_droid_content_has(value: &Value, expected: &str) -> bool {
    value
        .get("content")
        .or_else(|| value.pointer("/message/content"))
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some(expected))
        })
}

fn placeholder_results(
    content: Option<&Value>,
) -> std::result::Result<Vec<NativeJsonlResultSubrecord<'_>>, NativeJsonlResultExtractionError> {
    let count = content
        .map(|content| {
            content
                .as_array()
                .ok_or(NativeJsonlResultExtractionError::InvalidShape)
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter(|block| {
                            block.get("type").and_then(Value::as_str) == Some("tool_result")
                        })
                        .count()
                })
        })
        .transpose()?
        .unwrap_or(0);
    (0..count)
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
        .collect()
}

fn enumerate_content_results<'a>(
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
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        .enumerate()
        .map(|(index, block)| {
            let (content, redacted) =
                match extract_result_ref(Some(block), &["content", "output", "text"]) {
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
