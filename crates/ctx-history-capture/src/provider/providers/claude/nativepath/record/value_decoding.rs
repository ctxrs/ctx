use super::super::rows::ClaudeDiscoveryResultEvidence;
use super::*;

pub(super) fn complete_output_rows(
    raw_ordinal: u64,
    locator: &ClaudePhysicalLocator,
    native_record_id: Option<String>,
    timestamp: Option<String>,
    outputs: &[ClaudeOutputDescriptor],
    value: &Value,
) -> Vec<ClaudeRetainedRow> {
    let blocks = value
        .pointer("/message/content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let result_blocks = blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        .collect::<Vec<_>>();
    let mut consumed_result_blocks = vec![false; result_blocks.len()];
    let message = value.get("message").unwrap_or(value);
    let direct_content = message
        .get("content")
        .filter(|_| result_blocks.is_empty() && outputs.len() == 1);
    let direct_call_id = ["tool_use_id", "toolUseId", "toolCallId"]
        .into_iter()
        .find_map(|key| message.get(key).and_then(Value::as_str))
        .filter(|call_id| !call_id.is_empty() && call_id.len() <= 256);
    let tool_use_result = value
        .get("toolUseResult")
        .or_else(|| message.get("toolUseResult"));
    let one_output = outputs.len() == 1;
    outputs
        .iter()
        .map(|output| {
            let block = output
                .call_id
                .as_deref()
                .and_then(|call_id| {
                    result_blocks.iter().enumerate().find(|(index, block)| {
                        !consumed_result_blocks[*index]
                            && block.get("tool_use_id").and_then(Value::as_str) == Some(call_id)
                    })
                })
                .or_else(|| {
                    output
                        .call_id
                        .is_none()
                        .then(|| {
                            result_blocks
                                .iter()
                                .enumerate()
                                .find(|(index, _)| !consumed_result_blocks[*index])
                        })
                        .flatten()
                })
                .map(|(index, block)| {
                    consumed_result_blocks[index] = true;
                    *block
                });
            let outcome = match output.outcome.outcome {
                OutputOutcome::Success => ClaudeOutputOutcome::Success,
                OutputOutcome::Failure => ClaudeOutputOutcome::Failure,
                OutputOutcome::Timeout => ClaudeOutputOutcome::Timeout,
                OutputOutcome::Unknown => ClaudeOutputOutcome::Unknown,
            };
            let identity = identity(raw_ordinal, u64::from(output.subrecord_index));
            let content = block
                .and_then(|block| block.get("content"))
                .cloned()
                .or_else(|| output.content.clone())
                .or_else(|| direct_content.cloned())
                .unwrap_or(Value::Null);
            let discovery_evidence = if !one_output && tool_use_result.is_some() {
                // A record-level envelope cannot be assigned to one member of
                // a multi-result aggregate without guessing.
                ClaudeDiscoveryResultEvidence::Unknown
            } else {
                claude_discovery_result_evidence(block, tool_use_result, &content)
            };
            ClaudeRetainedRow {
                identity,
                native_order: order(identity),
                native_record_id: native_record_id.clone(),
                parent_native_record_id: None,
                kind: ClaudeEventKind::ToolOutput,
                role: Some("tool".to_owned()),
                occurred_at: timestamp.clone(),
                body: None,
                body_sha256: None,
                body_text_retention: None,
                tool_call: None,
                tool_result: Some(ClaudeToolResult {
                    call_id: output
                        .call_id
                        .clone()
                        .or_else(|| direct_call_id.map(str::to_owned)),
                    outcome,
                    exit_code: output.outcome.exit_code,
                    duration_ms: output.outcome.duration_ms,
                    content,
                    tool_use_result: tool_use_result.cloned(),
                    discovery_evidence,
                }),
                locator: locator.clone(),
            }
        })
        .collect()
}

fn claude_discovery_result_evidence(
    block: Option<&Value>,
    tool_use_result: Option<&Value>,
    selected_content: &Value,
) -> ClaudeDiscoveryResultEvidence {
    let Some(block) = block.and_then(Value::as_object) else {
        return ClaudeDiscoveryResultEvidence::Unknown;
    };
    if block.keys().any(|key| is_diagnostic_key(key)) {
        return ClaudeDiscoveryResultEvidence::Diagnostic;
    }
    if block.len() != 4
        || block.get("type").and_then(Value::as_str) != Some("tool_result")
        || block.get("tool_use_id").and_then(Value::as_str).is_none()
        || block.get("content") != Some(selected_content)
    {
        return ClaudeDiscoveryResultEvidence::Unknown;
    }
    match block.get("is_error").and_then(Value::as_bool) {
        Some(true) => return ClaudeDiscoveryResultEvidence::Failed,
        Some(false) => {}
        None => return ClaudeDiscoveryResultEvidence::Unknown,
    }
    if !payload_is_present(selected_content) {
        return ClaudeDiscoveryResultEvidence::Unknown;
    }
    let Some(tool_use_result) = tool_use_result else {
        return ClaudeDiscoveryResultEvidence::SuccessfulPayloadOnly;
    };
    let Some(envelope) = tool_use_result.as_object() else {
        return ClaudeDiscoveryResultEvidence::Unknown;
    };
    if envelope.keys().any(|key| {
        matches!(
            key.as_str(),
            "warning" | "warnings" | "error" | "errors" | "diagnostic" | "diagnostics"
        )
    }) {
        return ClaudeDiscoveryResultEvidence::Diagnostic;
    }
    if envelope.len() != 5
        || ![
            "stdout",
            "stderr",
            "interrupted",
            "isImage",
            "noOutputExpected",
        ]
        .into_iter()
        .all(|key| envelope.contains_key(key))
    {
        return ClaudeDiscoveryResultEvidence::Unknown;
    }
    let Some(stdout) = envelope.get("stdout").and_then(Value::as_str) else {
        return ClaudeDiscoveryResultEvidence::Unknown;
    };
    let Some(stderr) = envelope.get("stderr").and_then(Value::as_str) else {
        return ClaudeDiscoveryResultEvidence::Unknown;
    };
    let (Some(interrupted), Some(is_image), Some(no_output_expected)) = (
        envelope.get("interrupted").and_then(Value::as_bool),
        envelope.get("isImage").and_then(Value::as_bool),
        envelope.get("noOutputExpected").and_then(Value::as_bool),
    ) else {
        return ClaudeDiscoveryResultEvidence::Unknown;
    };
    if interrupted {
        return ClaudeDiscoveryResultEvidence::Failed;
    }
    if !stderr.is_empty() {
        return ClaudeDiscoveryResultEvidence::Diagnostic;
    }
    if is_image || no_output_expected || selected_content.as_str() != Some(stdout) {
        return ClaudeDiscoveryResultEvidence::Unknown;
    }
    ClaudeDiscoveryResultEvidence::SuccessfulPayloadOnly
}

fn payload_is_present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(value) => !value.is_empty(),
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
        Value::Bool(_) | Value::Number(_) => true,
    }
}

fn is_diagnostic_key(key: &str) -> bool {
    matches!(
        key,
        "stderr" | "warning" | "warnings" | "error" | "errors" | "diagnostic" | "diagnostics"
    )
}
