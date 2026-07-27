use ctx_history_core::CaptureProvider;
use serde_json::Value;

use crate::provider::normalization::provider_output_event_is_failure;
use crate::{OutputOutcome, OutputOutcomeMetadata};

pub(crate) const GEMINI_RESULT_PROFILE: &str = "gemini-jsonl.result-body.v1";
pub(crate) const TABNINE_RESULT_PROFILE: &str = "tabnine.result-body.v1";
pub(crate) const FACTORY_DROID_RESULT_PROFILE: &str = "factory-droid.result-body.v1";
pub(crate) const COPILOT_CLI_RESULT_PROFILE: &str = "copilot-cli.result-body.v1";
pub(crate) const QWEN_CODE_RESULT_PROFILE: &str = "qwen-code.result-body.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeJsonlResultExtractionError {
    UnsupportedProfile,
    Ambiguous,
    Redacted,
    InvalidShape,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NativeJsonlResultSubrecord<'a> {
    pub(super) subrecord_index: u32,
    pub(super) content: Option<&'a str>,
    pub(super) call_id: Option<&'a str>,
    pub(super) tool_name: Option<&'a str>,
    pub(super) outcome: OutputOutcomeMetadata,
}

pub(super) fn enumerate_native_jsonl_result_subrecords<'a>(
    profile: &str,
    value: &'a Value,
) -> Result<Vec<NativeJsonlResultSubrecord<'a>>, NativeJsonlResultExtractionError> {
    if !matches!(
        profile,
        GEMINI_RESULT_PROFILE | TABNINE_RESULT_PROFILE | COPILOT_CLI_RESULT_PROFILE
    ) {
        return Err(NativeJsonlResultExtractionError::UnsupportedProfile);
    }
    if reject_redacted(value).is_err() {
        return enumerate_redacted_result_subrecords(profile, value);
    }
    match profile {
        GEMINI_RESULT_PROFILE => enumerate_tool_call_results(value, "gemini"),
        TABNINE_RESULT_PROFILE => enumerate_tool_call_results(value, "tabnine"),
        COPILOT_CLI_RESULT_PROFILE => enumerate_copilot_results(value),
        _ => Err(NativeJsonlResultExtractionError::UnsupportedProfile),
    }
}

#[cfg(test)]
pub(crate) fn gemini_result_subrecord_oracle_for_tests(
    value: &Value,
) -> Result<Vec<(u32, Option<String>, OutputOutcomeMetadata)>, NativeJsonlResultExtractionError> {
    enumerate_native_jsonl_result_subrecords(GEMINI_RESULT_PROFILE, value).map(|subrecords| {
        subrecords
            .into_iter()
            .map(|subrecord| {
                (
                    subrecord.subrecord_index,
                    subrecord.content.map(str::to_owned),
                    subrecord.outcome,
                )
            })
            .collect()
    })
}

fn enumerate_tool_call_results<'a>(
    value: &'a Value,
    expected_type: &str,
) -> Result<Vec<NativeJsonlResultSubrecord<'a>>, NativeJsonlResultExtractionError> {
    if value.get("type").and_then(Value::as_str) != Some(expected_type) {
        return Ok(Vec::new());
    }
    let Some(calls) = value.get("toolCalls") else {
        return Ok(Vec::new());
    };
    let calls = calls
        .as_array()
        .ok_or(NativeJsonlResultExtractionError::InvalidShape)?;
    calls
        .iter()
        .filter(|call| call.get("result").is_some())
        .enumerate()
        .map(|(index, call)| {
            let subrecord_index =
                u32::try_from(index).map_err(|_| NativeJsonlResultExtractionError::InvalidShape)?;
            let (content, redacted) = if reject_redacted(call).is_err() {
                (None, true)
            } else {
                extract_result_ref_preserving_subrecord(
                    call.get("result"),
                    &["content", "output", "text"],
                )?
            };
            Ok(NativeJsonlResultSubrecord {
                subrecord_index,
                content,
                call_id: (!redacted).then(|| native_result_identity(call)).flatten(),
                tool_name: (!redacted).then(|| native_result_tool_name(call)).flatten(),
                outcome: if redacted {
                    unknown_result_outcome()
                } else {
                    native_result_outcome(call)
                },
            })
        })
        .collect()
}

