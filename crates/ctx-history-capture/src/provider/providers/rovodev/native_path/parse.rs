use super::*;

pub(super) fn prepare_message(
    source: &RovoDevSessionSource,
    context: &ProviderAdapterContext,
    document: &PreparedDocument,
    index: usize,
) -> Result<PreparedMessage> {
    let message = document
        .messages
        .get(index)
        .ok_or(CaptureError::SystemInvariant(
            "RovoDev NativePath message index escaped its document",
        ))?;
    let line = index.saturating_add(1);
    if !message.is_object() {
        return Ok(PreparedMessage {
            line,
            event: None,
            touches: Vec::new(),
            rejection: Some(failure(
                line,
                "Rovo Dev message_history member must be an object",
            )),
            estimated_bytes: 256,
        });
    }
    let event_index = u64::try_from(index)
        .map_err(|_| CaptureError::InvalidPayload("RovoDev event index exceeds u64".to_owned()))?;
    let occurred_at = message_timestamp(message).unwrap_or(document.started_at);
    let role_text = message
        .get("role")
        .or_else(|| message.get("kind"))
        .or_else(|| message.get("type"))
        .and_then(Value::as_str);
    let output = rovodev_event_type(message, role_text) == EventType::ToolOutput;
    let output_metadata =
        output.then(|| output_metadata(message, event_index, document.cwd.as_deref()));
    let retained_failure = output_metadata.as_ref().is_some_and(|metadata| {
        matches!(
            metadata.outcome.outcome,
            OutputOutcome::Failure | OutputOutcome::Timeout
        )
    });
    let mut event = if !output || retained_failure {
        let mut event = rovodev_event(event_index, message, occurred_at, source);
        event.metadata["source_record_ordinal"] = json!(0_u64);
        event.metadata["source_record_subrecord_index"] = json!(index);
        if let Some(metadata) = output_metadata.as_ref() {
            let content = super::rovodev_result_content(message).unwrap_or_default();
            if metadata.kind == OutputObservationKind::Command {
                event.event_type = EventType::CommandOutput;
            }
            let (preview, _) = provider_local_preview(&content, PROVIDER_MAX_PREVIEW_CHARS);
            event.payload["result_outcome"] = Value::String("failure".to_owned());
            event.payload["output_bytes"] = json!(content.len());
            event.payload["output_preview"] = Value::String(preview);
            event.payload["call_id"] = metadata
                .call_id
                .as_ref()
                .map_or(Value::Null, |value| Value::String(value.clone()));
            event.payload["exit_code"] = metadata
                .outcome
                .exit_code
                .map_or(Value::Null, |value| Value::from(i64::from(value)));
            event.payload["duration_ms"] = metadata
                .outcome
                .duration_ms
                .map_or(Value::Null, Value::from);
            event.payload["timed_out"] =
                Value::Bool(metadata.outcome.outcome == OutputOutcome::Timeout);
            if let Some(command) = &metadata.command {
                event.payload["tool"] = Value::String(command.tool_name.clone());
                event.payload["command"] = Value::String(command.command.clone());
                event.payload["cwd"] = command
                    .working_directory
                    .as_ref()
                    .map_or(Value::Null, |value| Value::String(value.clone()));
            }
        } else if let Some(complete_text) = provider_block_text(message) {
            let native_id = event.provider_event_hash.clone();
            attach_rovodev_complete_content_locator(
                &mut event,
                0,
                u32::try_from(index).map_err(|_| {
                    CaptureError::InvalidPayload("RovoDev event index exceeds u32".to_owned())
                })?,
                &native_id,
                &document.context_record,
                &complete_text,
            )?;
        }
        Some(event)
    } else {
        None
    };

    if let Some(event) = event.as_mut() {
        event.payload =
            provider_capped_json_value(&event.payload, MAX_PROVIDER_JSONL_LINE_BYTES / 4);
    }
    let source_root = context.source_root_display();
    let raw_source_path = source.context_path.display().to_string();
    let mut touches = Vec::new();
    let include_structured_touches = event
        .as_ref()
        .is_some_and(|event| event_type_supports_structured_file_touches(event.event_type));
    let event_supports_file_touches = event.as_ref().is_some_and(|event| {
        matches!(
            event.event_type,
            EventType::ToolCall
                | EventType::ToolOutput
                | EventType::CommandOutput
                | EventType::FileTouched
        )
    });
    let touch_limit_exceeded = (output || event_supports_file_touches)
        .then(|| {
            visit_provider_file_touch_drafts_with_limit(
                message,
                !output && include_structured_touches,
                MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
                |(ordinal, touch)| {
                    let provider_touch_index = if event_index > MAX_PACKED_PROVIDER_EVENT_INDEX {
                        ordinal
                    } else {
                        (event_index << 16) | ordinal
                    };
                    touches.push(RovoDevFileTouch {
                        provider_touch_index,
                        provider_event_index: Some(event_index),
                        raw_source_path: Some(raw_source_path.clone()),
                        source_root: source_root.clone(),
                        path: touch.path,
                        change_kind: touch.change_kind,
                        old_path: touch.old_path,
                        line_count_delta: None,
                        confidence: touch.confidence,
                        occurred_at,
                        metadata: touch.metadata,
                    });
                    Ok::<(), CaptureError>(())
                },
            )
        })
        .transpose()?
        .is_some_and(|outcome| outcome.limit_exceeded());
    let rejection =
        touch_limit_exceeded.then(|| failure(line, PROVIDER_FILE_TOUCH_LIMIT_REJECTION));
    let estimated_bytes = event
        .as_ref()
        .map_or(256, RovoDevCoreEvent::estimated_bytes)
        .saturating_add(
            touches
                .iter()
                .map(RovoDevFileTouch::estimated_bytes)
                .sum::<usize>(),
        )
        .saturating_add(256);
    Ok(PreparedMessage {
        line,
        event,
        touches,
        rejection,
        estimated_bytes,
    })
}

