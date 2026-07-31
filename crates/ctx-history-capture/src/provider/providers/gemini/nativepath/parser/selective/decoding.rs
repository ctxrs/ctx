use super::*;

#[allow(clippy::too_many_arguments)]
pub(in super::super) fn decode_result_record(
    payload: &[u8],
    profile: GeminiNativePathProfile,
    source: &GeminiTranscriptSource,
    session: &GeminiSession,
    raw_ordinal: u64,
    source_record: GeminiSourceRecordEvidence,
    byte_start: u64,
    byte_end_exclusive: u64,
) -> std::result::Result<DecodedGeminiResult, String> {
    #[cfg(test)]
    TEST_RESULT_SELECTIVE_PASSES.set(TEST_RESULT_SELECTIVE_PASSES.get().saturating_add(1));
    let capture_full_content = profile == GeminiNativePathProfile::CoreAndTransientOutputs;
    #[cfg(test)]
    if capture_full_content {
        TEST_RESULT_FULL_HYDRATIONS.set(TEST_RESULT_FULL_HYDRATIONS.get().saturating_add(1));
    }
    // This is the record's sole decoding pass. CoreOnly computes only the
    // bounded transient material needed to recognize the exact released
    // positional hash; the same visitor captures full Pro content.
    let result = parse_result_record_selectively(payload, capture_full_content)?;
    let occurred_at_unix_ms = result.occurred_at_unix_ms;
    let native_record_id = result.native_record_id;
    let probed = result.outputs;
    if probed.len() > MAX_GEMINI_NATIVE_PAGE_RECORDS {
        return Err(format!(
            "Gemini result record exceeds the {MAX_GEMINI_NATIVE_PAGE_RECORDS} output limit"
        ));
    }
    let mut decoded = DecodedGeminiResult {
        events: Vec::new(),
        outputs: Vec::new(),
        output_reservations: Vec::new(),
        decoded_body_bytes: 0,
        failure_diagnostics: 0,
        failure_previews: 0,
    };
    let mut retained_identities = BTreeSet::new();
    for (index, mut probed) in probed.into_iter().enumerate() {
        let content = probed.content.take();
        let retained_failure = !probed.redacted
            && matches!(
                probed.outcome.outcome,
                OutputOutcome::Failure | OutputOutcome::Timeout
            );
        decoded.decoded_body_bytes = decoded.decoded_body_bytes.saturating_add(
            if profile == GeminiNativePathProfile::CoreAndTransientOutputs {
                content.as_ref().map_or(0, |content| content.len() as u64)
            } else if retained_failure {
                probed
                    .released_diagnostic_preview
                    .as_ref()
                    .map_or(0, |preview| preview.len() as u64)
            } else {
                0
            },
        );
        let sub_ordinal = u32::try_from(index)
            .map_err(|_| "Gemini result subrecord ordinal overflowed".to_owned())?;
        let event_identity = result_event_identity(native_record_id.as_deref(), &probed);
        let GeminiEventIdentity::NativeRecordId(identity_key) = &event_identity;
        if !retained_identities.insert(identity_key.clone()) {
            continue;
        }
        if !probed.redacted && probed.has_output_content {
            decoded.output_reservations.push((
                sub_ordinal,
                conservative_transient_output_reservation(
                    probed.content_bytes,
                    probed.call_id.as_deref(),
                    &event_identity,
                    source,
                    session,
                    byte_start,
                    byte_end_exclusive,
                    native_record_id.as_deref(),
                )?,
            ));
        }
        if retained_failure {
            let event = decode_output_diagnostic(
                native_record_id.as_deref(),
                occurred_at_unix_ms,
                raw_ordinal,
                sub_ordinal,
                source_record,
                &probed,
                event_identity.clone(),
            )?;
            let event_bytes = retained_event_bytes(&event)?;
            decoded.failure_diagnostics = decoded.failure_diagnostics.saturating_add(1);
            if probed.released_diagnostic_preview.is_some() {
                decoded.failure_previews = decoded.failure_previews.saturating_add(1);
            }
            decoded.events.push((event.event, event_bytes));
        }
        if profile == GeminiNativePathProfile::CoreAndTransientOutputs
            && !probed.redacted
            && probed.has_output_content
        {
            push_transient_output(
                &mut decoded.outputs,
                content.unwrap_or_default(),
                probed.outcome,
                probed.call_id,
                sub_ordinal,
                event_identity,
                source,
                session,
                raw_ordinal,
                byte_start,
                byte_end_exclusive,
                occurred_at_unix_ms,
                native_record_id.as_deref(),
            )?;
        }
    }
    Ok(decoded)
}

