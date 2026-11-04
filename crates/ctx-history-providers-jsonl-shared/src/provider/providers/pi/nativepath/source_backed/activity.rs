use super::super::super::text::pi_result_value_text;
use super::*;
use ctx_history_core::{
    ActivityInvocation, ActivityJsonCapture, ActivityResult, ActivityTextCapture, CoreActivity,
    CORE_ACTIVITY_REVISION,
};

/// One Core activity owns one execution. Split parallel tool calls while
/// retaining ordinary message content with the first call and the native entry ID.
pub(super) fn split_tool_calls(mut value: Value) -> Result<Vec<(u32, Value)>> {
    let Some(message) = value.get_mut("message") else {
        return Ok(vec![(0, value)]);
    };
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return Ok(vec![(0, value)]);
    }
    let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) else {
        return Ok(vec![(0, value)]);
    };
    if blocks.iter().filter(|block| is_tool_call(block)).count() < 2 {
        return Ok(vec![(0, value)]);
    }
    let mut ids = BTreeSet::new();
    if blocks
        .iter()
        .filter(|block| is_tool_call(block))
        .filter_map(|block| block.get("id").and_then(Value::as_str))
        .any(|id| !ids.insert(id))
    {
        // Keep the complete ambiguous message without choosing one execution
        // for a result carrying the duplicated native ID.
        return Ok(vec![(0, value)]);
    }
    let mut first = Vec::new();
    let mut later = Vec::new();
    let mut seen_call = false;
    for block in std::mem::take(blocks) {
        if is_tool_call(&block) {
            if seen_call {
                later.push(block);
                continue;
            }
            seen_call = true;
        }
        first.push(block);
    }
    let template = value.clone();
    value["message"]["content"] = Value::Array(first);
    let mut entries = vec![(0, value)];
    for (index, call) in later.into_iter().enumerate() {
        let mut entry = template.clone();
        entry["message"]["content"] = Value::Array(vec![call]);
        let part = u32::try_from(index + 1).map_err(contract)?;
        entries.push((part, entry));
    }
    Ok(entries)
}

fn is_tool_call(value: &Value) -> bool {
    value.get("type").and_then(Value::as_str) == Some("toolCall")
}

pub(super) fn pi_activity(
    message: &Value,
    event_type: EventType,
    mut facts: Vec<ProviderDeclaredFact>,
) -> Result<Option<CoreActivity>> {
    let mut calls = message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|block| is_tool_call(block));
    let (call, id_fields): (_, &[&str]) = if event_type == EventType::ToolCall {
        match (calls.next(), calls.next()) {
            (Some(call), None) => (call, &["id"]),
            (Some(_), Some(_)) => (message, &[]),
            _ => (message, &["toolCallId", "tool_call_id", "callId"]),
        }
    } else {
        (message, &["toolCallId", "tool_call_id", "callId"])
    };
    let call_ids = id_fields
        .iter()
        .filter_map(|field| call.get(field).and_then(Value::as_str))
        .collect::<Vec<_>>();
    let provider_call_id = match call_ids.as_slice() {
        [id] => admit_optional_provider_call_id(Some((*id).to_owned())),
        _ => None,
    };
    let tools = ["toolName", "tool_name", "name"]
        .into_iter()
        .filter_map(|field| call.get(field).and_then(Value::as_str))
        .collect::<Vec<_>>();
    let invocation = if provider_call_id.is_some() && event_type == EventType::ToolCall {
        match tools.as_slice() {
            [tool] => admit_optional_metadata_text(Some((*tool).to_owned())).map(|tool| {
                ActivityInvocation {
                    protocol: None,
                    server: None,
                    tool,
                    arguments: call
                        .get("arguments")
                        .map_or(ActivityJsonCapture::Absent, |value| {
                            ActivityJsonCapture::Present {
                                value: value.clone(),
                            }
                        }),
                    started_at_unix_ms: None,
                }
            }),
            _ => None,
        }
    } else {
        None
    };
    if invocation
        .as_ref()
        .is_some_and(|invocation| invocation.tool == "bash")
    {
        if let Some(command) = call.pointer("/arguments/command").and_then(Value::as_str) {
            if let Some(fact) = admit_provider_declared_fact(
                LiteralFactKind::Command,
                command.to_owned(),
                facts.len(),
            ) {
                facts.push(fact);
            }
        }
    }
    let result = (provider_call_id.is_some()
        && matches!(event_type, EventType::ToolOutput | EventType::CommandOutput))
    .then(|| ActivityResult {
        status: message
            .get("isError")
            .and_then(Value::as_bool)
            .map(|is_error| if is_error { "error" } else { "success" }.to_owned()),
        completed_at_unix_ms: None,
        duration_ns: None,
        text: match event_type {
            EventType::ToolOutput => message.get("content").and_then(pi_result_value_text),
            EventType::CommandOutput => message
                .get("output")
                .and_then(Value::as_str)
                .map(str::to_owned),
            _ => None,
        }
        .map_or(ActivityTextCapture::Absent, |value| {
            ActivityTextCapture::Present { value }
        }),
        // This activity field is absent; it does not mark the native result
        // envelope unavailable. Core structured_content retains that envelope,
        // and result.text holds its selected text blocks.
        structured_content: ActivityJsonCapture::Absent,
    });
    if invocation.is_none() && result.is_none() && facts.is_empty() {
        return Ok(None);
    }
    Ok(Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id,
        invocation,
        result,
        facts,
    }))
}