#[derive(Debug)]
pub(super) struct RovoDevOutputMetadata {
    pub(super) kind: OutputObservationKind,
    pub(super) native_record_id: String,
    pub(super) call_id: Option<String>,
    pub(super) command: Option<OutputCommandContext>,
    pub(super) outcome: OutputOutcomeMetadata,
}

pub(super) fn output_metadata(
    value: &Value,
    event_index: u64,
    session_cwd: Option<&str>,
) -> RovoDevOutputMetadata {
    let call_id = recursive_string_field(
        value,
        &[
            "call_id",
            "callId",
            "tool_call_id",
            "toolCallId",
            "tool_use_id",
            "toolUseId",
        ],
    );
    let tool_name = recursive_string_field(value, &["tool_name", "toolName", "name", "tool"])
        .unwrap_or_else(|| "tool".to_owned());
    let kind = if tool_input::is_command_tool(&tool_name.to_ascii_lowercase()) {
        OutputObservationKind::Command
    } else {
        OutputObservationKind::Tool
    };
    let command = (kind == OutputObservationKind::Command).then(|| OutputCommandContext {
        tool_name: tool_name.clone(),
        command: tool_input::command(value).unwrap_or_default(),
        working_directory: tool_input::working_directory(value)
            .or_else(|| session_cwd.map(str::to_owned)),
    });
    let timed_out = value_timed_out(value);
    let exit_code =
        i64_field(value, &["exit_code", "exitCode"]).and_then(|value| i32::try_from(value).ok());
    let duration_ms = i64_field(value, &["duration_ms", "durationMs"])
        .and_then(|value| u64::try_from(value).ok());
    let outcome = if timed_out {
        OutputOutcome::Timeout
    } else if provider_output_event_is_failure(value) {
        OutputOutcome::Failure
    } else if provider_result_outcome_evidence(EventType::ToolOutput, value).as_str()
        == Some("success")
    {
        OutputOutcome::Success
    } else {
        OutputOutcome::Unknown
    };
    RovoDevOutputMetadata {
        kind,
        native_record_id: provider_message_id(value, event_index),
        call_id,
        command,
        outcome: OutputOutcomeMetadata {
            outcome,
            exit_code,
            duration_ms,
        },
    }
}