fn decode_output_diagnostic(
    native_record_id: Option<&str>,
    occurred_at_unix_ms: Option<i64>,
    raw_ordinal: u64,
    sub_ordinal: u32,
    source_record: GeminiSourceRecordEvidence,
    output: &ProbedGeminiOutput,
    identity: GeminiEventIdentity,
) -> std::result::Result<DecodedGeminiEvent, String> {
    let outcome = match output.outcome.outcome {
        OutputOutcome::Failure => "failure",
        OutputOutcome::Timeout => "timeout",
        OutputOutcome::Success => "success",
        OutputOutcome::Unknown => "unknown",
    }
    .to_owned();
    let body = GeminiEventBody::OutputDiagnostic {
        call_id: output.call_id.clone(),
        tool_name: output.tool_name.clone(),
        outcome,
        exit_code: output.outcome.exit_code,
        duration_ms: output.outcome.duration_ms,
    };
    let body_bytes = serde_json::to_vec(&body)
        .map_err(|error| format!("failed to encode Gemini output diagnostic: {error}"))?;
    let mut hasher = Sha256::new();
    hasher.update(BODY_HASH_DOMAIN);
    hasher.update(&body_bytes);
    // The normalized fallback hash must notice output rewrites even though
    // output bytes never enter the Core body.
    hasher.update(b"\0gemini-result-content\0");
    hasher.update(output.fallback_identity_sha256);
    let body_sha256 = hasher.finalize().into();
    let released_body = ReleasedGeminiEventBody::OutputDiagnostic {
        call_id: output.call_id.clone(),
        tool_name: output.tool_name.clone(),
        outcome: match output.outcome.outcome {
            OutputOutcome::Failure => "failure",
            OutputOutcome::Timeout => "timeout",
            OutputOutcome::Success => "success",
            OutputOutcome::Unknown => "unknown",
        }
        .to_owned(),
        exit_code: output.outcome.exit_code,
        duration_ms: output.outcome.duration_ms,
        output_preview: output.released_diagnostic_preview.clone(),
    };
    let released_body_bytes = serde_json::to_vec(&released_body)
        .map_err(|error| format!("failed to encode released Gemini output diagnostic: {error}"))?;
    let mut released_hasher = Sha256::new();
    released_hasher.update(BODY_HASH_DOMAIN);
    released_hasher.update(&released_body_bytes);
    Ok(DecodedGeminiEvent {
        event: GeminiRetainedEvent {
            identity,
            released_identity: format!(
                "{}:subrecord:{sub_ordinal}",
                native_record_id
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("raw-{raw_ordinal}"))
            ),
            native_order: GeminiNativeOrder {
                raw_ordinal,
                sub_ordinal,
            },
            source_record,
            event_type: EventType::ToolOutput,
            role: EventRole::Tool,
            occurred_at: occurred_at_unix_ms.and_then(DateTime::<Utc>::from_timestamp_millis),
            body,
            body_sha256,
            released_body_sha256: released_hasher.finalize().into(),
            preview: String::new(),
            searchable_text: String::new(),
            safe_file_touches: Vec::new(),
        },
        serialized_body_bytes: body_bytes.len(),
    })
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ReleasedGeminiEventBody {
    OutputDiagnostic {
        call_id: Option<String>,
        tool_name: Option<String>,
        outcome: String,
        exit_code: Option<i32>,
        duration_ms: Option<u64>,
        output_preview: Option<String>,
    },
}

