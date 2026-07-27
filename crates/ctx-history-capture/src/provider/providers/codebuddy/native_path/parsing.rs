use super::*;

pub(super) fn next_source_page(
    source: &CodeBuddySource,
    cursor: &CodeBuddyNativeCursor,
    context: &ProviderAdapterContext,
) -> Result<Option<CodeBuddyPage>> {
    if cursor.terminal {
        return Ok(None);
    }
    match source.shape {
        CodeBuddySourceShape::Cli => next_cli_page(source, cursor, context).map(Some),
        CodeBuddySourceShape::Extension => next_extension_page(source, cursor, context).map(Some),
    }
}

pub(super) fn next_cli_page(
    source: &CodeBuddySource,
    cursor: &CodeBuddyNativeCursor,
    context: &ProviderAdapterContext,
) -> Result<CodeBuddyPage> {
    let frozen = source.frozen.as_ref().ok_or(CaptureError::SystemInvariant(
        "CodeBuddy CLI page has no frozen source",
    ))?;
    if cursor.next_native_offset > frozen.length {
        return Err(CaptureError::InvalidPayload(
            "CodeBuddy CLI cursor exceeds its source".to_owned(),
        ));
    }
    let file = File::open(&source.path)?;
    if CodeBuddyFrozenFile::from_metadata(&file.metadata()?)? != *frozen {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(cursor.next_native_offset))?;
    let mut next = cursor.clone();
    next.source_revision.clone_from(&source.source_revision);
    next.file_identity = Some(frozen.identity_token());
    next.terminal = false;
    let mut records = Vec::new();
    let mut retained_bytes = 0_usize;
    let mut offset = cursor.next_native_offset;
    let mut reached_eof = false;
    let mut session_title = codebuddy_session_title(source, &next.session)?;

    while records.len() < CODEBUDDY_NATIVE_PAGE_MAX_UNITS {
        let start = offset;
        let record = read_bounded_jsonl_record(
            &mut reader,
            CODEBUDDY_NATIVE_RECORD_MAX_BYTES.min(MAX_PROVIDER_JSONL_LINE_BYTES),
        )?;
        if record.observed_bytes == 0 {
            reached_eof = true;
            break;
        }
        offset = offset
            .checked_add(record.observed_bytes)
            .ok_or(CaptureError::SystemInvariant(
                "CodeBuddy CLI byte offset overflowed",
            ))?;
        let mut payload = record.payload.as_slice();
        if record.newline_terminated && payload.last() == Some(&b'\n') {
            payload = &payload[..payload.len().saturating_sub(1)];
            if payload.last() == Some(&b'\r') {
                payload = &payload[..payload.len().saturating_sub(1)];
            }
        }
        let ordinal = next.next_native_ordinal;
        let physical_line = usize::try_from(ordinal)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "CodeBuddy CLI line exceeds platform limits",
            ))?;
        let record_bytes = payload.to_vec();
        let record_bound = record_bytes.len().saturating_add(4 * 1024);
        if !records.is_empty()
            && retained_bytes.saturating_add(record_bound) > CODEBUDDY_NATIVE_PAGE_MAX_BYTES
        {
            break;
        }

        next.next_native_offset = offset;
        next.next_native_ordinal =
            next.next_native_ordinal
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "CodeBuddy CLI ordinal overflowed",
                ))?;
        if record.oversized {
            record_cursor_failure(
                &mut next,
                physical_line,
                format!(
                    "provider record exceeds the NativePath page bound (observed {} bytes)",
                    record.observed_bytes
                ),
            )?;
            records.push(CodeBuddyRecord {
                native_ordinal: ordinal,
                physical_line,
                byte_start: Some(start),
                byte_end_exclusive: Some(offset),
                native_bytes: Vec::new(),
                classification: CodeBuddyRecordClassification::RejectedRecord,
                output: None,
            });
            retained_bytes = retained_bytes.saturating_add(256);
            continue;
        }
        if record_bytes.iter().all(u8::is_ascii_whitespace) {
            next.skipped_metadata =
                next.skipped_metadata
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "CodeBuddy skipped metadata count overflowed",
                    ))?;
            records.push(CodeBuddyRecord {
                native_ordinal: ordinal,
                physical_line,
                byte_start: Some(start),
                byte_end_exclusive: Some(offset),
                native_bytes: record_bytes,
                classification: CodeBuddyRecordClassification::SkippedMetadata,
                output: None,
            });
            retained_bytes = retained_bytes.saturating_add(record_bound);
            continue;
        }
        let value = match serde_json::from_slice::<Value>(&record_bytes) {
            Ok(value) => value,
            Err(_) if !record.newline_terminated => {
                next.next_native_offset = start;
                next.next_native_ordinal = ordinal;
                next.incomplete_tail = Some(CodeBuddyCursorFailure {
                    line: physical_line,
                    error: bounded_failure(format!(
                        "{}: incomplete trailing JSONL record",
                        source.path.display()
                    )),
                });
                reached_eof = true;
                break;
            }
            Err(error) => {
                record_cursor_failure(
                    &mut next,
                    physical_line,
                    format!("{}: malformed JSONL: {error}", source.path.display()),
                )?;
                records.push(CodeBuddyRecord {
                    native_ordinal: ordinal,
                    physical_line,
                    byte_start: Some(start),
                    byte_end_exclusive: Some(offset),
                    native_bytes: record_bytes,
                    classification: CodeBuddyRecordClassification::RejectedRecord,
                    output: None,
                });
                retained_bytes = retained_bytes.saturating_add(record_bound);
                continue;
            }
        };
        next.session.row_count =
            next.session
                .row_count
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "CodeBuddy CLI row count overflowed",
                ))?;
        update_cli_session(&mut next.session, &value, context.imported_at);
        let (classification, output) = cli_core_row(
            source,
            context,
            &mut next.session,
            &mut session_title,
            ordinal,
            physical_line,
            start,
            offset,
            &record_bytes,
            value,
        )?;
        match &classification {
            CodeBuddyRecordClassification::AcceptedMessage(_) => {
                next.accepted_events =
                    next.accepted_events
                        .checked_add(1)
                        .ok_or(CaptureError::SystemInvariant(
                            "CodeBuddy accepted event count overflowed",
                        ))?;
            }
            CodeBuddyRecordClassification::SkippedMetadata => {
                next.skipped_metadata =
                    next.skipped_metadata
                        .checked_add(1)
                        .ok_or(CaptureError::SystemInvariant(
                            "CodeBuddy skipped metadata count overflowed",
                        ))?;
            }
            CodeBuddyRecordClassification::RejectedRecord => {}
        }
        records.push(CodeBuddyRecord {
            native_ordinal: ordinal,
            physical_line,
            byte_start: Some(start),
            byte_end_exclusive: Some(offset),
            native_bytes: record_bytes,
            classification,
            output,
        });
        retained_bytes = retained_bytes.saturating_add(record_bound);
    }

    if offset == frozen.length {
        reached_eof = true;
    }
    next.terminal = reached_eof;
    next.certified_prefix_sha256 = file_prefix_sha256(&source.path, next.next_native_offset)?;
    retained_bytes = retained_bytes
        .saturating_add(serde_json::to_vec(&next)?.len())
        .saturating_add(serde_json::to_vec(cursor)?.len());
    validate_page_bounds(records.len().max(1), retained_bytes)?;
    Ok(CodeBuddyPage {
        records,
        expected_cursor: cursor.clone(),
        next_cursor: next,
        retained_bytes,
    })
}