pub(super) fn recursive_string_field(value: &Value, fields: &[&str]) -> Option<String> {
    match value {
        Value::Array(values) => values
            .iter()
            .find_map(|value| recursive_string_field(value, fields)),
        Value::Object(values) => fields
            .iter()
            .find_map(|field| values.get(*field).and_then(Value::as_str))
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .or_else(|| {
                values
                    .values()
                    .find_map(|value| recursive_string_field(value, fields))
            }),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}

pub(super) fn value_timed_out(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(value_timed_out),
        Value::Object(values) => {
            values.iter().any(|(key, value)| {
                matches!(key.as_str(), "timed_out" | "timedOut" | "timeout")
                    && value.as_bool().unwrap_or(false)
                    || matches!(key.as_str(), "status" | "state" | "outcome")
                        && value.as_str().is_some_and(|value| {
                            matches!(
                                value.trim().to_ascii_lowercase().as_str(),
                                "timeout" | "timed_out" | "timedout"
                            )
                        })
            }) || values.values().any(value_timed_out)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

pub(super) fn i64_field(value: &Value, fields: &[&str]) -> Option<i64> {
    match value {
        Value::Array(values) => values.iter().find_map(|value| i64_field(value, fields)),
        Value::Object(values) => fields
            .iter()
            .find_map(|field| values.get(*field).and_then(Value::as_i64))
            .or_else(|| values.values().find_map(|value| i64_field(value, fields))),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}

pub(super) fn message_history(value: &Value) -> Option<&Vec<Value>> {
    value
        .get("message_history")
        .or_else(|| value.pointer("/session_context/message_history"))
        .or_else(|| value.get("messages"))
        .or_else(|| value.pointer("/conversation/messages"))
        .and_then(Value::as_array)
}

pub(super) fn message_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    provider_timestamp_from_fields(
        value,
        &[
            "timestamp",
            "created_at",
            "createdAt",
            "updated_at",
            "updatedAt",
            "user_sent_time",
        ],
    )
}

#[derive(Debug, Clone, Copy)]
pub(super) enum JsonBoundsError {
    Depth,
    CollectionElements,
}

impl std::fmt::Display for JsonBoundsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Depth => write!(
                formatter,
                "exceeds maximum JSON depth of {ROVODEV_MAX_JSON_DEPTH}"
            ),
            Self::CollectionElements => write!(
                formatter,
                "exceeds JSON collection element budget of {ROVODEV_NATIVE_MAX_COLLECTION_ELEMENTS}"
            ),
        }
    }
}

pub(super) fn validate_json_bounds(value: &Value) -> std::result::Result<(), JsonBoundsError> {
    let mut stack = vec![(value, 0_usize)];
    let mut collection_elements = 0_usize;
    while let Some((value, depth)) = stack.pop() {
        if depth > ROVODEV_MAX_JSON_DEPTH {
            return Err(JsonBoundsError::Depth);
        }
        match value {
            Value::Array(values) => {
                collection_elements = collection_elements.saturating_add(values.len());
                if collection_elements > ROVODEV_NATIVE_MAX_COLLECTION_ELEMENTS {
                    return Err(JsonBoundsError::CollectionElements);
                }
                stack.extend(values.iter().map(|value| (value, depth.saturating_add(1))));
            }
            Value::Object(values) => {
                collection_elements = collection_elements.saturating_add(values.len());
                if collection_elements > ROVODEV_NATIVE_MAX_COLLECTION_ELEMENTS {
                    return Err(JsonBoundsError::CollectionElements);
                }
                stack.extend(
                    values
                        .values()
                        .map(|value| (value, depth.saturating_add(1))),
                );
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    Ok(())
}

pub(super) fn metadata_without_transcripts(value: &Value) -> Value {
    fn strip(value: &Value) -> Value {
        match value {
            Value::Array(values) => Value::Array(values.iter().map(strip).collect()),
            Value::Object(values) => Value::Object(
                values
                    .iter()
                    .filter(|(key, value)| {
                        !(value.is_array()
                            && matches!(key.as_str(), "message_history" | "messages"))
                    })
                    .map(|(key, value)| (key.clone(), strip(value)))
                    .collect(),
            ),
            _ => value.clone(),
        }
    }
    provider_capped_json_value(&strip(value), PROVIDER_MAX_PREVIEW_CHARS)
}

pub(super) fn failure(line: usize, error: impl Into<String>) -> RovoDevFailure {
    let mut error = error.into();
    if error.len() > ROVODEV_MAX_FAILURE_BYTES {
        let mut boundary = ROVODEV_MAX_FAILURE_BYTES;
        while !error.is_char_boundary(boundary) {
            boundary = boundary.saturating_sub(1);
        }
        error.truncate(boundary);
    }
    RovoDevFailure { line, error }
}
