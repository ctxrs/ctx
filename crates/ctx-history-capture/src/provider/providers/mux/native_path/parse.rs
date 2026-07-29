use super::*;

pub(super) fn open_reader_at_frontier(
    path: &Path,
    frontier: &MuxFrontier,
) -> Result<(BufReader<File>, Sha256)> {
    let mut file = File::open(path)?;
    let mut remaining = frontier.next_offset;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    while remaining != 0 {
        let take = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| CaptureError::SystemInvariant("Mux prefix size exceeds usize"))?;
        let read = file.read(&mut buffer[..take])?;
        if read == 0 {
            return Err(CaptureError::InvalidPayload(
                "Mux cursor frontier exceeds its source".to_owned(),
            ));
        }
        hasher.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    if <[u8; 32]>::from(hasher.clone().finalize()) != frontier.prefix_sha256 {
        return Err(CaptureError::InvalidPayload(
            "Mux cursor prefix no longer matches its source".to_owned(),
        ));
    }
    file.seek(SeekFrom::Start(frontier.next_offset))?;
    Ok((BufReader::new(file), hasher))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn read_core_page(
    reader: &mut BufReader<File>,
    hasher: &mut Sha256,
    session: &mut MuxBoundedSessionMetadata,
    plan: &MuxSourcePlan,
    expected: MuxFrontier,
    mut rejected_records: u64,
    mut first_failure: Option<MuxFailure>,
) -> Result<Option<MuxPreparedPage>> {
    let mut rows = Vec::new();
    let mut source_bytes = 0_usize;
    let mut physical_records = 0_usize;
    let mut offset = expected.next_offset;
    let mut ordinal = expected.next_ordinal;
    let mut metadata_failure = if expected.next_offset == 0 {
        session.metadata_failure.take()
    } else {
        session.metadata_failure = None;
        None
    };
    let mut deferred_incomplete = false;
    let max_records = if plan.kind == MuxStreamKind::Partial {
        1
    } else {
        MUX_PAGE_MAX_RECORDS
    };

    while physical_records < max_records && source_bytes < MUX_PAGE_MAX_BYTES {
        let record_hasher = hasher.clone();
        let record = if plan.kind == MuxStreamKind::Partial {
            read_bounded_whole_record(reader, hasher, offset)?
        } else {
            read_bounded_record(reader, hasher, offset)?
        };
        let Some(record) = record else {
            break;
        };
        let rejected_before_record = rejected_records;
        let failure_before_record = first_failure.clone();
        let metadata_failure_for_record = metadata_failure.take();
        if plan.kind == MuxStreamKind::Partial && ordinal != 0 {
            return Err(CaptureError::InvalidPayload(
                "Mux partial cursor exceeds its one-record source".to_owned(),
            ));
        }
        offset = record.end;
        source_bytes = source_bytes.saturating_add(record.observed_bytes);
        physical_records = physical_records.saturating_add(1);
        let line_number = usize::try_from(ordinal)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "Mux source ordinal exceeds platform limits",
            ))?;
        if let Some(error) = metadata_failure_for_record.as_ref() {
            record_rejection(
                line_number,
                error.clone(),
                &mut rejected_records,
                &mut first_failure,
            )?;
        }
        if record.oversized {
            record_rejection(
                line_number,
                format!(
                    "provider record exceeds the {} byte limit (observed {} bytes)",
                    MAX_PROVIDER_JSONL_LINE_BYTES, record.observed_bytes
                ),
                &mut rejected_records,
                &mut first_failure,
            )?;
            ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Mux source ordinal overflowed",
            ))?;
            continue;
        }
        if record.payload.iter().all(u8::is_ascii_whitespace) {
            ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Mux source ordinal overflowed",
            ))?;
            continue;
        }
        let value = match serde_json::from_slice::<Value>(&record.payload) {
            Ok(value) if value.is_object() => value,
            Ok(_) => {
                record_rejection(
                    line_number,
                    "Mux record must contain a JSON object".to_owned(),
                    &mut rejected_records,
                    &mut first_failure,
                )?;
                ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                    "Mux source ordinal overflowed",
                ))?;
                continue;
            }
            Err(error) => {
                if !record.terminated {
                    reader.seek(SeekFrom::Start(record.start))?;
                    *hasher = record_hasher;
                    offset = record.start;
                    physical_records = physical_records.saturating_sub(1);
                    rejected_records = rejected_before_record;
                    first_failure = failure_before_record;
                    metadata_failure = metadata_failure_for_record;
                    deferred_incomplete = true;
                    break;
                }
                record_rejection(
                    line_number,
                    format!("malformed Mux JSON record: {error}"),
                    &mut rejected_records,
                    &mut first_failure,
                )?;
                ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                    "Mux source ordinal overflowed",
                ))?;
                continue;
            }
        };
        if let Some(provider_session_id) = value
            .get("workspaceId")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
        {
            session.provider_session_id =
                bounded_mux_id(provider_session_id.to_owned(), &plan.path, "workspace id")?;
        }
        let row = prepare_core_row(
            value,
            &record,
            ordinal,
            line_number,
            session,
            plan,
            &mut rejected_records,
            &mut first_failure,
        )?;
        rows.push(row);
        ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
            "Mux source ordinal overflowed",
        ))?;
        if plan.kind == MuxStreamKind::Partial {
            break;
        }
    }
    session.metadata_failure = metadata_failure;
    let terminal = reader.fill_buf()?.is_empty();
    if physical_records == 0 && !terminal && !deferred_incomplete {
        return Err(CaptureError::SystemInvariant(
            "Mux page reader made no progress",
        ));
    }
    let next = MuxFrontier {
        version: MUX_FRONTIER_VERSION,
        next_offset: offset,
        next_ordinal: ordinal,
        prefix_sha256: hasher.clone().finalize().into(),
        file_identity: Some(plan.observation.content_identity()),
    };
    Ok(Some(MuxPreparedPage {
        rows,
        next,
        terminal,
        deferred_incomplete,
        rejected_records,
        first_failure,
    }))
}

