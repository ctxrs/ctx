use super::*;

pub(super) fn parse_auggie_source(
    path: &Path,
    context: &ProviderAdapterContext,
    inventory_token: Option<&str>,
    include_outputs: bool,
) -> Result<ParsedAuggieSource> {
    let before = AuggieFileStamp::observe(path)?;
    let max_bytes = u64::try_from(MAX_PROVIDER_JSONL_LINE_BYTES).unwrap_or(u64::MAX);
    if before.len > max_bytes {
        return Err(CaptureError::InvalidPayload(format!(
            "Auggie session JSON exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit"
        )));
    }
    let bytes = fs::read(&before.canonical_path)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != before.len {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let after = AuggieFileStamp::observe(&before.canonical_path)?;
    if after != before {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let root = serde_json::from_slice::<Value>(&bytes).map_err(|error| {
        CaptureError::InvalidPayload(format!("invalid Auggie session JSON: {error}"))
    })?;
    let data = AuggieSessionData::parse(&root, &before.canonical_path, context)?;
    let source_revision = source_revision(&before, &bytes, inventory_token);
    let session = ParsedAuggieSession {
        provider_session_id: data.provider_session_id.clone(),
        parent_provider_session_id: data.parent_provider_session_id.clone(),
        root_provider_session_id: data.root_provider_session_id.clone(),
        external_agent_id: data.external_agent_id.clone(),
        started_at: data.started_at,
        ended_at: data.ended_at,
        cwd: data.cwd.clone(),
        raw_source_path: data.raw_source_path.clone(),
        source_metadata: data.source_metadata.clone(),
        session_metadata: data.session_metadata.clone(),
    };
    let events = parse_core_events(&data, &bytes)?;
    let outputs = if include_outputs {
        parse_outputs(&data)?
    } else {
        Vec::new()
    };
    Ok(ParsedAuggieSource {
        stamp: before,
        source_revision,
        session,
        events,
        outputs,
    })
}

fn parse_core_events(data: &AuggieSessionData<'_>, bytes: &[u8]) -> Result<Vec<ParsedAuggieEvent>> {
    let mut events = Vec::new();
    let mut provider_event_index = 0_u64;
    for (chat_index, entry) in data.chat_history.iter().enumerate() {
        let exchange = entry.get("exchange").unwrap_or(entry);
        let base_time = auggie_entry_time(entry, Some(exchange)).unwrap_or_else(|| {
            data.started_at + Duration::milliseconds(saturating_i64(chat_index).saturating_mul(2))
        });
        for (role, label, occurred_at, text) in [
            (
                EventRole::User,
                "request",
                base_time,
                auggie_request_text(exchange),
            ),
            (
                EventRole::Assistant,
                "response",
                base_time + Duration::milliseconds(1),
                auggie_response_text(exchange),
            ),
        ] {
            let Some(text) = text else {
                continue;
            };
            let complete_text = text.clone();
            let mut event = auggie_event(AuggieEventInput {
                provider_session_id: &data.provider_session_id,
                provider_event_index,
                chat_index,
                role,
                label,
                occurred_at,
                text,
                entry,
                exchange,
                raw_source_path: &data.raw_source_path,
            });
            let event_hash = event.provider_event_hash.clone();
            let sub_index = u32::try_from(events.len()).map_err(|_| {
                CaptureError::InvalidPayload(
                    "Auggie session contains too many normalized messages".to_owned(),
                )
            })?;
            attach_auggie_complete_content_locator(
                &mut event,
                0,
                sub_index,
                &event_hash,
                bytes,
                &complete_text,
            )?;
            events.push(ParsedAuggieEvent {
                event,
                chat_index,
                sub_index,
            });
            provider_event_index =
                provider_event_index
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Auggie provider event index overflowed",
                    ))?;
        }
    }
    Ok(events)
}

fn attach_auggie_complete_content_locator(
    event: &mut AuggieEvent,
    source_record_ordinal: u64,
    source_record_subrecord_index: u32,
    native_record_id: &str,
    record_bytes: &[u8],
    complete_text: &str,
) -> Result<()> {
    if event.event_type != EventType::Message
        || complete_text.chars().count() <= PROVIDER_MAX_TEXT_CHARS
    {
        return Ok(());
    }
    if native_record_id.is_empty()
        || native_record_id.len() > 1_024
        || native_record_id.chars().any(char::is_control)
    {
        return Err(CaptureError::InvalidPayload(
            "Auggie complete-content native record identity is invalid".to_owned(),
        ));
    }
    let locator_value = auggie_structured_locator(
        source_record_ordinal,
        source_record_subrecord_index,
        native_record_id,
    )?;
    let content_ref = ContentRef::from_bytes(complete_text.as_bytes()).ok_or(
        CaptureError::SystemInvariant("Auggie complete content exceeds ContentRef bounds"),
    )?;
    let profile = verified_content_profile(
        CaptureProvider::Auggie,
        AUGGIE_SESSION_JSON_SOURCE_FORMAT,
        CompleteContentSourceFamily::Structured,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Auggie complete-content profile is not registered",
    ))?;
    let locator = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Structured,
        STRUCTURED_COMPLETE_CONTENT_LOCATOR_KIND,
        &locator_value,
        native_record_id,
        CompleteContentBodyDigest::from_bytes(record_bytes),
    )
    .ok_or(CaptureError::SystemInvariant(
        "Auggie complete-content locator exceeds its typed bounds",
    ))?;
    attach_verified_content_locator(&mut event.metadata, locator).ok_or(
        CaptureError::SystemInvariant("Auggie complete-content locator metadata is malformed"),
    )?;
    Ok(())
}

