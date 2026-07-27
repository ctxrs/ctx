use super::*;

pub(super) fn classify_output(message: &Value) -> bool {
    fn normalized(value: &str) -> String {
        value
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    }
    for field in ["role", "type", "kind", "messageType", "message_type"] {
        if message
            .get(field)
            .and_then(Value::as_str)
            .map(normalized)
            .is_some_and(|value| {
                matches!(
                    value.as_str(),
                    "tool"
                        | "toolresult"
                        | "tooloutput"
                        | "functionresult"
                        | "functionoutput"
                        | "commandresult"
                        | "commandoutput"
                )
            })
        {
            return true;
        }
    }
    let Value::Object(object) = message else {
        return false;
    };
    let normalized_keys = object
        .keys()
        .map(|key| normalized(key))
        .collect::<BTreeSet<_>>();
    normalized_keys.iter().any(|key| {
        matches!(
            key.as_str(),
            "toolresult"
                | "tooloutput"
                | "functionresult"
                | "functionoutput"
                | "commandresult"
                | "commandoutput"
        )
    }) || (normalized_keys
        .iter()
        .any(|key| matches!(key.as_str(), "toolcallid" | "tooluseid" | "callid"))
        && normalized_keys
            .iter()
            .any(|key| matches!(key.as_str(), "result" | "output" | "error")))
        || (normalized_keys.iter().any(|key| {
            matches!(
                key.as_str(),
                "output" | "result" | "error" | "stdout" | "stderr"
            )
        }) && normalized_keys.iter().any(|key| {
            matches!(
                key.as_str(),
                "command"
                    | "cmd"
                    | "exitcode"
                    | "duration"
                    | "durationms"
                    | "toolname"
                    | "functionname"
                    | "status"
                    | "outcome"
                    | "timedout"
                    | "timeout"
            )
        }))
}

pub(super) fn is_failure_or_timeout(message: &Value) -> bool {
    provider_output_event_is_failure(message)
}

pub(super) fn output_outcome(message: &Value) -> OutputOutcomeMetadata {
    let evidence = provider_result_outcome_evidence(EventType::ToolOutput, message);
    let timeout = recursively_has_timeout(message, &mut 4096);
    let outcome = if timeout {
        OutputOutcome::Timeout
    } else {
        match evidence.as_str() {
            Some("success") => OutputOutcome::Success,
            Some("failure") => OutputOutcome::Failure,
            _ => OutputOutcome::Unknown,
        }
    };
    let exit_code =
        find_i64(message, &["exit_code", "exitCode"]).and_then(|value| i32::try_from(value).ok());
    let duration_ms = find_i64(message, &["duration_ms", "durationMs"])
        .and_then(|value| u64::try_from(value).ok());
    OutputOutcomeMetadata {
        outcome,
        exit_code,
        duration_ms,
    }
}

pub(super) fn recursively_has_timeout(value: &Value, remaining: &mut usize) -> bool {
    if *remaining == 0 {
        return false;
    }
    *remaining -= 1;
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            (matches!(key.as_str(), "timed_out" | "timedOut" | "timeout")
                && value.as_bool() == Some(true))
                || (matches!(key.as_str(), "status" | "state" | "outcome")
                    && value
                        .as_str()
                        .is_some_and(|value| value.to_ascii_lowercase().contains("timeout")))
                || recursively_has_timeout(value, remaining)
        }),
        Value::Array(values) => values
            .iter()
            .any(|value| recursively_has_timeout(value, remaining)),
        _ => false,
    }
}

pub(super) fn find_i64(value: &Value, keys: &[&str]) -> Option<i64> {
    match value {
        Value::Object(object) => {
            for key in keys {
                if let Some(value) = object.get(*key).and_then(Value::as_i64) {
                    return Some(value);
                }
            }
            object.values().find_map(|value| find_i64(value, keys))
        }
        Value::Array(values) => values.iter().find_map(|value| find_i64(value, keys)),
        _ => None,
    }
}

