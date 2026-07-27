use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn scan_tasks(
    conn: &Connection,
    schema: &WarpSqliteSchema,
    hierarchy: &BTreeMap<String, WarpHierarchyNode>,
    builder: &mut WarpNativePageEmitter<'_>,
    counters: &mut WarpNativeCounters,
    profile: WarpNativeProfile,
    resume: &WarpNativeFrontier,
) -> Result<()> {
    let index = warp_quote_identifier(&schema.task_keyset_index);
    let mut first_candidate = conn.prepare(&format!(
        "select t.rowid, \
                typeof(t.conversation_id), coalesce(octet_length(t.conversation_id), 0), \
                typeof(t.task_id), coalesce(octet_length(t.task_id), 0), \
                typeof(t.task), coalesce(octet_length(t.task), 0), \
                typeof(t.last_modified_at), coalesce(octet_length(t.last_modified_at), 0) \
         from agent_tasks t indexed by {index} \
         order by t.task_id collate binary limit 1"
    ))?;
    let mut next_candidate = conn.prepare(&format!(
        "select t.rowid, \
                typeof(t.conversation_id), coalesce(octet_length(t.conversation_id), 0), \
                typeof(t.task_id), coalesce(octet_length(t.task_id), 0), \
                typeof(t.task), coalesce(octet_length(t.task), 0), \
                typeof(t.last_modified_at), coalesce(octet_length(t.last_modified_at), 0) \
         from agent_tasks t indexed by {index} \
         where t.task_id collate binary > ( \
                   select previous.task_id from agent_tasks previous \
                   where previous.rowid = ?1 \
               ) \
         order by t.task_id collate binary limit 1"
    ))?;
    let mut resumed_candidate = conn.prepare(&format!(
        "select t.rowid, \
                typeof(t.conversation_id), coalesce(octet_length(t.conversation_id), 0), \
                typeof(t.task_id), coalesce(octet_length(t.task_id), 0), \
                typeof(t.task), coalesce(octet_length(t.task), 0), \
                typeof(t.last_modified_at), coalesce(octet_length(t.last_modified_at), 0) \
         from agent_tasks t indexed by {index} \
         where t.rowid = ?1"
    ))?;
    let mut hydration = conn.prepare(
        "select conversation_id, task_id, task, last_modified_at \
         from agent_tasks where rowid = ?1",
    )?;
    let mut after_rowid = (resume.phase == WarpNativeFrontierPhase::Tasks)
        .then_some(resume.last_task_rowid)
        .flatten();
    let mut resume_inside_task =
        resume.phase == WarpNativeFrontierPhase::Tasks && resume.next_message_ordinal != 0;
    let mut completed_tasks = resume.completed_task_rows;
    let completed_conversations = builder.frontier().completed_conversation_rows;
    let completed_edges = builder.frontier().completed_hierarchy_edges;
    loop {
        let candidate = if resume_inside_task {
            let rowid = after_rowid.ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Warp in-task resume frontier omitted its task rowid".to_owned(),
                )
            })?;
            let _guard = SqliteLengthPreflightGuard::new(conn);
            resumed_candidate
                .query_row([rowid], task_candidate_from_row)
                .optional()?
        } else {
            next_task_candidate(conn, &mut first_candidate, &mut next_candidate, after_rowid)?
        };
        let Some(candidate) = candidate else {
            break;
        };
        let resumed_message_ordinal = resume_inside_task.then_some(resume.next_message_ordinal);
        resume_inside_task = false;
        after_rowid = Some(candidate.rowid);
        counters.task_rows = counters.task_rows.saturating_add(1);
        if let Some(rejection) = reject_task_candidate(&candidate)? {
            counters.oversized_task_rows = counters.oversized_task_rows.saturating_add(u64::from(
                rejection.kind == WarpNativeRejectionKind::OversizedTask,
            ));
            if resumed_message_ordinal.is_some() {
                return Err(CaptureError::InvalidPayload(
                    "Warp resume frontier points inside an invalid task row".to_owned(),
                ));
            }
            builder.record_source(b"task\0", rejected_task_candidate_digest(&candidate)?)?;
            let mut unit = WarpNativeUnit::progress();
            let native_key = rejection.native_key.clone();
            unit.push_rejection(rejection)?;
            completed_tasks = completed_tasks.saturating_add(1);
            builder.push(
                unit,
                WarpNativeFrontier::after_task(
                    completed_conversations,
                    completed_edges,
                    completed_tasks,
                    candidate.rowid,
                ),
                native_key,
                counters,
            )?;
            continue;
        }
        hydrate_task_candidate(
            &mut hydration,
            &candidate,
            hierarchy,
            builder,
            counters,
            profile,
            resumed_message_ordinal,
            completed_tasks,
            completed_conversations,
            completed_edges,
        )?;
        completed_tasks = completed_tasks.saturating_add(1);
    }
    Ok(())
}