#[allow(clippy::too_many_arguments)]
fn conservative_transient_output_reservation(
    output_content_bytes: usize,
    call_id: Option<&str>,
    event_identity: &GeminiEventIdentity,
    source: &GeminiTranscriptSource,
    session: &GeminiSession,
    byte_start: u64,
    byte_end_exclusive: u64,
    native_record_id: Option<&str>,
) -> std::result::Result<usize, String> {
    let source_locator = GeminiSourceLocator {
        path: source.path.clone(),
        byte_start,
        byte_end_exclusive,
    };
    let locator_payload = serde_json::to_vec(&source_locator)
        .map_err(|error| format!("failed to encode Gemini output source locator: {error}"))?;
    let unit_key = output_unit_key(session, event_identity);
    let root_session_id = session
        .parent_native_session_id
        .as_deref()
        .unwrap_or(&session.native_session_id);
    let mut total = OUTPUT_ENVELOPE_FIXED_BYTES;
    for value in [
        Some(unit_key.as_str()),
        native_record_id,
        Some(session.native_session_id.as_str()),
        Some(root_session_id),
        session.parent_native_session_id.as_deref(),
        Some(session.native_session_id.as_str()),
        call_id,
        Some("gemini/nativepath/jsonl-result"),
    ]
    .into_iter()
    .flatten()
    {
        total = total
            .checked_add(estimated_json_string_wire_bytes(value).ok_or_else(|| {
                "Gemini transient output reservation byte count overflowed".to_owned()
            })?)
            .ok_or_else(|| {
                "Gemini transient output reservation byte count overflowed".to_owned()
            })?;
    }
    total = total
        .checked_add(
            estimated_base64_wire_bytes(locator_payload.len()).ok_or_else(|| {
                "Gemini transient output reservation byte count overflowed".to_owned()
            })?,
        )
        .ok_or_else(|| "Gemini transient output reservation byte count overflowed".to_owned())?;
    total
        .checked_add(
            estimated_base64_wire_bytes(output_content_bytes).ok_or_else(|| {
                "Gemini transient output reservation byte count overflowed".to_owned()
            })?,
        )
        .ok_or_else(|| "Gemini transient output reservation byte count overflowed".to_owned())
}

#[allow(clippy::too_many_arguments)]
fn push_transient_output(
    outputs: &mut Vec<(ProOutputObservation, usize)>,
    content: String,
    outcome: OutputOutcomeMetadata,
    call_id: Option<String>,
    sub_ordinal: u32,
    event_identity: GeminiEventIdentity,
    source: &GeminiTranscriptSource,
    session: &GeminiSession,
    raw_ordinal: u64,
    byte_start: u64,
    byte_end_exclusive: u64,
    occurred_at_unix_ms: Option<i64>,
    native_record_id: Option<&str>,
) -> std::result::Result<(), String> {
    if outputs.len() >= MAX_GEMINI_NATIVE_PAGE_RECORDS {
        return Err(format!(
            "Gemini result record exceeds the {MAX_GEMINI_NATIVE_PAGE_RECORDS} output limit"
        ));
    }
    let source_locator = GeminiSourceLocator {
        path: source.path.clone(),
        byte_start,
        byte_end_exclusive,
    };
    let locator_payload = serde_json::to_vec(&source_locator)
        .map_err(|error| format!("failed to encode Gemini output source locator: {error}"))?;
    let root_session_id = session
        .parent_native_session_id
        .clone()
        .unwrap_or_else(|| session.native_session_id.clone());
    let observation = ProOutputObservation {
        kind: OutputObservationKind::Tool,
        coordinate: OutputNativeCoordinate {
            unit_key: output_unit_key(session, &event_identity),
            native_sequence: raw_ordinal,
            native_record_id: native_record_id.map(str::to_owned),
            source_record_ordinal: Some(raw_ordinal),
            source_record_subrecord_index: Some(sub_ordinal),
            byte_start: Some(byte_start),
            byte_end_exclusive: Some(byte_end_exclusive),
        },
        occurred_at_unix_ms,
        associations: OutputAssociations {
            direct_session_id: session.native_session_id.clone(),
            root_session_id,
            parent_session_id: session.parent_native_session_id.clone(),
            provider_session_id: Some(session.native_session_id.clone()),
            agent_id: None,
            repository: None,
        },
        call_id,
        command: None,
        outcome,
        locator: OutputSourceLocator {
            version: 1,
            kind: "gemini/nativepath/jsonl-result".to_owned(),
            payload: locator_payload,
        },
        content: content.into_bytes(),
    };
    let serialized_bytes = transient_output_bytes(&observation)?;
    outputs.push((observation, serialized_bytes));
    Ok(())
}

