use std::borrow::Cow;

use ctx_history_core::CaptureProvider;
use serde_json::Value;

use crate::provider::normalization::provider_output_event_is_failure;
use crate::{OutputOutcome, OutputOutcomeMetadata};

pub(crate) const GEMINI_RESULT_PROFILE: &str = "gemini-jsonl.result-body.v1";
pub(crate) const TABNINE_RESULT_PROFILE: &str = "tabnine.result-body.v2";
pub(crate) const FACTORY_DROID_RESULT_PROFILE: &str = "factory-droid.result-body.v2";
pub(crate) const COPILOT_CLI_RESULT_PROFILE: &str = "copilot-cli.result-body.v2";
pub(crate) const QWEN_CODE_RESULT_PROFILE: &str = "qwen-code.result-body.v2";

const GEMINI_RESULT_TYPES: &[&str] = &["gemini"];
const TABNINE_RESULT_TYPES: &[&str] = &["tabnine", "gemini"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeJsonlResultExtractionError {
    UnsupportedProfile,
    Redacted,
    InvalidShape,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NativeJsonlResultSubrecord<'a> {
    pub(super) subrecord_index: u32,
    pub(super) content: Option<Cow<'a, str>>,
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
        GEMINI_RESULT_PROFILE => {
            enumerate_tool_call_results(value, GEMINI_RESULT_TYPES, false, false)
        }
        TABNINE_RESULT_PROFILE => {
            enumerate_tool_call_results(value, TABNINE_RESULT_TYPES, true, true)
        }
        COPILOT_CLI_RESULT_PROFILE => enumerate_copilot_results(value),
        _ => Err(NativeJsonlResultExtractionError::UnsupportedProfile),
    }
}

fn result_type_is_allowed(value: &Value, expected_types: &[&str]) -> bool {
    value
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|record_type| expected_types.contains(&record_type))
}