pub(super) struct MuxRawRecord {
    pub(super) payload: Vec<u8>,
    pub(super) start: u64,
    pub(super) end: u64,
    pub(super) observed_bytes: usize,
    pub(super) oversized: bool,
    pub(super) terminated: bool,
}

pub(super) fn read_bounded_record(
    reader: &mut BufReader<File>,
    hasher: &mut Sha256,
    start: u64,
) -> Result<Option<MuxRawRecord>> {
    let mut payload = Vec::new();
    let mut observed = 0_usize;
    let mut saw_any = false;
    let mut ended = false;
    while !ended {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        saw_any = true;
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index.saturating_add(1));
        let chunk = &available[..take];
        hasher.update(chunk);
        observed = observed.saturating_add(chunk.len());
        if payload.len() <= MAX_PROVIDER_JSONL_LINE_BYTES {
            let remaining = MAX_PROVIDER_JSONL_LINE_BYTES
                .saturating_add(1)
                .saturating_sub(payload.len());
            payload.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        }
        ended = chunk.last() == Some(&b'\n');
        reader.consume(take);
    }
    if !saw_any {
        return Ok(None);
    }
    if payload.last() == Some(&b'\n') {
        payload.pop();
        if payload.last() == Some(&b'\r') {
            payload.pop();
        }
    }
    let end = start
        .checked_add(
            u64::try_from(observed)
                .map_err(|_| CaptureError::SystemInvariant("Mux record size exceeds u64"))?,
        )
        .ok_or(CaptureError::SystemInvariant(
            "Mux source offset overflowed",
        ))?;
    Ok(Some(MuxRawRecord {
        oversized: observed > MAX_PROVIDER_JSONL_LINE_BYTES,
        terminated: ended,
        payload,
        start,
        end,
        observed_bytes: observed,
    }))
}

pub(super) fn read_bounded_whole_record(
    reader: &mut BufReader<File>,
    hasher: &mut Sha256,
    start: u64,
) -> Result<Option<MuxRawRecord>> {
    let mut payload = Vec::new();
    let mut observed = 0_usize;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        let take = available.len();
        hasher.update(available);
        observed = observed.saturating_add(take);
        if payload.len() <= MAX_PROVIDER_JSONL_LINE_BYTES {
            let remaining = MAX_PROVIDER_JSONL_LINE_BYTES
                .saturating_add(1)
                .saturating_sub(payload.len());
            payload.extend_from_slice(&available[..take.min(remaining)]);
        }
        reader.consume(take);
    }
    if observed == 0 {
        return Ok(None);
    }
    let end = start
        .checked_add(
            u64::try_from(observed)
                .map_err(|_| CaptureError::SystemInvariant("Mux partial size exceeds u64"))?,
        )
        .ok_or(CaptureError::SystemInvariant(
            "Mux partial offset overflowed",
        ))?;
    Ok(Some(MuxRawRecord {
        payload,
        start,
        end,
        observed_bytes: observed,
        oversized: observed > MAX_PROVIDER_JSONL_LINE_BYTES,
        terminated: true,
    }))
}