fn next_task_candidate(
    conn: &Connection,
    first: &mut Statement<'_>,
    next: &mut Statement<'_>,
    after_rowid: Option<i64>,
) -> Result<Option<WarpTaskCandidate>> {
    let _guard = SqliteLengthPreflightGuard::new(conn);
    match after_rowid {
        Some(rowid) => next
            .query_row([rowid], task_candidate_from_row)
            .optional()
            .map_err(CaptureError::from),
        None => first
            .query_row([], task_candidate_from_row)
            .optional()
            .map_err(CaptureError::from),
    }
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
    hydration: &mut Statement<'_>,
    candidate: &WarpTaskCandidate,
    hierarchy: &BTreeMap<String, WarpHierarchyNode>,
    builder: &mut WarpNativePageEmitter<'_>,
    counters: &mut WarpNativeCounters,
    profile: WarpNativeProfile,
    resumed_message_ordinal: Option<u32>,
    completed_tasks: u64,
    completed_conversations: u64,
    completed_edges: u64,
) -> Result<()> {
    #[cfg(test)]
    trace_native_task_hydration(candidate.rowid);
    let mut rows = hydration.query([candidate.rowid])?;
    let row = rows.next()?.ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "Warp task row {} disappeared during immutable scan",
            candidate.rowid
        ))
    })?;
    let conversation_value = row.get_ref(0)?;
    let task_id_value = row.get_ref(1)?;
    let task_value = row.get_ref(2)?;
    let modified_value = row.get_ref(3)?;

    // This digest is control-plane evidence only. Output result bytes never
    // enter retained event bodies, hashes, previews, or downstream records.
    let source_values = [
        conversation_value,
        task_id_value,
        task_value,
        modified_value,
    ];
    let evidence_digest = source_row_digest(b"task\0", &source_values)?;
    let complete_content_record_digest =
        complete_content_record_digest(candidate.rowid, &source_values)?;
    if resumed_message_ordinal.is_none() {
        builder.record_source(b"task\0", evidence_digest)?;
    }

    let task_id = required_text(task_id_value, "task_id")?.to_owned();
    let conversation_id = required_text(conversation_value, "conversation_id")?.to_owned();
    if !hierarchy.contains_key(&conversation_id) {
        if resumed_message_ordinal.is_some() {
            return Err(CaptureError::InvalidPayload(
                "Warp resume frontier points inside a task with no conversation".to_owned(),
            ));
        }
        let mut unit = WarpNativeUnit::progress();
        unit.push_rejection(WarpNativeRejection {
            kind: WarpNativeRejectionKind::MissingConversation,
            native_key: task_id.clone(),
            reason: format!("Warp task references missing conversation {conversation_id:?}"),
        })?;
        builder.push(
            unit,
            WarpNativeFrontier::after_task(
                completed_conversations,
                completed_edges,
                completed_tasks.saturating_add(1),
                candidate.rowid,
            ),
            task_id,
            counters,
        )?;
        return Ok(());
    }
    let ValueRef::Blob(task_blob) = task_value else {
        return Err(CaptureError::SystemInvariant(
            "Warp task storage changed after metadata preflight",
        ));
    };
    counters.protobuf_bytes_scanned = counters
        .protobuf_bytes_scanned
        .saturating_add(u64::try_from(task_blob.len()).unwrap_or(u64::MAX));
    let mut task_prefix_unit = WarpNativeUnit::progress();
    let task_modified =
        match required_text(modified_value, "last_modified_at").and_then(parse_warp_timestamp) {
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
    let decoded = match decode_warp_native_task(task_blob, profile) {
        Ok(decoded) => decoded,
        Err(error) => {
            if resumed_message_ordinal.is_some() {
                return Err(CaptureError::InvalidPayload(
                    "Warp resume frontier points inside an undecodable task".to_owned(),
                ));
            }
            counters.malformed_task_cells = counters.malformed_task_cells.saturating_add(1);
            task_prefix_unit.push_rejection(WarpNativeRejection {
                kind: WarpNativeRejectionKind::MalformedProtobuf,
                native_key: task_id.clone(),
                reason: format!("failed to decode Warp task protobuf: {error}"),
            })?;
            builder.push(
                task_prefix_unit,
                WarpNativeFrontier::after_task(
                    completed_conversations,
                    completed_edges,
                    completed_tasks.saturating_add(1),
                    candidate.rowid,
                ),
                task_id,
                counters,
            )?;
            return Ok(());
        }
    };
    merge_decode_counters(counters, decoded.counters);
    if let Some(rejection) = prevalidate_message_identities(&task_id, &decoded.messages, counters) {
        if resumed_message_ordinal.is_some() {
            return Err(CaptureError::InvalidPayload(
                "Warp resume frontier points inside a task with duplicate message identity"
                    .to_owned(),
            ));
        }
        counters.duplicate_message_identity_tasks =
            counters.duplicate_message_identity_tasks.saturating_add(1);
        task_prefix_unit.push_rejection(rejection)?;
        builder.push(
            task_prefix_unit,
            WarpNativeFrontier::after_task(
                completed_conversations,
                completed_edges,
                completed_tasks.saturating_add(1),
                candidate.rowid,
            ),
            task_id,
            counters,
        )?;
        return Ok(());
    }
    let message_count = decoded.messages.len();
    if message_count == 0 {
        if resumed_message_ordinal.is_some() {
            return Err(CaptureError::InvalidPayload(
                "Warp resume frontier points inside an empty task".to_owned(),
            ));
        }
        builder.push(
            task_prefix_unit,
            WarpNativeFrontier::after_task(
                completed_conversations,
                completed_edges,
                completed_tasks.saturating_add(1),
                candidate.rowid,
            ),
            task_id,
            counters,
        )?;
        return Ok(());
    }
    if let Some(resume_at) = resumed_message_ordinal {
        if !decoded
            .messages
            .iter()
            .any(|message| message.message_ordinal == resume_at)
        {
            return Err(CaptureError::InvalidPayload(
                "Warp resume frontier message ordinal is absent from its certified task".to_owned(),
            ));
        }
        task_prefix_unit = WarpNativeUnit::progress();
    }
    for (index, decoded_message) in decoded.messages.into_iter().enumerate() {
        let message_ordinal = decoded_message.message_ordinal;
        if resumed_message_ordinal.is_some_and(|resume_at| message_ordinal < resume_at) {
            continue;
        }
        let mut next_frontier = if index.saturating_add(1) == message_count {
            WarpNativeFrontier::after_task(
                completed_conversations,
                completed_edges,
                completed_tasks.saturating_add(1),
                candidate.rowid,
            )
        } else {
            WarpNativeFrontier::in_task(
                completed_conversations,
                completed_edges,
                completed_tasks,
                candidate.rowid,
                message_ordinal.saturating_add(1),
            )
        };
        let legacy_provider_event_index = decoded_message
            .legacy_indexed
            .then_some(builder.frontier().legacy_indexed_events);
        next_frontier.legacy_indexed_events = builder
            .frontier()
            .legacy_indexed_events
            .checked_add(u64::from(decoded_message.legacy_indexed))
            .ok_or(CaptureError::SystemInvariant(
                "Warp released event index frontier overflowed",
            ))?;
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
                    provider_event_index: builder.frontier().retained_events,
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
                    source_record_digest: complete_content_record_digest.clone(),
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
                        provider_event_index: builder.frontier().retained_events,
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
                        source_record_digest: complete_content_record_digest.clone(),
                    })?;
                    record_retained_event_counters(counters, &event);
                    counters.result_events_created =
                        counters.result_events_created.saturating_add(1);
                    unit.push_event(event)?;
                }
                if let Some(pro_payload) = output.pro_payload {
                    match pro_payload {
                        WarpProOutputPayload::Content(content) => {
                            let observation = warp_output_observation(
                                candidate.rowid,
                                completed_tasks,
                                &conversation_id,
                                &task_id,
                                message_ordinal,
                                message_id,
                                request_id,
                                occurred_at.or(task_modified),
                                hierarchy,
                                call_id,
                                outcome,
                                content,
                            )?;
                            if unit.try_push_output(observation) {
                                counters.result_handoffs_created =
                                    counters.result_handoffs_created.saturating_add(1);
                            } else {
                                counters.oversized_output_records =
                                    counters.oversized_output_records.saturating_add(1);
                                unit.push_output_rejection(output_rejection(
                                    WarpNativeOutputRejectionKind::Oversized,
                                    &task_id,
                                    message_ordinal,
                                    format!(
                                        "Warp output observation exceeds the \
                                         {WARP_NATIVE_PAGE_MAX_BYTES}-byte safe-page limit"
                                    ),
                                ))?;
                            }
                        }
                        WarpProOutputPayload::Rejected { kind, reason } => {
                            unit.push_output_rejection(output_rejection(
                                match kind {
                                    WarpOutputLocalFailureKind::Malformed => {
                                        WarpNativeOutputRejectionKind::Malformed
                                    }
                                    WarpOutputLocalFailureKind::Oversized => {
                                        WarpNativeOutputRejectionKind::Oversized
                                    }
                                },
                                &task_id,
                                message_ordinal,
                                reason,
                            ))?;
                        }
                    }
                }
            }
            WarpDecodedMessagePayload::OutputLocalFailure { reason } => {
                unit.push_output_rejection(output_rejection(
                    WarpNativeOutputRejectionKind::Malformed,
                    &task_id,
                    message_ordinal,
                    reason,
                ))?;
            }
            WarpDecodedMessagePayload::Excluded => {}
        }
        builder.push(
            unit,
            next_frontier,
            format!("{task_id}:message:{message_ordinal}"),
            counters,
        )?;
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

