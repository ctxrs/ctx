use super::*;

pub(super) fn parsed_from_value(
    value: &Value,
    raw_ordinal: u64,
    locator: &ClaudePhysicalLocator,
    result: ResultClassification,
    core_outputs: &[ParsedClaudeOutput],
    outputs: Vec<ParsedClaudeOutput>,
) -> ParsedClaudeRecord {
    let safe = safe_record_from_value(value, result);
    let native_record_id = safe
        .uuid
        .clone()
        .or_else(|| safe.message.as_ref().and_then(|message| message.id.clone()));
    let session_id = safe.session_id.clone();
    let timestamp = safe.timestamp.clone();
    let cwd = safe.cwd.clone();
    let version = safe.version.clone();
    let git_branch = safe.git_branch.clone();
    let mut rows = retain_safe_record(safe, raw_ordinal, locator, result.is_result());
    let sparse_base = u64::try_from(rows.len()).unwrap_or(u64::MAX);
    rows.extend(sparse_output_rows(
        raw_ordinal,
        locator,
        native_record_id.clone(),
        timestamp.clone(),
        sparse_base,
        core_outputs,
    ));
    ParsedClaudeRecord {
        result,
        preallocation_exclusion: false,
        native_record_id,
        session_id,
        timestamp,
        cwd,
        version,
        git_branch,
        rows,
        outputs,
    }
}

fn safe_record_from_value(value: &Value, result: ResultClassification) -> SafeRecord {
    let message_value = value.get("message");
    let message = message_value
        .and_then(Value::as_object)
        .map(|message| SafeMessage {
            id: bounded_value_string(message.get("id"), MAX_CLASSIFICATION_METADATA_BYTES),
            role: bounded_value_string(message.get("role"), MAX_CLASSIFICATION_METADATA_BYTES),
            content: safe_content_from_value(
                message.get("content"),
                !result.top_level_result
                    && !result.tagged_command_output
                    && (!result.result_like_shape || result.result_block),
            ),
        });
    SafeRecord {
        entry_type: bounded_value_string(value.get("type"), MAX_CLASSIFICATION_METADATA_BYTES),
        uuid: bounded_value_string(value.get("uuid"), MAX_CLASSIFICATION_METADATA_BYTES),
        session_id: bounded_value_string(value.get("sessionId"), MAX_CLASSIFICATION_METADATA_BYTES),
        parent_uuid: bounded_value_string(
            value.get("parentUuid"),
            MAX_CLASSIFICATION_METADATA_BYTES,
        ),
        role: bounded_value_string(value.get("role"), MAX_CLASSIFICATION_METADATA_BYTES),
        timestamp: bounded_value_string(value.get("timestamp"), MAX_CLASSIFICATION_METADATA_BYTES),
        cwd: bounded_value_string(value.get("cwd"), MAX_CLASSIFICATION_METADATA_BYTES),
        version: bounded_value_string(value.get("version"), MAX_CLASSIFICATION_METADATA_BYTES),
        git_branch: bounded_value_string(value.get("gitBranch"), MAX_CLASSIFICATION_METADATA_BYTES),
        message,
        content: safe_content_from_value(
            value.get("content"),
            !result.top_level_result
                && !result.tagged_command_output
                && (!result.result_like_shape || result.result_block),
        ),
        summary: bounded_value_string(value.get("summary"), 8 * 1024 * 1024),
    }
}

fn safe_content_from_value(value: Option<&Value>, retain_direct_text: bool) -> SafeContent {
    if !retain_direct_text {
        return SafeContent::default();
    }
    match value {
        Some(Value::String(text)) if retain_direct_text => SafeContent {
            direct_text: Some(text.clone()),
            blocks: Vec::new(),
        },
        Some(Value::Array(blocks)) => SafeContent {
            direct_text: None,
            blocks: blocks
                .iter()
                .take(super::super::rows::CLAUDE_MAX_RECORD_ROWS + 1)
                .filter_map(Value::as_object)
                .map(|block| SafeBlock {
                    kind: bounded_value_string(
                        block.get("type"),
                        MAX_CLASSIFICATION_METADATA_BYTES,
                    ),
                    text: match block.get("type").and_then(Value::as_str) {
                        Some("text") => bounded_value_string(block.get("text"), 8 * 1024 * 1024),
                        _ => None,
                    },
                    id: bounded_value_string(block.get("id"), MAX_CLASSIFICATION_METADATA_BYTES),
                    name: bounded_value_string(
                        block.get("name"),
                        MAX_CLASSIFICATION_METADATA_BYTES,
                    ),
                    input: safe_tool_input_from_value(block.get("input")),
                })
                .collect(),
        },
        _ => SafeContent::default(),
    }
}