pub(super) fn output_row(
    provider_session_id: &str,
    frontier: TraeFrontier,
    raw_session_index: u32,
    byte_range: Range<usize>,
    event: &TraeEventInput,
    cwd: Option<&str>,
) -> TraeOutputRow {
    let call_id = task_json_string_field(
        &event.raw_message,
        &[
            "toolCallId",
            "tool_call_id",
            "callId",
            "call_id",
            "toolUseId",
        ],
    );
    let tool_name = task_json_string_field(
        &event.raw_message,
        &["toolName", "tool_name", "name", "functionName"],
    );
    let command = task_json_string_field(
        &event.raw_message,
        &["command", "cmd", "input", "toolInput"],
    );
    TraeOutputRow {
        provider_session_id: provider_session_id.to_owned(),
        key_index: frontier.key_index,
        session_index: raw_session_index,
        message_index: frontier.message_index,
        native_message_id: event.native_message_id.clone(),
        occurred_at: event.occurred_at,
        call_id,
        command: command.map(|command| OutputCommandContext {
            tool_name: tool_name.unwrap_or_else(|| "trae-tool".to_owned()),
            command,
            working_directory: cwd.map(str::to_owned),
        }),
        outcome: output_outcome(&event.raw_message),
        byte_range,
        content: event.text.as_bytes().to_vec(),
    }
}

pub(super) fn output_row_bytes(row: &TraeOutputRow) -> usize {
    row.provider_session_id
        .len()
        .saturating_add(row.native_message_id.len())
        .saturating_add(row.call_id.as_deref().map_or(0, str::len))
        .saturating_add(
            row.command
                .as_ref()
                .map_or(0, |command| command.tool_name.len() + command.command.len()),
        )
        .saturating_add(row.content.len())
        .saturating_add(1024)
}

pub(super) fn sparse_failure_event(
    provider_session_id: &str,
    workspace_id: &str,
    chat_key: &str,
    event: &TraeEventInput,
) -> TraeCoreEvent {
    let outcome = output_outcome(&event.raw_message);
    let call_id = task_json_string_field(
        &event.raw_message,
        &[
            "toolCallId",
            "tool_call_id",
            "callId",
            "call_id",
            "toolUseId",
        ],
    );
    let event_id = format!("{provider_session_id}:{}", event.native_message_id);
    TraeCoreEvent {
        provider_event_index: event.provider_event_index,
        provider_event_hash: event_id.clone(),
        cursor: format!("{chat_key}:{event_id}"),
        event_type: EventType::ToolOutput,
        role: Some(EventRole::Tool),
        occurred_at: event.occurred_at,
        fidelity: Fidelity::Partial,
        idempotency_key: format!("provider-event:trae:{TRAE_STATE_VSCDB_SOURCE_FORMAT}:{event_id}"),
        payload: json!({
            "event_id": event_id,
            "native_workspace_id": workspace_id,
            "native_message_id": event.native_message_id,
            "result_outcome": "failure",
            "exit_code": outcome.exit_code,
            "duration_ms": outcome.duration_ms,
            "timed_out": outcome.outcome == OutputOutcome::Timeout,
            "call_id": call_id,
            "output_bytes": event.text.len(),
            "artifacts": [],
        }),
        metadata: json!({
            "source": "trae_state_vscdb_itemtable",
            "source_format": TRAE_STATE_VSCDB_SOURCE_FORMAT,
            "chat_key": chat_key,
            "native_message_id": event.native_message_id,
            "output_body_retained": false,
        }),
    }
}

pub(super) fn core_event_bytes(event: &TraeCoreEvent) -> usize {
    serde_json::to_vec(&event.payload)
        .map_or(usize::MAX / 2, |value| value.len())
        .saturating_add(
            serde_json::to_vec(&event.metadata).map_or(usize::MAX / 2, |value| value.len()),
        )
        .saturating_add(2048)
}

pub(super) fn attach_trae_complete_content_locator(
    event: &mut TraeCoreEvent,
    locator: &NativeLocator,
    record_digest: &CompleteContentBodyDigest,
    complete_text: &str,
) -> Result<()> {
    if event.event_type != EventType::Message
        || event
            .payload
            .pointer("/text_retention/truncated")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Ok(());
    }
    let content_ref = ContentRef::from_bytes(complete_text.as_bytes()).ok_or(
        CaptureError::SystemInvariant("SQLite content length exceeds ContentRef bounds"),
    )?;
    let profile = verified_content_profile(
        CaptureProvider::Trae,
        TRAE_STATE_VSCDB_SOURCE_FORMAT,
        CompleteContentSourceFamily::Sqlite,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "supported Trae message route has no verified-content profile",
    ))?;
    let persisted = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Sqlite,
        locator.kind(),
        locator.value(),
        event.provider_event_hash.clone(),
        record_digest.clone(),
    )
    .ok_or(CaptureError::SystemInvariant(
        "Trae complete-content locator exceeds the bounded canonical schema",
    ))?;
    attach_verified_content_locator(&mut event.metadata, persisted).ok_or(
        CaptureError::SystemInvariant("Trae verified-content locator collection is malformed"),
    )?;
    Ok(())
}