fn transient_output_bytes(
    observation: &ProOutputObservation,
) -> std::result::Result<usize, String> {
    let mut total = OUTPUT_ENVELOPE_FIXED_BYTES;
    for value in [
        Some(observation.coordinate.unit_key.as_str()),
        observation.coordinate.native_record_id.as_deref(),
        Some(observation.associations.direct_session_id.as_str()),
        Some(observation.associations.root_session_id.as_str()),
        observation.associations.parent_session_id.as_deref(),
        observation.associations.provider_session_id.as_deref(),
        observation.associations.agent_id.as_deref(),
        observation.call_id.as_deref(),
        Some(observation.locator.kind.as_str()),
    ]
    .into_iter()
    .flatten()
    {
        total = total
            .checked_add(estimated_json_string_wire_bytes(value).ok_or_else(|| {
                "Gemini transient output serialized byte count overflowed".to_owned()
            })?)
            .ok_or_else(|| "Gemini transient output serialized byte count overflowed".to_owned())?;
    }
    if let Some(command) = &observation.command {
        for value in [
            Some(command.tool_name.as_str()),
            Some(command.command.as_str()),
            command.working_directory.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            total = total
                .checked_add(estimated_json_string_wire_bytes(value).ok_or_else(|| {
                    "Gemini transient output serialized byte count overflowed".to_owned()
                })?)
                .ok_or_else(|| {
                    "Gemini transient output serialized byte count overflowed".to_owned()
                })?;
        }
    }
    total = total
        .checked_add(
            estimated_base64_wire_bytes(observation.locator.payload.len()).ok_or_else(|| {
                "Gemini transient output serialized byte count overflowed".to_owned()
            })?,
        )
        .ok_or_else(|| "Gemini transient output serialized byte count overflowed".to_owned())?;
    total
        .checked_add(
            estimated_base64_wire_bytes(observation.content.len()).ok_or_else(|| {
                "Gemini transient output serialized byte count overflowed".to_owned()
            })?,
        )
        .ok_or_else(|| "Gemini transient output serialized byte count overflowed".to_owned())
}

#[derive(Debug, Deserialize)]
struct GeminiStateNoticeDto {
    id: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "$set")]
    set: GeminiStateSetDto,
}

#[derive(Debug, Deserialize)]
struct GeminiStateSetDto {
    summary: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiRewindNoticeDto {
    id: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "$rewindTo")]
    rewind_to: String,
}

#[derive(Debug)]
pub(in super::super) enum GeminiDecodingError {
    Invalid(String),
    TouchOverflow(GeminiTouchOverflow),
}

pub(in super::super) struct DecodedGeminiEvent {
    pub(in super::super) event: GeminiRetainedEvent,
    pub(in super::super) serialized_body_bytes: usize,
}

