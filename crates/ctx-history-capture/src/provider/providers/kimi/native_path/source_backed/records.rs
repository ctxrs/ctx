use super::*;

struct KimiOutputClassification {
    kind: OutputObservationKind,
}

pub(super) fn core_record(
    compound: &KimiCompoundObservation,
    session_id: StableEntityId,
    ordinal: u64,
    value: &Value,
    fallback_timestamp: DateTime<Utc>,
) -> KimiSourceBackedResult<Option<CoreRecord>> {
    let record_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let Some((event_type, body)) =
        kimi_lexical_body(value, ordinal, compound.native.session.cwd.as_deref())?
    else {
        return Ok(None);
    };
    let role = kimi_event_role(record_type, value, event_type);
    let occurred_at =
        kimi_record_timestamp(value, fallback_timestamp).unwrap_or(fallback_timestamp);
    let line_number = usize::try_from(ordinal)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or(KimiSourceBackedError::CountOverflow)?;
    let native_event_id = kimi_legacy_provider_event_hash(record_type, value, line_number);
    let event_key = NativeItemKey::certified_position(
        KIMI_NATIVE_EVENT_POSITION_KIND,
        TypedKey::U64(ordinal),
        PositionStability::AppendStable,
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source: &compound.source,
        session_id,
        logical_item_kind: KIMI_LOGICAL_EVENT_KIND,
        native_item_key: &event_key,
        subrecord_selector: None,
    })?;
    let touched_files = kimi_touched_paths(
        value,
        event_type,
        event_type_supports_structured_file_touches(event_type),
    )?;
    let parent_session_id = compound
        .native
        .session
        .parent_provider_session_id
        .as_deref()
        .map(lineage_session_identity)
        .transpose()?;
    let root_session_id = compound
        .native
        .session
        .root_provider_session_id
        .as_deref()
        .map(lineage_session_identity)
        .transpose()?
        .unwrap_or(session_id);
    let workspace = compound.native.session.cwd.clone();
    let agent_type = if compound.native.session.is_primary {
        AgentType::Primary
    } else {
        AgentType::Subagent
    };
    let event = value.get("event").unwrap_or(value);
    let structured_content = kimi_structured_content(event, event_type, touched_files);
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        root_session_id,
        compound.source.clone(),
        ordinal,
        event_type.as_str(),
        agent_type.as_str(),
        compound.native.session.is_primary,
        KIMI_SOURCE_PARSER_REVISION,
        body,
    )?;
    record.parent_session_id = parent_session_id;
    record.provider_session_id = Some(compound.native.session.provider_session_id.clone());
    record.native_event_id = Some(TypedKey::utf8(native_event_id)?);
    record.occurred_at_unix_ms = Some(occurred_at.timestamp_millis());
    record.role = Some(role.as_str().to_owned());
    record.workspace = workspace;
    record.cwd = compound.native.session.cwd.clone();
    record.content.structured_content = structured_content;
    record.validate_contract()?;
    Ok(Some(record))
}

fn kimi_structured_content(
    event: &Value,
    event_type: EventType,
    touched_files: Vec<String>,
) -> Option<Value> {
    let tool_name = event
        .get("toolName")
        .or_else(|| event.get("tool_name"))
        .or_else(|| event.get("name"))
        .cloned();
    let call_id = event
        .get("toolCallId")
        .or_else(|| event.get("callId"))
        .or_else(|| event.get("call_id"))
        .or_else(|| event.get("id"))
        .cloned();
    let provider_tool_content = matches!(
        event_type,
        EventType::ToolCall | EventType::ToolOutput | EventType::CommandOutput
    )
    .then(|| event.clone());
    (!touched_files.is_empty()
        || tool_name.is_some()
        || call_id.is_some()
        || provider_tool_content.is_some())
    .then(|| {
        serde_json::json!({
            "tool_name": tool_name,
            "call_id": call_id,
            "file_touches": touched_files,
            "provider_tool_content": provider_tool_content,
        })
    })
}

pub(super) fn kimi_lexical_body(
    value: &Value,
    _ordinal: u64,
    _cwd: Option<&str>,
) -> KimiSourceBackedResult<Option<(EventType, String)>> {
    let record_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut event_type = kimi_event_type(record_type, value);
    let body = if event_type == EventType::ToolOutput {
        let output = kimi_output_classification(value);
        if output.kind == OutputObservationKind::Command {
            event_type = EventType::CommandOutput;
        }
        kimi_output_content(value).unwrap_or_default()
    } else {
        kimi_event_text(record_type, value, event_type)
    };
    if body.is_empty() {
        return Ok(None);
    }
    Ok(Some((event_type, body)))
}

fn kimi_touched_paths(
    value: &Value,
    event_type: EventType,
    include_structured_touches: bool,
) -> KimiSourceBackedResult<Vec<String>> {
    if !matches!(
        event_type,
        EventType::ToolCall
            | EventType::ToolOutput
            | EventType::CommandOutput
            | EventType::FileTouched
    ) {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    visit_provider_file_touch_drafts_with_limit(
        value,
        include_structured_touches,
        MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
        |(_, draft)| {
            paths.push(draft.path);
            Ok::<(), CaptureError>(())
        },
    )?;
    Ok(paths)
}

fn kimi_output_classification(value: &Value) -> KimiOutputClassification {
    let event = value.get("event").unwrap_or(value);
    let tool_name = event
        .get("toolName")
        .or_else(|| event.get("tool_name"))
        .or_else(|| event.get("name"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("tool");
    let kind = if tool_input::is_command_tool(&tool_name.to_ascii_lowercase()) {
        OutputObservationKind::Command
    } else {
        OutputObservationKind::Tool
    };
    KimiOutputClassification { kind }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_tool_call_id_is_published_for_call_and_result() {
        let call = serde_json::json!({
            "type": "tool.call",
            "toolCallId": "call_1",
            "name": "Read",
            "args": {"path": "/tmp/splines.txt"}
        });
        let result = serde_json::json!({
            "type": "tool.result",
            "toolCallId": "call_1",
            "result": {"output": "spline data", "isError": false}
        });

        for (event, event_type) in [
            (&call, EventType::ToolCall),
            (&result, EventType::ToolOutput),
        ] {
            let structured = kimi_structured_content(event, event_type, Vec::new()).unwrap();
            assert_eq!(
                structured.get("call_id").and_then(Value::as_str),
                Some("call_1"),
                "{structured:#}"
            );
        }
    }

    #[test]
    fn successful_textual_result_over_16k_is_complete() {
        let tail = "kimi_success_result_tail_complete";
        let output = format!("{} {tail}", "successful kimi output ".repeat(800));
        assert!(output.len() > 16_000);
        let value = serde_json::json!({
            "type": "context.append_loop_event",
            "event": {
                "type": "tool.result",
                "toolName": "bash",
                "call_id": "complete-success",
                "exit_code": 0,
                "output": output,
            }
        });

        let (event_type, body) = kimi_lexical_body(&value, 0, None).unwrap().unwrap();
        assert_eq!(event_type, EventType::CommandOutput);
        assert_eq!(body, output);
        assert!(body.ends_with(tail));
    }
}