fn safe_tool_input_from_value(value: Option<&Value>) -> SafeToolInput {
    let Some(object) = value.and_then(Value::as_object) else {
        return SafeToolInput::default();
    };
    SafeToolInput {
        path: bounded_path_from_value(object.get("path")),
        file_path: bounded_path_from_value(
            object.get("file_path").or_else(|| object.get("filePath")),
        ),
        old_path: bounded_path_from_value(object.get("old_path").or_else(|| object.get("oldPath"))),
        new_path: bounded_path_from_value(object.get("new_path").or_else(|| object.get("newPath"))),
        command: bounded_patch_from_value(object.get("command")),
        patch: bounded_patch_from_value(object.get("patch")),
    }
}

fn bounded_path_from_value(value: Option<&Value>) -> Option<BoundedPath> {
    Some(BoundedPath(Some(bounded_value_string(value, 4 * 1024)?)))
}

fn bounded_patch_from_value(value: Option<&Value>) -> Option<BoundedPatch> {
    bounded_value_string(value, 64 * 1024).map(|value| BoundedPatch(Some(value)))
}

fn bounded_value_string(value: Option<&Value>, max_bytes: usize) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| value.len() <= max_bytes)
        .map(str::to_owned)
}

pub(super) fn value_output_descriptors(
    value: &Value,
    result: ResultClassification,
    record_outcome: &OutputOutcomeMetadata,
) -> Vec<ParsedClaudeOutput> {
    let message = value.get("message").unwrap_or(value);
    let top_result = value
        .get("toolUseResult")
        .or_else(|| message.get("toolUseResult"));
    let mut outputs = Vec::new();
    if let Some(blocks) = message.get("content").and_then(Value::as_array) {
        for block in blocks {
            if outputs.len() > super::super::rows::CLAUDE_MAX_RECORD_ROWS {
                break;
            }
            let Some(object) = block.as_object() else {
                continue;
            };
            let kind = object.get("type").and_then(Value::as_str);
            let is_result = kind.is_some_and(is_result_label)
                || object.keys().any(|key| is_result_shape_label(key));
            if is_result {
                let content = object
                    .get("content")
                    .or_else(|| object.get("text"))
                    .and_then(provider_explicit_result_value_text)
                    .unwrap_or_default()
                    .into_bytes();
                outputs.push(ParsedClaudeOutput {
                    subrecord_index: u32::try_from(outputs.len()).unwrap_or(u32::MAX),
                    call_id: bounded_value_string(
                        object
                            .get("tool_use_id")
                            .or_else(|| object.get("toolUseId")),
                        256,
                    ),
                    outcome: record_outcome.clone(),
                    content: Some(content),
                });
                if outputs.len() > super::super::rows::CLAUDE_MAX_RECORD_ROWS {
                    break;
                }
            }
            for (key, candidate) in object {
                if matches!(
                    key.as_str(),
                    "type"
                        | "content"
                        | "text"
                        | "tool_use_id"
                        | "toolUseId"
                        | "is_error"
                        | "isError"
                ) || !is_result_label(key)
                {
                    continue;
                }
                outputs.push(ParsedClaudeOutput {
                    subrecord_index: u32::try_from(outputs.len()).unwrap_or(u32::MAX),
                    call_id: None,
                    outcome: record_outcome.clone(),
                    content: Some(
                        provider_explicit_result_value_text(candidate)
                            .unwrap_or_default()
                            .into_bytes(),
                    ),
                });
                if outputs.len() > super::super::rows::CLAUDE_MAX_RECORD_ROWS {
                    break;
                }
            }
        }
    }
    if outputs.is_empty()
        && value
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(is_result_label)
    {
        let content = value
            .get("content")
            .or_else(|| value.get("output"))
            .or_else(|| value.get("result"))
            .and_then(provider_explicit_result_value_text)
            .unwrap_or_default()
            .into_bytes();
        outputs.push(ParsedClaudeOutput {
            subrecord_index: 0,
            call_id: bounded_value_string(
                value.get("tool_use_id").or_else(|| value.get("toolUseId")),
                256,
            ),
            outcome: record_outcome.clone(),
            content: Some(content),
        });
    }
    if outputs.is_empty() {
        if let Some(object) = value.as_object() {
            for (key, candidate) in object {
                if matches!(
                    key.as_str(),
                    "type"
                        | "content"
                        | "output"
                        | "result"
                        | "toolUseResult"
                        | "tool_use_id"
                        | "toolUseId"
                        | "is_error"
                        | "isError"
                ) || !is_result_label(key)
                {
                    continue;
                }
                outputs.push(ParsedClaudeOutput {
                    subrecord_index: u32::try_from(outputs.len()).unwrap_or(u32::MAX),
                    call_id: None,
                    outcome: record_outcome.clone(),
                    content: Some(
                        provider_explicit_result_value_text(candidate)
                            .unwrap_or_default()
                            .into_bytes(),
                    ),
                });
                if outputs.len() > super::super::rows::CLAUDE_MAX_RECORD_ROWS {
                    break;
                }
            }
        }
    }
    if outputs.is_empty() {
        if let Some(top_result) = top_result {
            outputs.push(ParsedClaudeOutput {
                subrecord_index: 0,
                call_id: None,
                outcome: record_outcome.clone(),
                content: Some(
                    tool_use_result_text(top_result)
                        .unwrap_or_default()
                        .into_bytes(),
                ),
            });
        } else if result.tagged_command_output {
            let content = message
                .get("content")
                .and_then(provider_explicit_result_value_text)
                .unwrap_or_default();
            outputs.push(ParsedClaudeOutput {
                subrecord_index: 0,
                call_id: None,
                outcome: record_outcome.clone(),
                content: Some(content.into_bytes()),
            });
        } else if result.is_result() {
            outputs.push(ParsedClaudeOutput {
                subrecord_index: 0,
                call_id: None,
                outcome: record_outcome.clone(),
                content: Some(Vec::new()),
            });
        }
    }
    outputs
}

