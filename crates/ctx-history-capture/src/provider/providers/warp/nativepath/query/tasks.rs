use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn scan_tasks(
    conn: &Connection,
    schema: &WarpSqliteSchema,
    hierarchy: &BTreeMap<String, WarpHierarchyNode>,
    builder: &mut WarpNativePageEmitter<'_>,
    counters: &mut WarpNativeCounters,
) -> Result<()> {
    let index = warp_quote_identifier(&schema.task_keyset_index);
    let representable = format!(
        "typeof(t.conversation_id) = 'text' \
         and typeof(t.task_id) = 'text' \
         and typeof(t.task) = 'blob' \
         and typeof(t.last_modified_at) = 'text' \
         and coalesce(octet_length(t.conversation_id), 0) > 0 \
         and coalesce(octet_length(t.task_id), 0) > 0 \
         and coalesce(octet_length(t.task_id), 0) <= {WARP_ORDERING_KEY_MAX_BYTES} \
         and coalesce(octet_length(t.conversation_id), 0) \
             + coalesce(octet_length(t.task_id), 0) \
             + coalesce(octet_length(t.task), 0) \
             + coalesce(octet_length(t.last_modified_at), 0) \
             + {WARP_NATIVE_SQLITE_ROW_OVERHEAD_BYTES} \
             <= {MAX_PROVIDER_SQLITE_VALUE_BYTES}"
    );
    let mut candidates = conn.prepare(&format!(
        "select t.rowid, \
                typeof(t.conversation_id), coalesce(octet_length(t.conversation_id), 0), \
                typeof(t.task_id), coalesce(octet_length(t.task_id), 0), \
                typeof(t.task), coalesce(octet_length(t.task), 0), \
                typeof(t.last_modified_at), coalesce(octet_length(t.last_modified_at), 0), \
                case when {representable} then t.conversation_id end, \
                case when {representable} then t.task_id end, \
                case when {representable} then t.task end, \
                case when {representable} then t.last_modified_at end \
         from agent_tasks t indexed by {index} \
         order by t.task_id collate binary"
    ))?;
    let _guard = SqliteLengthPreflightGuard::new(conn);
    let mut rows = candidates.query([])?;
    while let Some(row) = rows.next()? {
        let candidate = task_candidate_from_row(row)?;
        counters.task_rows = counters.task_rows.saturating_add(1);
        if let Some(rejection) = reject_task_candidate(&candidate)? {
            counters.oversized_task_rows = counters.oversized_task_rows.saturating_add(u64::from(
                rejection.kind == WarpNativeRejectionKind::OversizedTask,
            ));
            builder.record_source(b"task\0", rejected_task_candidate_digest(&candidate)?)?;
            let mut unit = WarpNativeUnit::progress();
            let native_key = rejection.native_key.clone();
            unit.push_rejection(rejection)?;
            builder.push(unit, native_key, counters)?;
            continue;
        }
        hydrate_task_candidate(&candidate, hierarchy, builder, counters)?;
    }
    Ok(())
}

fn task_candidate_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WarpTaskCandidate> {
    Ok(WarpTaskCandidate {
        rowid: row.get(0)?,
        conversation_id: WarpTaskCellMetadata {
            storage_class: row.get(1)?,
            bytes: row.get(2)?,
        },
        task_id: WarpTaskCellMetadata {
            storage_class: row.get(3)?,
            bytes: row.get(4)?,
        },
        task: WarpTaskCellMetadata {
            storage_class: row.get(5)?,
            bytes: row.get(6)?,
        },
        last_modified_at: WarpTaskCellMetadata {
            storage_class: row.get(7)?,
            bytes: row.get(8)?,
        },
        hydrated_conversation_id: row.get(9)?,
        hydrated_task_id: row.get(10)?,
        hydrated_task: row.get(11)?,
        hydrated_last_modified_at: row.get(12)?,
    })
}

