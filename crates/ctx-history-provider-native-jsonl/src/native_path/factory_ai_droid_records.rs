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
) -> (
    String,
    Option<String>,
    Option<String>,
    Option<AgentScope>,
    Option<ProviderNativeSessionRelationship>,
) {
    let parent = header
        .get("parent")
        .or_else(|| header.get("callingSessionId"))
        .and_then(Value::as_str)
        .filter(|session_id| !session_id.trim().is_empty())
        .map(str::to_owned);
    let agent_scope = if parent.is_some()
        || header.get("decompSessionType").and_then(Value::as_str) == Some("worker")
    {
        Some(AgentScope::Subagent)
    } else {
        Some(AgentScope::Primary)
    };
    // A malformed optional parent must not invalidate the child's own records.
    let parent = parent.filter(|parent| {
        parent != native_session_id
            && ctx_history_core::TypedKey::utf8(parent.as_str())
                .and_then(|key| key.validate_contract())
                .is_ok()
    });
    let relationship = parent
        .as_ref()
        .map(|_| ProviderNativeSessionRelationship::Delegated);
    (
        native_session_id.to_owned(),
        parent,
        header
            .get("decompMissionId")
            .and_then(Value::as_str)
            .filter(|mission_id| !mission_id.trim().is_empty())
            .map(str::to_owned),
        agent_scope,
        relationship,
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
    let results = if reject_redacted(value).is_err() {
        placeholder_results(content)?
    } else {
        enumerate_content_results(content)?
    };
    for result in &results {
        factory_droid_retry_discriminator(value, result.subrecord_index)?;
    }
    Ok(results)
}

pub(crate) fn factory_droid_retry_discriminator(
    value: &Value,
    subrecord_index: u32,
) -> std::result::Result<
    Option<super::super::DirectJsonlRetryDiscriminator>,
    NativeJsonlResultExtractionError,
> {
    let Some(message_id) = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
    else {
        return Ok(None);
    };
    let Some(parent_id) = value
        .get("parentId")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
    else {
        return Ok(None);
    };
    if parent_id != message_id {
        return Ok(None);
    }
    let index = usize::try_from(subrecord_index)
        .map_err(|_| NativeJsonlResultExtractionError::InvalidShape)?;
    let result = value
        .get("content")
        .or_else(|| value.pointer("/message/content"))
        .and_then(Value::as_array)
        .and_then(|content| content.get(index))
        .filter(|result| result.get("type").and_then(Value::as_str) == Some("tool_result"))
        .ok_or(NativeJsonlResultExtractionError::InvalidShape)?;
    let tool_use_id = result
        .get("tool_use_id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or(NativeJsonlResultExtractionError::InvalidShape)?;
    Ok(Some(
        super::super::DirectJsonlRetryDiscriminator::FactoryDroidToolResult {
            tool_use_id: tool_use_id.to_owned(),
        },
    ))
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
    let Some(content) = content else {
        return Ok(Vec::new());
    };
    content
        .as_array()
        .ok_or(NativeJsonlResultExtractionError::InvalidShape)?
        .iter()
        .enumerate()
        .filter_map(|(index, block)| {
            (block.get("type").and_then(Value::as_str) == Some("tool_result")).then_some(index)
        })
        .map(|index| {
            Ok(NativeJsonlResultSubrecord {
                subrecord_index: u32::try_from(index)
                    .map_err(|_| NativeJsonlResultExtractionError::InvalidShape)?,
                content: None,
                call_id: None,
                tool_name: None,
            })
        })
        .collect()
}

fn enumerate_content_results<'a>(
    content: Option<&'a Value>,
) -> std::result::Result<Vec<NativeJsonlResultSubrecord<'a>>, NativeJsonlResultExtractionError> {
    let Some(content) = content else {
        return Ok(Vec::new());
    };
    content
        .as_array()
        .ok_or(NativeJsonlResultExtractionError::InvalidShape)?
        .iter()
        .enumerate()
        .filter(|(_, block)| block.get("type").and_then(Value::as_str) == Some("tool_result"))
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
            })
        })
        .collect()
}

fn extract_result_ref<'a>(
    value: Option<&'a Value>,
    object_fields: &[&str],
) -> std::result::Result<Option<std::borrow::Cow<'a, str>>, NativeJsonlResultExtractionError> {
    extract_direct_result_content(value, object_fields, true)
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