impl From<String> for GeminiDecodingError {
    fn from(error: String) -> Self {
        Self::Invalid(error)
    }
}

pub(in super::super) fn decode_retained_event(
    payload: &[u8],
    class: GeminiRecordClass,
    raw_ordinal: u64,
    source_record: GeminiSourceRecordEvidence,
) -> std::result::Result<Option<DecodedGeminiEvent>, GeminiDecodingError> {
    let (id, occurred_at, event_type, role, body, searchable_text, safe_file_touches) = match class
    {
        GeminiRecordClass::Message => {
            let dto: GeminiMessageDto = serde_json::from_slice(payload)
                .map_err(|error| format!("invalid Gemini message: {error}"))?;
            let Some(text) = dto.content.filter(|text| !text.is_empty()) else {
                return Ok(None);
            };
            let role = match dto.record_type.as_deref() {
                Some("user") => EventRole::User,
                Some("gemini") => EventRole::Assistant,
                _ => return Ok(None),
            };
            (
                required_record_id(dto.id)?,
                dto.timestamp.as_deref().and_then(parse_timestamp),
                EventType::Message,
                role,
                GeminiEventBody::Message {
                    text: text.clone(),
                    model: dto.model,
                },
                text,
                Vec::new(),
            )
        }
        GeminiRecordClass::ToolCall => {
            let dto: GeminiToolCallRecordDto = serde_json::from_slice(payload)
                .map_err(|error| format!("invalid Gemini tool call: {error}"))?;
            if dto.tool_calls.iter().any(|call| call.result.0) {
                return Err(GeminiDecodingError::Invalid(
                    "Gemini result-bearing tool call reached retained decoding".to_owned(),
                ));
            }
            let calls: Vec<_> = dto
                .tool_calls
                .into_iter()
                .map(|call| GeminiToolCall {
                    id: nonempty(call.id),
                    name: nonempty(call.name),
                    args: call.args,
                })
                .collect();
            if calls.is_empty() {
                return Ok(None);
            }
            let searchable_text = tool_call_search_text(&calls);
            let safe_file_touches =
                safe_file_touches(&calls).map_err(GeminiDecodingError::TouchOverflow)?;
            (
                required_record_id(dto.id)?,
                dto.timestamp.as_deref().and_then(parse_timestamp),
                EventType::ToolCall,
                EventRole::Assistant,
                GeminiEventBody::ToolCall { calls },
                searchable_text,
                safe_file_touches,
            )
        }
        GeminiRecordClass::StateNotice => {
            let dto: GeminiStateNoticeDto = serde_json::from_slice(payload)
                .map_err(|error| format!("invalid Gemini state notice: {error}"))?;
            let summary = dto.set.summary;
            (
                required_record_id(dto.id)?,
                dto.timestamp.as_deref().and_then(parse_timestamp),
                EventType::Notice,
                EventRole::System,
                GeminiEventBody::StateNotice {
                    summary: summary.clone(),
                },
                summary.unwrap_or_default(),
                Vec::new(),
            )
        }
        GeminiRecordClass::RewindNotice => {
            let dto: GeminiRewindNoticeDto = serde_json::from_slice(payload)
                .map_err(|error| format!("invalid Gemini rewind notice: {error}"))?;
            let target = dto.rewind_to.trim().to_owned();
            if target.is_empty() {
                return Ok(None);
            }
            (
                required_record_id(dto.id)?,
                dto.timestamp.as_deref().and_then(parse_timestamp),
                EventType::Notice,
                EventRole::System,
                GeminiEventBody::RewindNotice {
                    target_native_record_id: target.clone(),
                },
                format!("rewind to {target}"),
                Vec::new(),
            )
        }
        GeminiRecordClass::Header | GeminiRecordClass::Result | GeminiRecordClass::Ignored => {
            return Ok(None)
        }
    };

    let body_bytes = serde_json::to_vec(&body)
        .map_err(|error| format!("failed to encode retained Gemini body: {error}"))?;
    let mut hasher = Sha256::new();
    hasher.update(BODY_HASH_DOMAIN);
    hasher.update(&body_bytes);
    let body_sha256 = hasher.finalize().into();
    let preview = searchable_text
        .chars()
        .take(PROVIDER_MAX_PREVIEW_CHARS)
        .collect();
    Ok(Some(DecodedGeminiEvent {
        event: GeminiRetainedEvent {
            identity: GeminiEventIdentity::NativeRecordId(id.clone()),
            released_identity: id,
            native_order: GeminiNativeOrder {
                raw_ordinal,
                sub_ordinal: 0,
            },
            source_record,
            event_type,
            role,
            occurred_at,
            body,
            body_sha256,
            released_body_sha256: body_sha256,
            preview,
            searchable_text,
            safe_file_touches,
        },
        serialized_body_bytes: body_bytes.len(),
    }))
}