pub(super) struct BoundedJsonlRecord {
    observed_bytes: u64,
    payload: Vec<u8>,
    newline_terminated: bool,
    oversized: bool,
}

pub(super) fn read_bounded_jsonl_record(
    reader: &mut impl BufRead,
    payload_limit: usize,
) -> Result<BoundedJsonlRecord> {
    let retained_limit = payload_limit.saturating_add(2);
    let mut observed_bytes = 0_u64;
    let mut payload = Vec::new();
    let mut newline_terminated = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        let consumed = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| {
                newline_terminated = true;
                index.saturating_add(1)
            })
            .unwrap_or(available.len());
        observed_bytes =
            observed_bytes
                .checked_add(consumed as u64)
                .ok_or(CaptureError::SystemInvariant(
                    "CodeBuddy CLI record length overflowed",
                ))?;
        if payload.len() < retained_limit {
            let retained = consumed.min(retained_limit.saturating_sub(payload.len()));
            payload.extend_from_slice(&available[..retained]);
        }
        reader.consume(consumed);
        if newline_terminated {
            break;
        }
    }
    let observed_payload_bytes = observed_bytes.saturating_sub(u64::from(newline_terminated));
    Ok(BoundedJsonlRecord {
        observed_bytes,
        oversized: observed_payload_bytes > payload_limit as u64,
        payload,
        newline_terminated,
    })
}

