use super::*;

#[allow(clippy::too_many_arguments)]
pub(in super::super) fn decode_result_record(
    payload: &[u8],
    raw_ordinal: u64,
    source_record: GeminiSourceRecordEvidence,
) -> std::result::Result<DecodedGeminiResult, String> {
    #[cfg(test)]
    TEST_RESULT_SELECTIVE_PASSES.set(TEST_RESULT_SELECTIVE_PASSES.get().saturating_add(1));
    let result = parse_result_record_selectively(payload)?;
    let occurred_at_unix_ms = result.occurred_at_unix_ms;
    let native_record_id = result.native_record_id;
    let mut probed = result.outputs;
    if probed.len() > MAX_GEMINI_NATIVE_PAGE_RECORDS {
        return Err(format!(
            "Gemini result record exceeds the {MAX_GEMINI_NATIVE_PAGE_RECORDS} output limit"
        ));
    }
    let mut decoded = DecodedGeminiResult { events: Vec::new() };
    let mut result_call_counts = BTreeMap::<String, usize>::new();
    for output in &probed {
        if let Some(call_id) = output.call_id.as_ref() {
            *result_call_counts.entry(call_id.clone()).or_default() += 1;
        }
    }
    for output in &mut probed {
        output.ambiguous_native_fields |= output
            .call_id
            .as_ref()
            .and_then(|call_id| result_call_counts.get(call_id))
            .is_some_and(|count| *count != 1);
    }
    for (index, probed) in probed.into_iter().enumerate() {
        let sub_ordinal = u32::try_from(index)
            .map_err(|_| "Gemini result subrecord ordinal overflowed".to_owned())?;
        let event_identity = result_event_identity(native_record_id.as_deref(), &probed, index);
        if !probed.redacted {
            let event = decode_output_diagnostic(
                occurred_at_unix_ms,
                raw_ordinal,
                sub_ordinal,
                source_record,
                &probed,
                event_identity.clone(),
            )?;
            let event_bytes = retained_event_bytes(&event)?;
            decoded.events.push((event.event, event_bytes));
        }
    }
    Ok(decoded)
}

fn decode_output_diagnostic(
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
        result: output.result.clone(),
        call_id: output.call_id.clone(),
        tool_name: output.tool_name.clone(),
        command: output.command.clone(),
        command_too_large: output.command_too_large,
        declared_workdir: output.declared_workdir.clone(),
        file_paths: output.file_paths.clone(),
        ambiguous_native_fields: output.ambiguous_native_fields,
        outcome,
        exit_code: output.outcome.exit_code,
        duration_ms: output.outcome.duration_ms,
    };
    let body_bytes = serde_json::to_vec(&body)
        .map_err(|error| format!("failed to encode Gemini output diagnostic: {error}"))?;
    let mut hasher = Sha256::new();
    hasher.update(BODY_HASH_DOMAIN);
    hasher.update(&body_bytes);
    let body_sha256 = hasher.finalize().into();
    Ok(DecodedGeminiEvent {
        event: GeminiRetainedEvent {
            identity,
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
            preview: String::new(),
            searchable_text: String::new(),
            safe_file_touches: Vec::new(),
        },
        serialized_body_bytes: body_bytes.len(),
    })
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
    super::super::super::file_invocation::normalize_gemini_tool_calls(calls).text
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