pub(super) fn record_rejection(
    line: usize,
    error: String,
    rejected: &mut u64,
    first_failure: &mut Option<MuxFailure>,
) -> Result<()> {
    *rejected = rejected
        .checked_add(1)
        .ok_or(CaptureError::SystemInvariant(
            "Mux rejection count overflowed",
        ))?;
    if first_failure.is_none() {
        *first_failure = Some(MuxFailure {
            line,
            error: bounded_mux_failure(error),
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn prepare_core_row(
    value: Value,
    record: &MuxRawRecord,
    ordinal: u64,
    line_number: usize,
    session: &MuxBoundedSessionMetadata,
    plan: &MuxSourcePlan,
    rejected_records: &mut u64,
    first_failure: &mut Option<MuxFailure>,
) -> Result<MuxPreparedRow> {
    let started_at = session
        .started_at
        .parse::<DateTime<Utc>>()
        .map_err(|_| CaptureError::InvalidPayload("Mux start time is invalid".to_owned()))?;
    let occurred_at = mux_message_timestamp_opt(&value).unwrap_or(started_at);
    let event_type = mux_event_type(&value);
    let output_projection = matches!(
        event_type,
        ctx_history_core::EventType::ToolOutput | ctx_history_core::EventType::CommandOutput
    )
    .then(|| mux_output_projection(&value))
    .flatten();
    let unaddressable_output = output_projection
        .as_ref()
        .filter(|projection| !projection.body_available)
        .map(|_| {
            let redacted = value
                .get("parts")
                .and_then(Value::as_array)
                .is_some_and(|parts| {
                    parts.iter().any(|part| {
                        part.get("type").and_then(Value::as_str) == Some("dynamic-tool")
                            && part.get("state").and_then(Value::as_str) == Some("output-redacted")
                    })
                });
            if redacted {
                MuxUnaddressableOutput::Redacted
            } else {
                MuxUnaddressableOutput::Missing
            }
        });
    let retain_core_output = output_projection.as_ref().is_some_and(|projection| {
        matches!(
            projection.outcome,
            MuxOutputOutcome::Failure | MuxOutputOutcome::Timeout
        )
    });
    let native_ordinal = mux_native_event_index(plan, record, ordinal)?;
    let role = value
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let native_record_id = mux_event_id(&value, line_number, role, plan.kind.is_partial());
    let complete_message = (event_type == ctx_history_core::EventType::Message)
        .then(|| mux_event_text(&value, event_type));
    let message_content_ref = complete_message
        .as_ref()
        .map(|text| {
            ContentRef::from_bytes(text.as_bytes()).ok_or(CaptureError::SystemInvariant(
                "Mux complete message exceeds ContentRef bounds",
            ))
        })
        .transpose()?;
    let source_locator = mux_record_locator(plan.kind.is_partial(), record.start, record.end)
        .ok_or(CaptureError::SystemInvariant(
            "Mux source record address is invalid",
        ))?;
    let source_record_digest = CompleteContentBodyDigest::from_bytes(&record.payload);
    let row = MuxMessageRow {
        line_number,
        source_path: plan.path.clone(),
        value,
        is_partial: plan.kind.is_partial(),
    };
    let model = session
        .model
        .clone()
        .or_else(|| mux_message_model(&row.value));
    let event = if output_projection.is_none() || retain_core_output {
        let mut event = mux_core_event(native_ordinal, &row, occurred_at, model.as_deref());
        if retain_core_output {
            if let Some(projection) = output_projection.as_ref() {
                apply_mux_core_output_diagnostic(&mut event, &row.value, projection);
            }
        }
        Some(event)
    } else {
        None
    };
    let mut file_touches = Vec::new();
    if matches!(
        event_type,
        ctx_history_core::EventType::ToolCall
            | ctx_history_core::EventType::ToolOutput
            | ctx_history_core::EventType::CommandOutput
            | ctx_history_core::EventType::FileTouched
    ) {
        let limit_exceeded = match visit_provider_file_touch_drafts_with_limit(
            &row.value,
            event_type_supports_structured_file_touches(event_type),
            MUX_MAX_FILE_TOUCHES_PER_EVENT,
            |(_, touch)| {
                file_touches.push(MuxFileTouch { path: touch.path });
                Ok::<(), std::convert::Infallible>(())
            },
        ) {
            Ok(outcome) => outcome.limit_exceeded(),
            Err(never) => match never {},
        };
        if limit_exceeded {
            record_rejection(
                line_number,
                PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned(),
                rejected_records,
                first_failure,
            )?;
        }
    }
    Ok(MuxPreparedRow {
        source_record_ordinal: ordinal,
        source_locator,
        source_record_digest,
        native_record_id,
        message_content_ref,
        unaddressable_output,
        event,
        file_touches,
    })
}

pub(super) fn mux_native_event_index(
    plan: &MuxSourcePlan,
    record: &MuxRawRecord,
    ordinal: u64,
) -> Result<u64> {
    if plan.generation > MUX_MAX_GENERATION {
        return Err(CaptureError::InvalidPayload(
            "Mux source generation exceeds NativePath event identity capacity".to_owned(),
        ));
    }
    let ordinal = if plan.kind.is_partial() {
        mux_partial_event_index(&record.payload) & MUX_MAX_ORDINAL
    } else {
        if ordinal > MUX_MAX_ORDINAL {
            return Err(CaptureError::InvalidPayload(
                "Mux source ordinal exceeds NativePath event identity capacity".to_owned(),
            ));
        }
        ordinal
    };
    Ok(
        (u64::from(plan.kind.is_partial()) * MUX_PARTIAL_NATIVE_ORDINAL)
            | (plan.generation << MUX_ORDINAL_BITS)
            | ordinal,
    )
}
