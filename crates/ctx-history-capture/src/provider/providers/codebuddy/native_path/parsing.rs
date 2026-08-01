use super::*;

pub(super) fn initial_state(
    source: &CodeBuddySource,
    _context: &ProviderAdapterContext,
) -> Result<CodeBuddyScanState> {
    let session = match source.shape {
        CodeBuddySourceShape::Cli => CodeBuddySessionState {
            native_session_id: source
                .canonical_path
                .file_stem()
                .and_then(|name| name.to_str())
                .filter(|name| !name.trim().is_empty())
                .unwrap_or("unknown-session")
                .to_owned(),
            project_hash: cli_project_hash(&source.canonical_path),
            ..CodeBuddySessionState::default()
        },
        CodeBuddySourceShape::Extension => {
            let metadata = source
                .capability
                .as_ref()
                .and_then(|capability| capability.extension.as_ref())
                .map(|extension| &extension.metadata)
                .ok_or(CaptureError::SystemInvariant(
                    "CodeBuddy extension source lost its admitted metadata",
                ))?;
            CodeBuddySessionState {
                native_session_id: metadata.native_session_id.clone(),
                project_hash: metadata.project_hash.clone(),
                cwd: None,
                generated_title: None,
                row_count: 0,
            }
        }
    };
    Ok(CodeBuddyScanState {
        shape: source.shape,
        source_revision: source.source_revision.clone(),
        next_native_offset: 0,
        next_native_ordinal: 0,
        certified_prefix_sha256: sha256_hex(&[]),
        file_identity: source
            .frozen
            .as_ref()
            .map(CodeBuddyFrozenFile::identity_token),
        terminal: false,
        accepted_events: 0,
        skipped_metadata: 0,
        rejected_records: 0,
        failures: Vec::new(),
        incomplete_tail: None,
        session,
    })
}

pub(super) fn next_source_page(
    source: &CodeBuddySource,
    state: &CodeBuddyScanState,
    context: &ProviderAdapterContext,
) -> Result<Option<CodeBuddyPage>> {
    if state.terminal {
        return Ok(None);
    }
    match source.shape {
        CodeBuddySourceShape::Cli => next_cli_page(source, state, context).map(Some),
        CodeBuddySourceShape::Extension => next_extension_page(source, state, context).map(Some),
    }
}