pub(in super::super) fn retained_event_bytes(
    event: &DecodedGeminiEvent,
) -> std::result::Result<usize, String> {
    let mut total = EVENT_ENVELOPE_FIXED_BYTES
        .checked_add(event.serialized_body_bytes)
        .ok_or_else(|| "Gemini retained event byte count overflowed".to_owned())?;
    let GeminiEventIdentity::NativeRecordId(identity) = &event.event.identity;
    for value in [
        identity.as_str(),
        event.event.preview.as_str(),
        event.event.searchable_text.as_str(),
    ]
    .into_iter()
    .chain(event.event.safe_file_touches.iter().map(String::as_str))
    {
        total =
            total
                .checked_add(estimated_json_string_wire_bytes(value).ok_or_else(|| {
                    "Gemini retained event string byte count overflowed".to_owned()
                })?)
                .ok_or_else(|| "Gemini retained event byte count overflowed".to_owned())?;
    }
    Ok(total)
}

fn required_record_id(id: Option<String>) -> std::result::Result<String, String> {
    nonempty(id).ok_or_else(|| "Gemini event is missing a nonempty native id".to_owned())
}

pub(in super::super) fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

pub(super) fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn tool_call_search_text(calls: &[GeminiToolCall]) -> String {
    let mut text = String::new();
    for call in calls {
        if let Some(name) = call.name.as_deref() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(name);
        }
        if let Some(args) = call.args.as_ref() {
            if let Ok(args) = serde_json::to_string(args) {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&args);
            }
        }
    }
    text
}

fn safe_file_touches(
    calls: &[GeminiToolCall],
) -> std::result::Result<Vec<String>, GeminiTouchOverflow> {
    let mut touches = BTreeSet::new();
    let mut touch_bytes = 0_usize;
    for call in calls {
        let Some(Value::Object(args)) = call.args.as_ref() else {
            continue;
        };
        for key in ["path", "file_path", "filePath"] {
            if let Some(Value::String(path)) = args.get(key) {
                if path.trim().is_empty() || touches.contains(path) {
                    continue;
                }
                if touches.len() >= MAX_GEMINI_FILE_TOUCHES_PER_EVENT {
                    return Err(GeminiTouchOverflow::Count {
                        limit: MAX_GEMINI_FILE_TOUCHES_PER_EVENT,
                    });
                }
                let next_bytes =
                    touch_bytes
                        .checked_add(path.len())
                        .ok_or(GeminiTouchOverflow::Bytes {
                            limit: MAX_GEMINI_FILE_TOUCH_BYTES_PER_EVENT,
                        })?;
                if next_bytes > MAX_GEMINI_FILE_TOUCH_BYTES_PER_EVENT {
                    return Err(GeminiTouchOverflow::Bytes {
                        limit: MAX_GEMINI_FILE_TOUCH_BYTES_PER_EVENT,
                    });
                }
                touch_bytes = next_bytes;
                touches.insert(path.clone());
            }
        }
    }
    Ok(touches.into_iter().collect())
}
