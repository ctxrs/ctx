use ctx_history_core::CaptureProvider;
use serde_json::Value;

pub(crate) const GEMINI_RESULT_PROFILE: &str = "gemini-jsonl.result-body.v1";
pub(crate) const TABNINE_RESULT_PROFILE: &str = "tabnine.result-body.v1";
pub(crate) const FACTORY_DROID_RESULT_PROFILE: &str = "factory-droid.result-body.v1";
pub(crate) const CURSOR_RESULT_PROFILE: &str = "cursor-jsonl.result-body.v1";
pub(crate) const QODER_RESULT_PROFILE: &str = "qoder.result-body.v1";
pub(crate) const COPILOT_CLI_RESULT_PROFILE: &str = "copilot-cli.result-body.v1";
pub(crate) const QWEN_CODE_RESULT_PROFILE: &str = "qwen-code.result-body.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeJsonlResultExtractionError {
    UnsupportedProfile,
    Ambiguous,
    Redacted,
    InvalidShape,
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
        CaptureProvider::Cursor => Some(CURSOR_RESULT_PROFILE),
        CaptureProvider::Qoder => Some(QODER_RESULT_PROFILE),
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
            | CURSOR_RESULT_PROFILE
            | QODER_RESULT_PROFILE
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
        CURSOR_RESULT_PROFILE => extract_content_block_result(
            value
                .pointer("/message/content")
                .or_else(|| value.get("content")),
        ),
        QODER_RESULT_PROFILE => extract_qoder_result(value),
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

fn extract_qoder_result(value: &Value) -> Result<Option<String>, NativeJsonlResultExtractionError> {
    if value.get("toolUseResult").is_some() {
        return extract_direct_result(value.get("toolUseResult"), &["content", "output", "text"]);
    }
    if value.get("type").and_then(Value::as_str) != Some("user") {
        return Ok(None);
    }
    if let Some(result) = extract_content_block_result(value.pointer("/message/content"))? {
        return Ok(Some(result));
    }
    if let Some(data) = value.get("data") {
        reject_redacted(data)?;
    }
    extract_direct_result(value.pointer("/data/content"), &[])
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
            (CaptureProvider::Cursor, CURSOR_RESULT_PROFILE),
            (CaptureProvider::Qoder, QODER_RESULT_PROFILE),
            (CaptureProvider::CopilotCli, COPILOT_CLI_RESULT_PROFILE),
            (CaptureProvider::QwenCode, QWEN_CODE_RESULT_PROFILE),
        ] {
            assert_eq!(
                native_jsonl_result_content_profile(provider),
                Some(expected)
            );
        }
        assert_eq!(
            native_jsonl_result_content_profile(CaptureProvider::Codex),
            None
        );
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
                CURSOR_RESULT_PROFILE,
                json!({"role":"user","message":{"content":[{"type":"tool_result","content":"cursor result"}]}}),
                "cursor result",
            ),
            (
                QODER_RESULT_PROFILE,
                json!({"type":"user","toolUseResult":"qoder result","message":{"content":[{"type":"tool_result","content":"lower priority"}]}}),
                "qoder result",
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
            CURSOR_RESULT_PROFILE,
            QODER_RESULT_PROFILE,
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
        assert_eq!(
            extract_native_jsonl_result_content(
                CURSOR_RESULT_PROFILE,
                &json!({"message":{"content":[{"type":"tool_result","content":"one"},{"type":"tool_result","content":"two"}]}}),
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