fn auggie_structured_locator(
    source_record_ordinal: u64,
    source_record_subrecord_index: u32,
    native_record_id: &str,
) -> Result<Vec<u8>> {
    let provider = CaptureProvider::Auggie.as_str().as_bytes();
    let provider_len = u8::try_from(provider.len())
        .map_err(|_| CaptureError::SystemInvariant("Auggie provider identity is too long"))?;
    let native_id = native_record_id.as_bytes();
    let native_len = u16::try_from(native_id.len()).map_err(|_| {
        CaptureError::InvalidPayload(
            "Auggie complete-content native record identity is too long".to_owned(),
        )
    })?;
    let mut value = Vec::with_capacity(4 + 1 + provider.len() + 8 + 4 + 2 + native_id.len());
    value.extend_from_slice(b"SC\0\x01");
    value.push(provider_len);
    value.extend_from_slice(provider);
    value.extend_from_slice(&source_record_ordinal.to_be_bytes());
    value.extend_from_slice(&source_record_subrecord_index.to_be_bytes());
    value.extend_from_slice(&native_len.to_be_bytes());
    value.extend_from_slice(native_id);
    Ok(value)
}

fn parse_outputs(data: &AuggieSessionData<'_>) -> Result<Vec<ParsedAuggieOutput>> {
    let mut outputs = Vec::new();
    for (chat_index, entry) in data.chat_history.iter().enumerate() {
        let exchange = entry.get("exchange").unwrap_or(entry);
        let occurred_at = auggie_entry_time(entry, Some(exchange));
        for (node_collection, nodes) in [
            (
                "request",
                exchange
                    .get("request_nodes")
                    .or_else(|| exchange.get("requestNodes"))
                    .and_then(Value::as_array),
            ),
            (
                "response",
                exchange
                    .get("response_nodes")
                    .or_else(|| exchange.get("responseNodes"))
                    .and_then(Value::as_array),
            ),
        ]
        .into_iter()
        .filter_map(|(collection, nodes)| nodes.map(|nodes| (collection, nodes)))
        {
            for (node_index, node) in nodes.iter().enumerate() {
                if !auggie_node_is_tool_result(node) {
                    continue;
                }
                let Some(content) = auggie_tool_result_content(node) else {
                    continue;
                };
                let content = content.into_bytes();
                let output_sequence = u32::try_from(outputs.len()).map_err(|_| {
                    CaptureError::InvalidPayload(
                        "Auggie session contains too many output observations".to_owned(),
                    )
                })?;
                outputs.push(ParsedAuggieOutput {
                    output_sequence,
                    chat_index,
                    node_collection,
                    node_index,
                    occurred_at,
                    call_id: provider_text(
                        node,
                        &["call_id", "callId", "tool_call_id", "toolCallId", "id"],
                    ),
                    outcome: auggie_output_outcome(node),
                    content_sha256: format!("{:x}", Sha256::digest(&content)),
                    content,
                });
            }
        }
    }
    Ok(outputs)
}

fn auggie_node_is_tool_result(node: &Value) -> bool {
    let kind = node
        .get("type")
        .or_else(|| node.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    matches!(
        kind,
        "tool_result"
            | "tool-result"
            | "tool_use_result"
            | "tool-use-result"
            | "tool_output"
            | "tool-output"
            | "function_result"
            | "function_output"
    ) || node.get("tool_result").is_some()
        || node.get("toolResult").is_some()
}

fn auggie_tool_result_content(node: &Value) -> Option<String> {
    let content = if node.get("tool_result").is_some() || node.get("toolResult").is_some() {
        node.pointer("/tool_result/content")
            .or_else(|| node.pointer("/toolResult/content"))
    } else {
        ["content", "output", "result"]
            .into_iter()
            .find_map(|key| node.get(key))
    };
    content
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|content| !content.is_empty())
}

fn auggie_output_outcome(node: &Value) -> OutputOutcomeMetadata {
    let exit_code = node
        .get("exit_code")
        .or_else(|| node.get("exitCode"))
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok());
    let duration_ms = node
        .get("duration_ms")
        .or_else(|| node.get("durationMs"))
        .and_then(Value::as_u64);
    let status = node
        .get("status")
        .or_else(|| node.get("outcome"))
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase);
    let outcome = if node
        .get("timed_out")
        .or_else(|| node.get("timedOut"))
        .and_then(Value::as_bool)
        == Some(true)
        || status.as_deref() == Some("timeout")
    {
        OutputOutcome::Timeout
    } else if node
        .get("is_error")
        .or_else(|| node.get("isError"))
        .and_then(Value::as_bool)
        == Some(true)
        || exit_code.is_some_and(|code| code != 0)
        || matches!(status.as_deref(), Some("failure" | "failed" | "error"))
    {
        OutputOutcome::Failure
    } else if node
        .get("is_error")
        .or_else(|| node.get("isError"))
        .and_then(Value::as_bool)
        == Some(false)
        || exit_code == Some(0)
        || matches!(status.as_deref(), Some("success" | "succeeded" | "ok"))
    {
        OutputOutcome::Success
    } else {
        OutputOutcome::Unknown
    };
    OutputOutcomeMetadata {
        outcome,
        exit_code,
        duration_ms,
    }
}