fn reject_task_candidate(candidate: &WarpTaskCandidate) -> Result<Option<WarpNativeRejection>> {
    let native_key = format!("rowid:{}", candidate.rowid);
    for (field, metadata, required_storage) in [
        ("conversation_id", &candidate.conversation_id, "text"),
        ("task_id", &candidate.task_id, "text"),
        ("task", &candidate.task, "blob"),
        ("last_modified_at", &candidate.last_modified_at, "text"),
    ] {
        if metadata.storage_class != required_storage {
            return Ok(Some(WarpNativeRejection {
                kind: WarpNativeRejectionKind::TaskRecord,
                native_key,
                reason: format!(
                    "Warp task {field} must use SQLite {} storage (observed {})",
                    required_storage.to_ascii_uppercase(),
                    metadata.storage_class
                ),
            }));
        }
    }
    if candidate.task_id.bytes == 0 {
        return Ok(Some(WarpNativeRejection {
            kind: WarpNativeRejectionKind::TaskRecord,
            native_key,
            reason: "Warp task_id is empty".to_owned(),
        }));
    }
    if candidate.conversation_id.bytes == 0 {
        return Ok(Some(WarpNativeRejection {
            kind: WarpNativeRejectionKind::TaskRecord,
            native_key,
            reason: "Warp task conversation_id is empty".to_owned(),
        }));
    }
    let task_id_bytes = candidate.task_id.observed_bytes("task_id")?;
    if task_id_bytes > WARP_ORDERING_KEY_MAX_BYTES as u64 {
        return Ok(Some(WarpNativeRejection {
            kind: WarpNativeRejectionKind::TaskRecord,
            native_key,
            reason: format!(
                "Warp task_id exceeds {WARP_ORDERING_KEY_MAX_BYTES}-byte native ordering limit \
                 ({task_id_bytes} bytes)"
            ),
        }));
    }
    let observed_bytes = candidate.hydrated_bytes()?;
    let limit = u64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).map_err(|_| {
        CaptureError::SystemInvariant("Warp NativePath SQLite byte limit exceeds u64")
    })?;
    if observed_bytes > limit {
        return Ok(Some(WarpNativeRejection {
            kind: WarpNativeRejectionKind::OversizedTask,
            native_key,
            reason: format!(
                "Warp task row exceeds {MAX_PROVIDER_SQLITE_VALUE_BYTES}-byte hydration limit \
                 ({observed_bytes} bytes)"
            ),
        }));
    }
    Ok(None)
}

impl WarpTaskCellMetadata {
    pub(super) fn observed_bytes(&self, field: &str) -> Result<u64> {
        u64::try_from(self.bytes).map_err(|_| {
            CaptureError::InvalidPayload(format!(
                "Warp task {field} byte count must be nonnegative"
            ))
        })
    }
}