pub(super) fn next_cli_page(
    source: &CodeBuddySource,
    state: &CodeBuddyScanState,
    context: &ProviderAdapterContext,
) -> Result<CodeBuddyPage> {
    let frozen = source.frozen.as_ref().ok_or(CaptureError::SystemInvariant(
        "CodeBuddy CLI page has no frozen source",
    ))?;
    if state.next_native_offset > frozen.length {
        return Err(CaptureError::InvalidPayload(
            "CodeBuddy CLI state exceeds its source".to_owned(),
        ));
    }
    let file = source
        .capability
        .as_ref()
        .and_then(|capability| capability.primary.as_ref())
        .ok_or(CaptureError::SystemInvariant(
            "CodeBuddy CLI source lost its admitted file",
        ))?
        .file()
        .try_clone()?;
    if CodeBuddyFrozenFile::from_metadata(&file.metadata()?)? != *frozen {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(state.next_native_offset))?;
    let mut next = state.clone();
    next.source_revision.clone_from(&source.source_revision);
    next.file_identity = Some(frozen.identity_token());
    next.terminal = false;
    let mut records = Vec::new();
    let mut retained_bytes = 0_usize;
    let mut offset = state.next_native_offset;
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
            record_scan_rejection(
                &mut next,
                physical_line,
                format!(
                    "provider record exceeds the NativePath page bound (observed {} bytes)",
                    record.observed_bytes
                ),
            )?;
            records.push(CodeBuddyRecord {
                native_ordinal: ordinal,
                native_bytes: Vec::new(),
                classification: CodeBuddyRecordClassification::RejectedRecord,
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
                native_bytes: record_bytes,
                classification: CodeBuddyRecordClassification::SkippedMetadata,
            });
            retained_bytes = retained_bytes.saturating_add(record_bound);
            continue;
        }
        let value = match serde_json::from_slice::<Value>(&record_bytes) {
            Ok(value) => value,
            Err(_) if !record.newline_terminated => {
                next.next_native_offset = start;
                next.next_native_ordinal = ordinal;
                next.incomplete_tail = Some(CodeBuddyScanRejection {
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
                record_scan_rejection(
                    &mut next,
                    physical_line,
                    format!("{}: malformed JSONL: {error}", source.path.display()),
                )?;
                records.push(CodeBuddyRecord {
                    native_ordinal: ordinal,
                    native_bytes: record_bytes,
                    classification: CodeBuddyRecordClassification::RejectedRecord,
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
        update_cli_session(&mut next.session, &value);
        let classification = cli_core_row(
            context,
            &mut next.session,
            &mut session_title,
            physical_line,
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
            native_bytes: record_bytes,
            classification,
        });
        retained_bytes = retained_bytes.saturating_add(record_bound);
    }

    if offset == frozen.length {
        reached_eof = true;
    }
    next.terminal = reached_eof;
    next.certified_prefix_sha256 = source_prefix_sha256(source, next.next_native_offset)?;
    retained_bytes = retained_bytes.saturating_add(next.estimated_bytes());
    validate_page_bounds(records.len().max(1), retained_bytes)?;
    Ok(CodeBuddyPage {
        records,
        next_state: next,
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
    session: &CodeBuddySessionState,
) -> Result<Option<String>> {
    if source.shape == CodeBuddySourceShape::Extension {
        let metadata = source
            .capability
            .as_ref()
            .and_then(|capability| capability.extension.as_ref())
            .map(|extension| &extension.metadata)
            .ok_or(CaptureError::SystemInvariant(
                "CodeBuddy extension source lost its admitted metadata",
            ))?;
        if let Some(title) = codebuddy_extension_metadata_text(metadata, &["name", "title"]) {
            return Ok(Some(title));
        }
    }
    Ok(session.generated_title.clone())
}

pub(super) fn next_extension_page(
    source: &CodeBuddySource,
    state: &CodeBuddyScanState,
    context: &ProviderAdapterContext,
) -> Result<CodeBuddyPage> {
    let admitted = source
        .capability
        .as_ref()
        .and_then(|capability| capability.extension.as_ref())
        .ok_or(CaptureError::SystemInvariant(
            "CodeBuddy extension source lost its admitted metadata",
        ))?;
    let metadata = &admitted.metadata;
    let mut next = state.clone();
    next.source_revision.clone_from(&source.source_revision);
    next.terminal = false;
    let mut records = Vec::new();
    let mut retained_bytes = 0_usize;
    let mut valid_ordinal = 0_u64;
    let mut reached_end = true;
    let mut session_title = codebuddy_session_title(source, &next.session)?;

    for (message_index, message_ref) in metadata.messages().iter().enumerate() {
        let Some(message_id) = message_ref
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| provider_safe_path_segment(id))
        else {
            if state.next_native_ordinal == 0 {
                record_scan_rejection(
                    &mut next,
                    codebuddy_extension_line_number(source.session_ordinal, message_index),
                    "CodeBuddy message ref has an invalid id".to_owned(),
                )?;
            }
            continue;
        };
        let admitted_message = admitted
            .messages
            .get(message_id)
            .ok_or(CaptureError::SourceChangedDuringCapture)?;
        let message_path = &admitted_message.display_path;
        let frozen = &admitted_message.frozen;
        let ordinal = valid_ordinal;
        valid_ordinal = valid_ordinal
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "CodeBuddy extension ordinal overflowed",
            ))?;
        if ordinal < state.next_native_ordinal {
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
            record_scan_rejection(
                &mut next,
                physical_line,
                format!(
                    "{}: CodeBuddy message JSON exceeds the NativePath page bound",
                    message_path.display()
                ),
            )?;
            records.push(CodeBuddyRecord {
                native_ordinal: ordinal,
                native_bytes: Vec::new(),
                classification: CodeBuddyRecordClassification::RejectedRecord,
            });
            retained_bytes = retained_bytes.saturating_add(256);
            continue;
        }
        let capability = source
            .capability
            .as_ref()
            .ok_or(CaptureError::SystemInvariant(
                "CodeBuddy extension source lost its authority",
            ))?;
        let record_bytes = read_observed_file(
            &capability.authority,
            admitted_message,
            CODEBUDDY_NATIVE_RECORD_MAX_BYTES,
        )?;
        let raw_message = match serde_json::from_slice::<Value>(&record_bytes) {
            Ok(value) => value,
            Err(error) => {
                record_scan_rejection(
                    &mut next,
                    physical_line,
                    format!("{}: json error: {error}", message_path.display()),
                )?;
                records.push(CodeBuddyRecord {
                    native_ordinal: ordinal,
                    native_bytes: record_bytes,
                    classification: CodeBuddyRecordClassification::RejectedRecord,
                });
                retained_bytes = retained_bytes.saturating_add(record_bound);
                continue;
            }
        };
        let classification = extension_core_row(
            context,
            metadata,
            &mut next.session,
            &mut session_title,
            message_ref,
            Some(frozen.modified()),
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
            native_bytes: record_bytes,
            classification,
        });
        retained_bytes = retained_bytes.saturating_add(record_bound);
    }

    if next.next_native_ordinal < valid_ordinal {
        reached_end = false;
    }
    next.terminal = reached_end;
    next.certified_prefix_sha256 = sha256_hex(source.source_revision.as_bytes());
    retained_bytes = retained_bytes.saturating_add(next.estimated_bytes());
    validate_page_bounds(records.len().max(1), retained_bytes)?;
    Ok(CodeBuddyPage {
        records,
        next_state: next,
    })
}

fn source_prefix_sha256(source: &CodeBuddySource, length: u64) -> Result<String> {
    let opened = source
        .capability
        .as_ref()
        .and_then(|capability| capability.primary.as_ref())
        .ok_or(CaptureError::SystemInvariant(
            "CodeBuddy CLI source lost its admitted file",
        ))?;
    let mut file = opened.file().try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let mut remaining = length;
    let mut buffer = [0_u8; 64 * 1024];
    let mut digest = Sha256::new();
    while remaining != 0 {
        let wanted = usize::try_from(remaining.min(buffer.len() as u64)).map_err(|_| {
            CaptureError::SystemInvariant("CodeBuddy prefix length exceeds platform limits")
        })?;
        let read = file.read(&mut buffer[..wanted])?;
        if read == 0 {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        digest.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    Ok(hex(&digest.finalize()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    hex(&digest.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