fn tool_use_result_text(value: &Value) -> Option<String> {
    let Some(object) = value.as_object() else {
        return provider_explicit_result_value_text(value);
    };
    let streams = ["stdout", "stderr"]
        .into_iter()
        .filter_map(|key| {
            object
                .get(key)
                .and_then(provider_explicit_result_value_text)
        })
        .collect::<Vec<_>>();
    if !streams.is_empty() {
        return Some(streams.join("\n"));
    }
    ["output", "content", "result"].into_iter().find_map(|key| {
        object
            .get(key)
            .and_then(provider_explicit_result_value_text)
    })
}

pub(super) fn sparse_output_rows(
    raw_ordinal: u64,
    locator: &ClaudePhysicalLocator,
    native_record_id: Option<String>,
    timestamp: Option<String>,
    core_subrecord_base: u64,
    outputs: &[ParsedClaudeOutput],
) -> Vec<ClaudeRetainedRow> {
    outputs
        .iter()
        .filter_map(|output| {
            let outcome = match output.outcome.outcome {
                OutputOutcome::Failure => ClaudeOutputOutcome::Failure,
                OutputOutcome::Timeout => ClaudeOutputOutcome::Timeout,
                OutputOutcome::Success | OutputOutcome::Unknown => return None,
            };
            let subrecord_index =
                core_subrecord_base.saturating_add(u64::from(output.subrecord_index));
            let identity = identity(raw_ordinal, subrecord_index);
            Some(ClaudeRetainedRow {
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
                complete_body_ref: None,
                tool_call: None,
                sparse_output: Some(ClaudeSparseOutputDiagnostic {
                    call_id: output.call_id.clone(),
                    outcome,
                    exit_code: output.outcome.exit_code,
                    duration_ms: output.outcome.duration_ms,
                }),
                locator: locator.clone(),
            })
        })
        .collect()
}