fn enumerate_tool_call_results<'a>(
    value: &'a Value,
    expected_types: &[&str],
    allow_structured_content: bool,
    preserve_native_index: bool,
) -> Result<Vec<NativeJsonlResultSubrecord<'a>>, NativeJsonlResultExtractionError> {
    if !result_type_is_allowed(value, expected_types) {
        return Ok(Vec::new());
    }
    let Some(calls) = value.get("toolCalls") else {
        return Ok(Vec::new());
    };
    let calls = calls
        .as_array()
        .ok_or(NativeJsonlResultExtractionError::InvalidShape)?;
    let mut retained_index = 0_usize;
    calls
        .iter()
        .enumerate()
        .filter(|(_, call)| call.get("result").is_some())
        .map(|(native_index, call)| {
            let index = if preserve_native_index {
                native_index
            } else {
                let index = retained_index;
                retained_index = retained_index.saturating_add(1);
                index
            };
            let subrecord_index =
                u32::try_from(index).map_err(|_| NativeJsonlResultExtractionError::InvalidShape)?;
            let (content, redacted) = if reject_redacted(call).is_err() {
                (None, true)
            } else {
                extract_result_ref_preserving_subrecord(
                    call.get("result"),
                    &["content", "output", "text"],
                    allow_structured_content,
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
    let indices = match profile {
        GEMINI_RESULT_PROFILE => {
            redacted_tool_call_result_indices(value, GEMINI_RESULT_TYPES, false)?
        }
        TABNINE_RESULT_PROFILE => {
            redacted_tool_call_result_indices(value, TABNINE_RESULT_TYPES, true)?
        }
        COPILOT_CLI_RESULT_PROFILE
            if value.get("type").and_then(Value::as_str) == Some("tool.execution_complete")
                && value.get("data").is_some() =>
        {
            vec![0]
        }
        COPILOT_CLI_RESULT_PROFILE => Vec::new(),
        _ => return Err(NativeJsonlResultExtractionError::UnsupportedProfile),
    };
    indices
        .into_iter()
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

fn redacted_tool_call_result_indices(
    value: &Value,
    expected_types: &[&str],
    preserve_native_index: bool,
) -> Result<Vec<usize>, NativeJsonlResultExtractionError> {
    if !result_type_is_allowed(value, expected_types) {
        return Ok(Vec::new());
    }
    let Some(calls) = value.get("toolCalls") else {
        return Ok(Vec::new());
    };
    let native_indices = calls
        .as_array()
        .ok_or(NativeJsonlResultExtractionError::InvalidShape)?
        .iter()
        .enumerate()
        .filter_map(|(index, call)| call.get("result").is_some().then_some(index))
        .collect::<Vec<_>>();
    Ok(if preserve_native_index {
        native_indices
    } else {
        (0..native_indices.len()).collect()
    })
}

fn extract_result_ref_preserving_subrecord<'a>(
    value: Option<&'a Value>,
    object_fields: &[&str],
    allow_structured_content: bool,
) -> Result<(Option<Cow<'a, str>>, bool), NativeJsonlResultExtractionError> {
    match extract_direct_result_content(value, object_fields, allow_structured_content) {
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
    let direct = data.get("content");
    let result = data.pointer("/result/content");
    let error = data.pointer("/error/message");
    if [direct, result, error]
        .into_iter()
        .filter(Option::is_some)
        .count()
        > 1
    {
        return Err(NativeJsonlResultExtractionError::InvalidShape);
    }
    if result.is_some() {
        reject_redacted(
            data.get("result")
                .ok_or(NativeJsonlResultExtractionError::InvalidShape)?,
        )?;
    }
    if error.is_some() {
        reject_redacted(
            data.get("error")
                .ok_or(NativeJsonlResultExtractionError::InvalidShape)?,
        )?;
    }
    let selected = direct.or(result).or(error);
    Ok(vec![NativeJsonlResultSubrecord {
        subrecord_index: 0,
        content: extract_direct_result_content(selected, &[], true)?,
        call_id: native_result_identity(data).or_else(|| native_result_identity(value)),
        tool_name: native_result_tool_name(data).or_else(|| native_result_tool_name(value)),
        outcome: native_result_outcome(data),
    }])
}

pub(super) fn extract_direct_result_content<'a>(
    value: Option<&'a Value>,
    object_fields: &[&str],
    allow_structured_content: bool,
) -> Result<Option<Cow<'a, str>>, NativeJsonlResultExtractionError> {
    let Some(value) = value else {
        return Ok(None);
    };
    reject_redacted(value)?;
    match value {
        Value::String(text) => Ok(Some(Cow::Borrowed(text))),
        Value::Null => Ok(None),
        Value::Object(object) if !object_fields.is_empty() => {
            let mut selected = object_fields.iter().filter_map(|field| object.get(*field));
            let Some(selected_value) = selected.next() else {
                return Ok(None);
            };
            if selected.next().is_some() {
                return Err(NativeJsonlResultExtractionError::InvalidShape);
            }
            extract_direct_result_content(Some(selected_value), &[], allow_structured_content)
        }
        Value::Object(_) | Value::Array(_) | Value::Bool(_) | Value::Number(_)
            if allow_structured_content =>
        {
            serde_json::to_string(value)
                .map(Cow::Owned)
                .map(Some)
                .map_err(|_| NativeJsonlResultExtractionError::InvalidShape)
        }
        Value::Object(_) | Value::Array(_) | Value::Bool(_) | Value::Number(_) => {
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
///
/// Antigravity and Windsurf intentionally have no profile: their admitted
/// native dialects expose calls/messages but no stable result-bearing record
/// shape. Qoder uses its stricter provider-specific parser instead.
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
        assert_eq!(results[0].content.as_deref(), Some("zero"));
        assert_eq!(results[0].call_id, Some("call-0"));
        assert_eq!(results[0].outcome.outcome, OutputOutcome::Success);
        assert_eq!(results[1].subrecord_index, 1);
        assert_eq!(results[1].content.as_deref(), Some("two"));
        assert_eq!(results[1].tool_name, Some("shell"));
        assert_eq!(results[1].outcome.outcome, OutputOutcome::Failure);
        assert_eq!(results[1].outcome.exit_code, Some(9));
    }

    #[test]
    fn tabnine_accepts_current_and_released_gemini_result_dialects() {
        for record_type in ["tabnine", "gemini"] {
            let value = json!({
                "type": record_type,
                "toolCalls": [
                    {"id": "call-visible", "result": {"content": "visible"}},
                    {"id": "call-redacted", "result": {"redacted": true, "content": "secret"}}
                ]
            });
            let results =
                enumerate_native_jsonl_result_subrecords(TABNINE_RESULT_PROFILE, &value).unwrap();
            assert_eq!(results.len(), 2, "{record_type}");
            assert_eq!(
                results[0].content.as_deref(),
                Some("visible"),
                "{record_type}"
            );
            assert_eq!(results[0].call_id, Some("call-visible"), "{record_type}");
            assert_eq!(results[1].content, None, "{record_type}");

            let single = json!({
                "type": record_type,
                "toolCalls": [{"result": {"content": "reopened exactly"}}]
            });
            let single_results =
                enumerate_native_jsonl_result_subrecords(TABNINE_RESULT_PROFILE, &single).unwrap();
            assert_eq!(single_results.len(), 1, "{record_type}");
            assert_eq!(
                single_results[0].content.as_deref(),
                Some("reopened exactly"),
                "{record_type}"
            );

            let entirely_redacted = json!({
                "redacted": true,
                "type": record_type,
                "toolCalls": [
                    {"result": {"content": "secret-zero"}},
                    {"result": {"content": "secret-one"}}
                ]
            });
            let redacted = enumerate_native_jsonl_result_subrecords(
                TABNINE_RESULT_PROFILE,
                &entirely_redacted,
            )
            .unwrap();
            assert_eq!(redacted.len(), 2, "{record_type}");
            assert!(redacted.iter().all(|result| result.content.is_none()));
        }
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
        assert_eq!(results[0].content.as_deref(), Some("visible-zero"));
        assert_eq!(results[1].content, None);
        assert_eq!(results[1].outcome.outcome, OutputOutcome::Unknown);
        assert_eq!(results[2].subrecord_index, 2);
        assert_eq!(results[2].content.as_deref(), Some("visible-two"));

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
}