fn enumerate_redacted_result_subrecords<'a>(
    profile: &str,
    value: &'a Value,
) -> Result<Vec<NativeJsonlResultSubrecord<'a>>, NativeJsonlResultExtractionError> {
    let count = match profile {
        GEMINI_RESULT_PROFILE => redacted_tool_call_result_count(value, "gemini")?,
        TABNINE_RESULT_PROFILE => redacted_tool_call_result_count(value, "tabnine")?,
        COPILOT_CLI_RESULT_PROFILE => usize::from(
            value.get("type").and_then(Value::as_str) == Some("tool.execution_complete")
                && value.get("data").is_some(),
        ),
        _ => return Err(NativeJsonlResultExtractionError::UnsupportedProfile),
    };
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

fn redacted_tool_call_result_count(
    value: &Value,
    expected_type: &str,
) -> Result<usize, NativeJsonlResultExtractionError> {
    if value.get("type").and_then(Value::as_str) != Some(expected_type) {
        return Ok(0);
    }
    let Some(calls) = value.get("toolCalls") else {
        return Ok(0);
    };
    Ok(calls
        .as_array()
        .ok_or(NativeJsonlResultExtractionError::InvalidShape)?
        .iter()
        .filter(|call| call.get("result").is_some())
        .count())
}

fn extract_result_ref_preserving_subrecord<'a>(
    value: Option<&'a Value>,
    object_fields: &[&str],
) -> Result<(Option<&'a str>, bool), NativeJsonlResultExtractionError> {
    match extract_direct_result_ref(value, object_fields) {
        Ok(content) => Ok((content, false)),
        Err(NativeJsonlResultExtractionError::Redacted) => Ok((None, true)),
        Err(error) => Err(error),
    }
}

fn enumerate_copilot_results(
    value: &Value,
) -> Result<Vec<NativeJsonlResultSubrecord<'_>>, NativeJsonlResultExtractionError> {
    if value.get("type").and_then(Value::as_str) != Some("tool.execution_complete") {
        return Ok(Vec::new());
    }
    let Some(data) = value.get("data") else {
        return Ok(Vec::new());
    };
    reject_redacted(data)?;
    let selected = if data.get("content").is_some() {
        data.get("content")
    } else if data.pointer("/result/content").is_some() {
        if let Some(result) = data.get("result") {
            reject_redacted(result)?;
        }
        data.pointer("/result/content")
    } else {
        if let Some(error) = data.get("error") {
            reject_redacted(error)?;
        }
        data.pointer("/error/message")
    };
    Ok(vec![NativeJsonlResultSubrecord {
        subrecord_index: 0,
        content: extract_direct_result_ref(selected, &[])?,
        call_id: native_result_identity(data).or_else(|| native_result_identity(value)),
        tool_name: native_result_tool_name(data).or_else(|| native_result_tool_name(value)),
        outcome: native_result_outcome(data),
    }])
}

fn extract_direct_result_ref<'a>(
    value: Option<&'a Value>,
    object_fields: &[&str],
) -> Result<Option<&'a str>, NativeJsonlResultExtractionError> {
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

fn unknown_result_outcome() -> OutputOutcomeMetadata {
    OutputOutcomeMetadata {
        outcome: OutputOutcome::Unknown,
        exit_code: None,
        duration_ms: None,
    }
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

/// Returns the single allowlisted result-content profile for a direct native
/// JSONL provider. The profile token is stable data: changing field selection
/// or normalization requires a new token.
pub(crate) const fn native_jsonl_result_content_profile(
    provider: CaptureProvider,
) -> Option<&'static str> {
    match provider {
        CaptureProvider::Gemini => Some(GEMINI_RESULT_PROFILE),
        CaptureProvider::Tabnine => Some(TABNINE_RESULT_PROFILE),
        CaptureProvider::FactoryAiDroid => Some(FACTORY_DROID_RESULT_PROFILE),
        CaptureProvider::CopilotCli => Some(COPILOT_CLI_RESULT_PROFILE),
        CaptureProvider::QwenCode => Some(QWEN_CODE_RESULT_PROFILE),
        _ => None,
    }
}