pub(super) fn codebuddy_session_title(
    source: &CodeBuddySource,
    session: &CodeBuddySessionCheckpoint,
) -> Result<Option<String>> {
    if source.shape == CodeBuddySourceShape::Extension {
        let (metadata, _) = codebuddy_extension_metadata(&source.path, source.session_ordinal)?;
        let metadata = metadata.ok_or(CaptureError::SourceChangedDuringCapture)?;
        if let Some(title) = codebuddy_extension_metadata_text(&metadata, &["name", "title"]) {
            return Ok(Some(title));
        }
    }
    let Some(anchor) = session.generated_title_anchor.as_ref() else {
        return Ok(None);
    };
    let title = match (source.shape, anchor) {
        (
            CodeBuddySourceShape::Cli,
            CodeBuddyGeneratedTitleAnchor::Cli {
                native_ordinal: _,
                byte_start,
                byte_end_exclusive,
                payload_sha256,
            },
        ) => {
            let length =
                byte_end_exclusive
                    .checked_sub(*byte_start)
                    .ok_or(CaptureError::InvalidPayload(
                        "CodeBuddy CLI title anchor has an invalid byte range".to_owned(),
                    ))?;
            if length > CODEBUDDY_NATIVE_RECORD_MAX_BYTES as u64 {
                return Err(CaptureError::InvalidPayload(
                    "CodeBuddy CLI title anchor exceeds its record bound".to_owned(),
                ));
            }
            let mut file = File::open(&source.path)?;
            file.seek(SeekFrom::Start(*byte_start))?;
            let mut record = vec![
                0_u8;
                usize::try_from(length).map_err(|_| {
                    CaptureError::InvalidPayload(
                        "CodeBuddy CLI title anchor exceeds platform limits".to_owned(),
                    )
                })?
            ];
            file.read_exact(&mut record)?;
            if record.last() == Some(&b'\n') {
                record.pop();
                if record.last() == Some(&b'\r') {
                    record.pop();
                }
            }
            if sha256_hex(&record) != *payload_sha256 {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            let value: Value = serde_json::from_slice(&record)?;
            if provider_role(value.get("role").and_then(Value::as_str)) != EventRole::User {
                return Err(CaptureError::InvalidPayload(
                    "CodeBuddy CLI title anchor no longer identifies a user message".to_owned(),
                ));
            }
            codebuddy_title_from_text(&cli_message_text(&value))
        }
        (
            CodeBuddySourceShape::Extension,
            CodeBuddyGeneratedTitleAnchor::Extension { message_index },
        ) => {
            let message_index = usize::try_from(*message_index).map_err(|_| {
                CaptureError::InvalidPayload(
                    "CodeBuddy extension title anchor exceeds platform limits".to_owned(),
                )
            })?;
            let (metadata, _) = codebuddy_extension_metadata(&source.path, source.session_ordinal)?;
            let metadata = metadata.ok_or(CaptureError::SourceChangedDuringCapture)?;
            let message_ref = metadata
                .messages()
                .get(message_index)
                .ok_or(CaptureError::SourceChangedDuringCapture)?;
            let (message_path, frozen) =
                codebuddy_extension_message_file(&metadata.session_dir, message_ref).map_err(
                    |error| match error {
                        CodeBuddyExtensionMessageError::Rejected(error) => {
                            CaptureError::InvalidPayload(error)
                        }
                        CodeBuddyExtensionMessageError::Source(error) => error,
                    },
                )?;
            let record = fs::read(&message_path)?;
            if !frozen.revalidate(&message_path)? {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            let raw_message: Value = serde_json::from_slice(&record)?;
            let role = message_ref
                .get("role")
                .and_then(Value::as_str)
                .or_else(|| raw_message.get("role").and_then(Value::as_str));
            if provider_role(role) != EventRole::User {
                return Err(CaptureError::InvalidPayload(
                    "CodeBuddy extension title anchor no longer identifies a user message"
                        .to_owned(),
                ));
            }
            let decoded = codebuddy_decoded_message(&raw_message);
            codebuddy_title_from_text(&codebuddy_message_text(&decoded, &raw_message))
        }
        _ => {
            return Err(CaptureError::InvalidPayload(
                "CodeBuddy title anchor does not match its source shape".to_owned(),
            ));
        }
    };
    title.map(Some).ok_or_else(|| {
        CaptureError::InvalidPayload(
            "CodeBuddy title anchor no longer resolves to non-empty text".to_owned(),
        )
    })
}

pub(super) fn next_extension_page(
    source: &CodeBuddySource,
    cursor: &CodeBuddyNativeCursor,
    context: &ProviderAdapterContext,
) -> Result<CodeBuddyPage> {
    let (metadata, metadata_summary) =
        codebuddy_extension_metadata(&source.path, source.session_ordinal)?;
    let metadata = metadata.ok_or(CaptureError::InvalidProviderTranscriptPath {
        path: source.path.clone(),
        reason: "CodeBuddy extension session index is unreadable",
    })?;
    let mut next = cursor.clone();
    next.source_revision.clone_from(&source.source_revision);
    next.terminal = false;
    let mut records = Vec::new();
    let mut retained_bytes = 0_usize;
    let mut valid_ordinal = 0_u64;
    let mut reached_end = true;
    let mut session_title = codebuddy_session_title(source, &next.session)?;

    if cursor.next_native_ordinal == 0 {
        for failure in metadata_summary.failures {
            record_cursor_failure(&mut next, failure.line, failure.error)?;
        }
    }

    for (message_index, message_ref) in metadata.messages().iter().enumerate() {
        let (message_path, frozen) =
            match codebuddy_extension_message_file(&metadata.session_dir, message_ref) {
                Ok(value) => value,
                Err(error) => {
                    let error = error.rejected()?;
                    if cursor.next_native_ordinal == 0 {
                        record_cursor_failure(
                            &mut next,
                            codebuddy_extension_line_number(source.session_ordinal, message_index),
                            error,
                        )?;
                    }
                    continue;
                }
            };
        let ordinal = valid_ordinal;
        valid_ordinal = valid_ordinal
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "CodeBuddy extension ordinal overflowed",
            ))?;
        if ordinal < cursor.next_native_ordinal {
            continue;
        }
        if records.len() >= CODEBUDDY_NATIVE_PAGE_MAX_UNITS {
            reached_end = false;
            break;
        }
        let physical_line = codebuddy_extension_line_number(source.session_ordinal, message_index);
        let record_bound = usize::try_from(frozen.length)
            .unwrap_or(usize::MAX)
            .saturating_add(4 * 1024);
        if !records.is_empty()
            && retained_bytes.saturating_add(record_bound) > CODEBUDDY_NATIVE_PAGE_MAX_BYTES
        {
            reached_end = false;
            break;
        }
        next.next_native_ordinal =
            next.next_native_ordinal
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "CodeBuddy extension ordinal overflowed",
                ))?;
        next.session.row_count =
            next.session
                .row_count
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "CodeBuddy extension row count overflowed",
                ))?;
        if frozen.length > CODEBUDDY_NATIVE_RECORD_MAX_BYTES as u64 {
            record_cursor_failure(
                &mut next,
                physical_line,
                format!(
                    "{}: CodeBuddy message JSON exceeds the NativePath page bound",
                    message_path.display()
                ),
            )?;
            records.push(CodeBuddyRecord {
                native_ordinal: ordinal,
                physical_line,
                byte_start: None,
                byte_end_exclusive: None,
                native_bytes: Vec::new(),
                classification: CodeBuddyRecordClassification::RejectedRecord,
                output: None,
            });
            retained_bytes = retained_bytes.saturating_add(256);
            continue;
        }
        let record_bytes = fs::read(&message_path)?;
        if !frozen.revalidate(&message_path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let raw_message = match serde_json::from_slice::<Value>(&record_bytes) {
            Ok(value) => value,
            Err(error) => {
                record_cursor_failure(
                    &mut next,
                    physical_line,
                    format!("{}: json error: {error}", message_path.display()),
                )?;
                records.push(CodeBuddyRecord {
                    native_ordinal: ordinal,
                    physical_line,
                    byte_start: None,
                    byte_end_exclusive: None,
                    native_bytes: record_bytes,
                    classification: CodeBuddyRecordClassification::RejectedRecord,
                    output: None,
                });
                retained_bytes = retained_bytes.saturating_add(record_bound);
                continue;
            }
        };
        let (classification, output) = extension_core_row(
            context,
            &metadata,
            &mut next.session,
            &mut session_title,
            ordinal,
            message_index,
            message_ref,
            &message_path,
            &record_bytes,
            raw_message,
        )?;
        match &classification {
            CodeBuddyRecordClassification::AcceptedMessage(_) => {
                next.accepted_events =
                    next.accepted_events
                        .checked_add(1)
                        .ok_or(CaptureError::SystemInvariant(
                            "CodeBuddy accepted event count overflowed",
                        ))?;
            }
            CodeBuddyRecordClassification::SkippedMetadata => {
                next.skipped_metadata =
                    next.skipped_metadata
                        .checked_add(1)
                        .ok_or(CaptureError::SystemInvariant(
                            "CodeBuddy skipped metadata count overflowed",
                        ))?;
            }
            CodeBuddyRecordClassification::RejectedRecord => {}
        }
        records.push(CodeBuddyRecord {
            native_ordinal: ordinal,
            physical_line,
            byte_start: None,
            byte_end_exclusive: None,
            native_bytes: record_bytes,
            classification,
            output,
        });
        retained_bytes = retained_bytes.saturating_add(record_bound);
    }

    if next.next_native_ordinal < valid_ordinal {
        reached_end = false;
    }
    next.terminal = reached_end;
    next.certified_prefix_sha256 = sha256_hex(source.source_revision.as_bytes());
    retained_bytes = retained_bytes
        .saturating_add(serde_json::to_vec(&next)?.len())
        .saturating_add(serde_json::to_vec(cursor)?.len());
    validate_page_bounds(records.len().max(1), retained_bytes)?;
    Ok(CodeBuddyPage {
        records,
        expected_cursor: cursor.clone(),
        next_cursor: next,
        retained_bytes,
    })
}