#[allow(clippy::too_many_arguments)]
fn warp_output_observation(
    rowid: i64,
    task_ordinal: u64,
    conversation_id: &str,
    task_id: &str,
    message_ordinal: u32,
    message_id: Option<String>,
    request_id: Option<String>,
    occurred_at: Option<DateTime<Utc>>,
    hierarchy: &BTreeMap<String, WarpHierarchyNode>,
    call_id: Option<String>,
    outcome: OutputOutcome,
    content: Vec<u8>,
) -> Result<ProOutputObservation> {
    let mut locator = Vec::with_capacity(12);
    locator.extend_from_slice(&rowid.to_be_bytes());
    locator.extend_from_slice(&message_ordinal.to_be_bytes());
    let hierarchy = hierarchy
        .get(conversation_id)
        .ok_or(CaptureError::SystemInvariant(
            "Warp output conversation disappeared from hierarchy",
        ))?;
    let native_record_id = message_id.unwrap_or_else(|| format!("{task_id}:{message_ordinal}"));
    Ok(ProOutputObservation {
        kind: OutputObservationKind::Tool,
        coordinate: OutputNativeCoordinate {
            unit_key: format!("warp/nativepath/{conversation_id}/{task_id}/{message_ordinal:010}"),
            native_sequence: task_ordinal,
            native_record_id: Some(native_record_id),
            source_record_ordinal: Some(task_ordinal),
            source_record_subrecord_index: Some(message_ordinal),
            byte_start: None,
            byte_end_exclusive: None,
        },
        occurred_at_unix_ms: occurred_at.map(|value| value.timestamp_millis()),
        associations: OutputAssociations {
            direct_session_id: conversation_id.to_owned(),
            root_session_id: hierarchy.root_conversation_id.clone(),
            parent_session_id: hierarchy.parent_conversation_id.clone(),
            provider_session_id: Some(conversation_id.to_owned()),
            agent_id: Some("warp-agent".to_owned()),
            repository: None,
        },
        call_id: call_id.or(request_id),
        command: None,
        outcome: OutputOutcomeMetadata {
            outcome,
            exit_code: None,
            duration_ms: None,
        },
        locator: OutputSourceLocator {
            version: 1,
            kind: WARP_CONTENT_LOCATOR_KIND.to_owned(),
            payload: locator,
        },
        content,
    })
}

fn output_rejection(
    kind: WarpNativeOutputRejectionKind,
    task_id: &str,
    message_ordinal: u32,
    reason: String,
) -> WarpNativeOutputRejection {
    WarpNativeOutputRejection {
        kind,
        native_key: format!("{task_id}:message:{message_ordinal}"),
        reason,
    }
}