/// Extracts the exact normalized UTF-8 result body for one allowlisted native
/// JSONL profile. It is intentionally pure and contains no fallback traversal:
/// capture and source reopening must call this same function on the same native
/// record. Strings are returned byte-for-byte after JSON decoding; no trimming,
/// newline rewriting, Unicode normalization, or arbitrary object rendering is
/// performed.
pub(crate) fn extract_native_jsonl_result_content(
    profile: &str,
    value: &Value,
) -> Result<Option<String>, NativeJsonlResultExtractionError> {
    if !matches!(
        profile,
        GEMINI_RESULT_PROFILE
            | TABNINE_RESULT_PROFILE
            | FACTORY_DROID_RESULT_PROFILE
            | COPILOT_CLI_RESULT_PROFILE
            | QWEN_CODE_RESULT_PROFILE
    ) {
        return Err(NativeJsonlResultExtractionError::UnsupportedProfile);
    }
    reject_redacted(value)?;
    match profile {
        GEMINI_RESULT_PROFILE => extract_tool_calls_result(value, "gemini"),
        TABNINE_RESULT_PROFILE => extract_tool_calls_result(value, "tabnine"),
        FACTORY_DROID_RESULT_PROFILE => {
            if value.get("type").and_then(Value::as_str) != Some("message") {
                return Ok(None);
            }
            extract_content_block_result(
                value
                    .get("content")
                    .or_else(|| value.pointer("/message/content")),
            )
        }
        COPILOT_CLI_RESULT_PROFILE => extract_copilot_result(value),
        QWEN_CODE_RESULT_PROFILE => extract_qwen_result(value),
        _ => Err(NativeJsonlResultExtractionError::UnsupportedProfile),
    }
}

fn extract_tool_calls_result(
    value: &Value,
    expected_type: &str,
) -> Result<Option<String>, NativeJsonlResultExtractionError> {
    if value.get("type").and_then(Value::as_str) != Some(expected_type) {
        return Ok(None);
    }
    let Some(calls) = value.get("toolCalls") else {
        return Ok(None);
    };
    let calls = calls
        .as_array()
        .ok_or(NativeJsonlResultExtractionError::InvalidShape)?;
    let mut candidates = calls.iter().filter(|call| call.get("result").is_some());
    let Some(call) = candidates.next() else {
        return Ok(None);
    };
    if candidates.next().is_some() {
        return Err(NativeJsonlResultExtractionError::Ambiguous);
    }
    reject_redacted(call)?;
    extract_direct_result(call.get("result"), &["content", "output", "text"])
}

fn extract_content_block_result(
    content: Option<&Value>,
) -> Result<Option<String>, NativeJsonlResultExtractionError> {
    let Some(content) = content else {
        return Ok(None);
    };
    let blocks = content
        .as_array()
        .ok_or(NativeJsonlResultExtractionError::InvalidShape)?;
    let mut candidates = blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"));
    let Some(block) = candidates.next() else {
        return Ok(None);
    };
    if candidates.next().is_some() {
        return Err(NativeJsonlResultExtractionError::Ambiguous);
    }
    reject_redacted(block)?;
    extract_direct_result(Some(block), &["content", "output", "text"])
}

fn extract_copilot_result(
    value: &Value,
) -> Result<Option<String>, NativeJsonlResultExtractionError> {
    if value.get("type").and_then(Value::as_str) != Some("tool.execution_complete") {
        return Ok(None);
    }
    let Some(data) = value.get("data") else {
        return Ok(None);
    };
    reject_redacted(data)?;

    // Preserve the provider normalizer's explicit precedence. A lower-priority
    // field is not an ambiguity and is not inspected once a higher one exists.
    if data.get("content").is_some() {
        return extract_direct_result(data.get("content"), &[]);
    }
    if let Some(result) = data.get("result") {
        reject_redacted(result)?;
    }
    if data.pointer("/result/content").is_some() {
        return extract_direct_result(data.pointer("/result/content"), &[]);
    }
    if let Some(error) = data.get("error") {
        reject_redacted(error)?;
    }
    extract_direct_result(data.pointer("/error/message"), &[])
}

fn extract_qwen_result(value: &Value) -> Result<Option<String>, NativeJsonlResultExtractionError> {
    if value.get("type").and_then(Value::as_str) != Some("tool_result")
        && value.get("toolCallResult").is_none()
    {
        return Ok(None);
    }
    if let Some(result) = extract_content_block_result(value.pointer("/message/content"))? {
        return Ok(Some(result));
    }
    if value.get("toolCallResult").is_some() {
        return extract_direct_result(value.get("toolCallResult"), &["output", "content", "text"]);
    }
    extract_direct_result(value.get("content"), &[])
}