impl WarpTaskCandidate {
    fn hydrated_bytes(&self) -> Result<u64> {
        [
            ("conversation_id", &self.conversation_id),
            ("task_id", &self.task_id),
            ("task", &self.task),
            ("last_modified_at", &self.last_modified_at),
        ]
        .into_iter()
        .try_fold(
            WARP_NATIVE_SQLITE_ROW_OVERHEAD_BYTES,
            |total, (field, cell)| {
                total
                    .checked_add(cell.observed_bytes(field)?)
                    .ok_or(CaptureError::SystemInvariant(
                        "Warp NativePath task row byte count overflowed",
                    ))
            },
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn hydrate_task_candidate(
    candidate: &WarpTaskCandidate,
    hierarchy: &BTreeMap<String, WarpHierarchyNode>,
    builder: &mut WarpNativePageEmitter<'_>,
    counters: &mut WarpNativeCounters,
) -> Result<()> {
    let (Some(conversation_id), Some(task_id), Some(task_blob), Some(last_modified_at)) = (
        candidate.hydrated_conversation_id.as_deref(),
        candidate.hydrated_task_id.as_deref(),
        candidate.hydrated_task.as_deref(),
        candidate.hydrated_last_modified_at.as_deref(),
    ) else {
        return Err(CaptureError::SystemInvariant(
            "Warp task passed preflight without bounded hydrated values",
        ));
    };
    let conversation_value = ValueRef::Text(conversation_id.as_bytes());
    let task_id_value = ValueRef::Text(task_id.as_bytes());
    let task_value = ValueRef::Blob(task_blob);
    let modified_value = ValueRef::Text(last_modified_at.as_bytes());

    // This digest is control-plane evidence only. Output result bytes never
    // enter retained event bodies, hashes, previews, or downstream records.
    let source_values = [
        conversation_value,
        task_id_value,
        task_value,
        modified_value,
    ];
    let evidence_digest = source_row_digest(b"task\0", &source_values)?;
    let record_digest = record_evidence_digest(&source_values)?;
    builder.record_source(b"task\0", evidence_digest)?;

    let task_id = task_id.to_owned();
    let conversation_id = conversation_id.to_owned();
    if !hierarchy.contains_key(&conversation_id) {
        let mut unit = WarpNativeUnit::progress();
        unit.push_rejection(WarpNativeRejection {
            kind: WarpNativeRejectionKind::MissingConversation,
            native_key: task_id.clone(),
            reason: format!("Warp task references missing conversation {conversation_id:?}"),
        })?;
        builder.push(unit, task_id, counters)?;
        return Ok(());
    }
    counters.protobuf_bytes_scanned = counters
        .protobuf_bytes_scanned
        .saturating_add(u64::try_from(task_blob.len()).unwrap_or(u64::MAX));
    let mut task_prefix_unit = WarpNativeUnit::progress();
    let task_modified = match parse_warp_timestamp(last_modified_at) {
        Ok(value) => Some(value),
        Err(error) => {
            task_prefix_unit.push_rejection(WarpNativeRejection {
                kind: WarpNativeRejectionKind::TaskRecord,
                native_key: task_id.clone(),
                reason: error.to_string(),
            })?;
            None
        }
    };
    let decoded = match decode_warp_native_task(task_blob) {
        Ok(decoded) => decoded,
        Err(error) => {
            counters.malformed_task_cells = counters.malformed_task_cells.saturating_add(1);
            task_prefix_unit.push_rejection(WarpNativeRejection {
                kind: WarpNativeRejectionKind::MalformedProtobuf,
                native_key: task_id.clone(),
                reason: format!("failed to decode Warp task protobuf: {error}"),
            })?;
            builder.push(task_prefix_unit, task_id, counters)?;
            return Ok(());
        }
    };
    merge_decode_counters(counters, decoded.counters);
    if let Some(rejection) = prevalidate_message_identities(&task_id, &decoded.messages, counters) {
        counters.duplicate_message_identity_tasks =
            counters.duplicate_message_identity_tasks.saturating_add(1);
        task_prefix_unit.push_rejection(rejection)?;
        builder.push(task_prefix_unit, task_id, counters)?;
        return Ok(());
    }
    let message_count = decoded.messages.len();
    if message_count == 0 {
        builder.push(task_prefix_unit, task_id, counters)?;
        return Ok(());
    }
    for (index, decoded_message) in decoded.messages.into_iter().enumerate() {
        let message_ordinal = decoded_message.message_ordinal;
        let legacy_indexed = decoded_message.legacy_indexed;
        let legacy_provider_event_index = legacy_indexed.then_some(builder.legacy_indexed_events());
        let mut unit = if index == 0 {
            std::mem::replace(&mut task_prefix_unit, WarpNativeUnit::progress())
        } else {
            WarpNativeUnit::progress()
        };
        let WarpDecodedMessage {
            message_id,
            request_id,
            occurred_at,
            legacy_indexed: _,
            payload,
            ..
        } = decoded_message;
        match payload {
            WarpDecodedMessagePayload::Retained(message) => {
                let event = WarpNativeEvent::from_draft(WarpNativeEventDraft {
                    provider_event_index: builder.retained_events(),
                    legacy_provider_event_index,
                    task_rowid: candidate.rowid,
                    conversation_id: conversation_id.clone(),
                    task_id: task_id.clone(),
                    message_id,
                    message_ordinal,
                    event_type: message.event_type,
                    role: message.role,
                    kind: message.kind,
                    request_id,
                    result_outcome: None,
                    call_id: None,
                    occurred_at: occurred_at.or(task_modified),
                    body: message.body,
                    source_record_digest: record_digest.clone(),
                })?;
                record_retained_event_counters(counters, &event);
                if message.tool_call {
                    counters.tool_calls_retained = counters.tool_calls_retained.saturating_add(1);
                }
                unit.push_event(event)?;
            }
            WarpDecodedMessagePayload::Output(output) => {
                let outcome = output.outcome;
                let call_id = output.call_id;
                let tool_name = output.tool_name;
                if matches!(outcome, OutputOutcome::Failure | OutputOutcome::Timeout) {
                    let event = WarpNativeEvent::from_draft(WarpNativeEventDraft {
                        provider_event_index: builder.retained_events(),
                        legacy_provider_event_index,
                        task_rowid: candidate.rowid,
                        conversation_id: conversation_id.clone(),
                        task_id: task_id.clone(),
                        message_id: message_id.clone(),
                        message_ordinal,
                        event_type: ctx_history_core::EventType::ToolOutput,
                        role: Some(ctx_history_core::EventRole::Tool),
                        kind: "tool_call_result",
                        request_id: request_id.clone(),
                        result_outcome: Some(outcome),
                        call_id: call_id.clone(),
                        occurred_at: occurred_at.or(task_modified),
                        body: format!("tool result: {tool_name}"),
                        source_record_digest: record_digest.clone(),
                    })?;
                    record_retained_event_counters(counters, &event);
                    counters.result_events_created =
                        counters.result_events_created.saturating_add(1);
                    unit.push_event(event)?;
                }
            }
            WarpDecodedMessagePayload::Excluded => {}
        }
        builder.push(
            unit,
            format!("{task_id}:message:{message_ordinal}"),
            counters,
        )?;
        if legacy_indexed {
            builder.advance_legacy_index()?;
        }
    }
    Ok(())
}

fn prevalidate_message_identities(
    task_id: &str,
    messages: &[WarpDecodedMessage],
    counters: &mut WarpNativeCounters,
) -> Option<WarpNativeRejection> {
    let mut message_identities = HashSet::new();
    for message in messages {
        let message_identity = message.message_id.as_ref().map_or(
            WarpNativeMessageIdentity::MessageOrdinal(message.message_ordinal),
            |message_id| WarpNativeMessageIdentity::ProviderId(message_id.clone()),
        );
        if !message_identities.insert(message_identity) {
            return Some(WarpNativeRejection {
                kind: WarpNativeRejectionKind::DuplicateMessageIdentity,
                native_key: task_id.to_owned(),
                reason: format!(
                    "Warp task contains duplicate message identity at ordinal {}",
                    message.message_ordinal
                ),
            });
        }
        counters.peak_task_identity_entries = counters
            .peak_task_identity_entries
            .max(u64::try_from(message_identities.len()).unwrap_or(u64::MAX));
    }
    None
}