fn extract_direct_result(
    value: Option<&Value>,
    object_fields: &[&str],
) -> Result<Option<String>, NativeJsonlResultExtractionError> {
    let Some(value) = value else {
        return Ok(None);
    };
    reject_redacted(value)?;
    match value {
        Value::String(text) => Ok(Some(text.clone())),
        Value::Null => Ok(None),
        Value::Object(object) => {
            for field in object_fields {
                if let Some(selected) = object.get(*field) {
                    return match selected {
                        Value::String(text) => Ok(Some(text.clone())),
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

fn reject_redacted(value: &Value) -> Result<(), NativeJsonlResultExtractionError> {
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
    use super::*;
    use serde_json::json;

    #[test]
    fn profile_tokens_are_provider_specific_and_stable() {
        for (provider, expected) in [
            (CaptureProvider::Gemini, GEMINI_RESULT_PROFILE),
            (CaptureProvider::Tabnine, TABNINE_RESULT_PROFILE),
            (
                CaptureProvider::FactoryAiDroid,
                FACTORY_DROID_RESULT_PROFILE,
            ),
            (CaptureProvider::CopilotCli, COPILOT_CLI_RESULT_PROFILE),
            (CaptureProvider::QwenCode, QWEN_CODE_RESULT_PROFILE),
        ] {
            assert_eq!(
                native_jsonl_result_content_profile(provider),
                Some(expected)
            );
        }
        for provider in [
            CaptureProvider::Antigravity,
            CaptureProvider::Windsurf,
            CaptureProvider::Qoder,
            CaptureProvider::Codex,
        ] {
            assert_eq!(native_jsonl_result_content_profile(provider), None);
        }
    }

    #[test]
    fn extracts_checked_in_native_result_shapes_exactly() {
        let cases = [
            (
                GEMINI_RESULT_PROFILE,
                json!({"type":"gemini","toolCalls":[{"result":{"content":"gemini\nresult "}}]}),
                "gemini\nresult ",
            ),
            (
                TABNINE_RESULT_PROFILE,
                json!({"type":"tabnine","toolCalls":[{"result":"tabnine result"}]}),
                "tabnine result",
            ),
            (
                FACTORY_DROID_RESULT_PROFILE,
                json!({"type":"message","message":{"content":[{"type":"tool_result","content":"droid result"}]}}),
                "droid result",
            ),
            (
                COPILOT_CLI_RESULT_PROFILE,
                json!({"type":"tool.execution_complete","data":{"result":{"content":"copilot result"}}}),
                "copilot result",
            ),
            (
                QWEN_CODE_RESULT_PROFILE,
                json!({"type":"tool_result","message":{"content":[{"type":"tool_result","content":"qwen result"}]},"toolCallResult":{"output":"lower priority"}}),
                "qwen result",
            ),
        ];
        for (profile, value, expected) in cases {
            assert_eq!(
                extract_native_jsonl_result_content(profile, &value).unwrap(),
                Some(expected.to_owned()),
                "{profile}"
            );
        }
    }

    #[test]
    fn enumerates_multiple_result_subrecords_in_native_order() {
        let gemini = json!({
            "type": "gemini",
            "toolCalls": [
                {"id": "call-0", "result": {"content": "zero"}, "success": true},
                {"id": "call-only"},
                {"id": "call-2", "name": "shell", "result": {"content": "two"}, "exitCode": 9}
            ]
        });
        let results =
            enumerate_native_jsonl_result_subrecords(GEMINI_RESULT_PROFILE, &gemini).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].subrecord_index, 0);
        assert_eq!(results[0].content, Some("zero"));
        assert_eq!(results[0].call_id, Some("call-0"));
        assert_eq!(results[0].outcome.outcome, OutputOutcome::Success);
        assert_eq!(results[1].subrecord_index, 1);
        assert_eq!(results[1].content, Some("two"));
        assert_eq!(results[1].tool_name, Some("shell"));
        assert_eq!(results[1].outcome.outcome, OutputOutcome::Failure);
        assert_eq!(results[1].outcome.exit_code, Some(9));
    }

    #[test]
    fn redaction_preserves_all_native_subrecord_coordinates() {
        let partially_redacted = json!({
            "type": "gemini",
            "toolCalls": [
                {"result": {"content": "visible-zero"}},
                {"result": {"redacted": true, "content": "secret-one"}},
                {"result": {"content": "visible-two"}}
            ]
        });
        let results =
            enumerate_native_jsonl_result_subrecords(GEMINI_RESULT_PROFILE, &partially_redacted)
                .unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].content, Some("visible-zero"));
        assert_eq!(results[1].content, None);
        assert_eq!(results[1].outcome.outcome, OutputOutcome::Unknown);
        assert_eq!(results[2].subrecord_index, 2);
        assert_eq!(results[2].content, Some("visible-two"));

        let entirely_redacted = json!({
            "redacted": true,
            "type": "gemini",
            "toolCalls": [
                {"result": {"content": "secret-zero"}},
                {"result": {"content": "secret-one"}}
            ]
        });
        let results =
            enumerate_native_jsonl_result_subrecords(GEMINI_RESULT_PROFILE, &entirely_redacted)
                .unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|result| result.content.is_none()));
        assert_eq!(results[1].subrecord_index, 1);
    }

    #[test]
    fn explicit_field_precedence_is_deterministic() {
        assert_eq!(
            extract_native_jsonl_result_content(
                GEMINI_RESULT_PROFILE,
                &json!({"type":"gemini","toolCalls":[{"result":{"content":"content","output":"output","text":"text"}}]}),
            )
            .unwrap(),
            Some("content".to_owned())
        );
        assert_eq!(
            extract_native_jsonl_result_content(
                COPILOT_CLI_RESULT_PROFILE,
                &json!({"type":"tool.execution_complete","data":{"content":"content","result":{"content":"result"},"error":{"message":"error"}}}),
            )
            .unwrap(),
            Some("content".to_owned())
        );
        assert_eq!(
            extract_native_jsonl_result_content(
                QWEN_CODE_RESULT_PROFILE,
                &json!({"type":"tool_result","toolCallResult":{"output":"output","content":"content","text":"text"}}),
            )
            .unwrap(),
            Some("output".to_owned())
        );
    }

    #[test]
    fn absent_ambiguous_redacted_and_unknown_results_fail_closed() {
        for profile in [
            GEMINI_RESULT_PROFILE,
            TABNINE_RESULT_PROFILE,
            FACTORY_DROID_RESULT_PROFILE,
            COPILOT_CLI_RESULT_PROFILE,
            QWEN_CODE_RESULT_PROFILE,
        ] {
            assert_eq!(
                extract_native_jsonl_result_content(profile, &json!({})).unwrap(),
                None,
                "{profile}"
            );
            assert_eq!(
                extract_native_jsonl_result_content(profile, &json!({"redacted":true})),
                Err(NativeJsonlResultExtractionError::Redacted),
                "{profile}"
            );
        }
        assert_eq!(
            extract_native_jsonl_result_content(
                GEMINI_RESULT_PROFILE,
                &json!({"type":"gemini","toolCalls":[{"result":"one"},{"result":"two"}]}),
            ),
            Err(NativeJsonlResultExtractionError::Ambiguous)
        );
        for (profile, value) in [
            (
                GEMINI_RESULT_PROFILE,
                json!({"type":"gemini","toolCalls":[{"result":{"redacted":true,"content":"secret"}}]}),
            ),
            (
                FACTORY_DROID_RESULT_PROFILE,
                json!({"type":"message","content":[{"type":"tool_result","redacted":true,"content":"secret"}]}),
            ),
            (
                COPILOT_CLI_RESULT_PROFILE,
                json!({"type":"tool.execution_complete","data":{"result":{"redacted":true,"content":"secret"}}}),
            ),
            (
                QWEN_CODE_RESULT_PROFILE,
                json!({"type":"tool_result","toolCallResult":{"redacted":true,"output":"secret"}}),
            ),
        ] {
            assert_eq!(
                extract_native_jsonl_result_content(profile, &value),
                Err(NativeJsonlResultExtractionError::Redacted),
                "{profile}"
            );
        }
        assert_eq!(
            extract_native_jsonl_result_content("unregistered-profile-v1", &json!({})),
            Err(NativeJsonlResultExtractionError::UnsupportedProfile)
        );
    }

    #[test]
    fn malformed_selected_fields_do_not_fall_back() {
        assert_eq!(
            extract_native_jsonl_result_content(
                QWEN_CODE_RESULT_PROFILE,
                &json!({"toolCallResult":{"output":{"nested":"not accepted"},"content":"fallback"}}),
            ),
            Err(NativeJsonlResultExtractionError::InvalidShape)
        );
        assert_eq!(
            extract_native_jsonl_result_content(
                FACTORY_DROID_RESULT_PROFILE,
                &json!({"type":"message","content":"not a tool-result block array","message":{"content":[{"type":"tool_result","content":"must not fall through"}]}}),
            ),
            Err(NativeJsonlResultExtractionError::InvalidShape)
        );
    }
}
