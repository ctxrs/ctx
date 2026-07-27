use super::*;

pub(crate) fn import_kimi_nativepath_tree(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let inventory = discover_kimi_wire_files(path)?;
    let configured_source_root = inventory.source_root.clone();
    let known_routes = known_kimi_routes(store, &context.machine_id, &configured_source_root)?;
    let sink = options.import_profile.sink().cloned();

    if options.import_profile.is_replay_only() {
        let mut summary = ProviderImportSummary::default();
        record_output_replay(
            &mut summary,
            replay_outputs(
                &inventory.paths,
                &configured_source_root,
                context.imported_at,
                sink.as_deref(),
            )?,
        );
        return Ok(summary);
    }

    if inventory.paths.is_empty() && known_routes.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "no Kimi Code CLI wire.jsonl transcripts found",
        });
    }

    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        let mut changed_groups = 0_usize;
        for wire in &inventory.paths {
            let migrate_source_root = known_routes.iter().any(|route| {
                route.needs_source_root_migration && route.path.as_path() == wire.as_path()
            });
            let mut file_context = context.clone();
            file_context.source_path = Some(wire.clone());
            file_context.source_root = Some(configured_source_root.clone());
            let result = import_kimi_core_file(
                wire,
                store,
                &committed_store,
                &bulk_guard,
                file_context,
                &options,
                &mut changed_groups,
                migrate_source_root,
            )?;
            summary.merge_from(result);
            if summary.work_remaining {
                return Ok(summary);
            }
        }
        summary.merge_from(retire_missing_routes(
            store,
            &bulk_guard,
            &context.machine_id,
            context.imported_at,
            &known_routes,
            &inventory.paths,
            if inventory.root_missing {
                ProviderSourceRouteRetirementReason::RootMissing
            } else {
                ProviderSourceRouteRetirementReason::SourceMissing
            },
        )?);
        Ok(summary)
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    let mut summary = match (operation, finish) {
        (Ok(summary), Ok(())) => summary,
        (_, Err(error)) => return Err(error),
        (Err(error), Ok(())) => return Err(error),
    };
    if !summary.work_remaining {
        let replay = replay_outputs(
            &inventory.paths,
            &configured_source_root,
            context.imported_at,
            sink.as_deref(),
        )?;
        record_output_replay(&mut summary, replay);
    }
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn import_kimi_core_file(
    path: &Path,
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: ProviderAdapterContext,
    options: &ProviderImportOptions,
    changed_groups: &mut usize,
    migrate_source_root: bool,
) -> Result<ProviderImportSummary> {
    let observation = KimiWireObservation::read(path)?;
    let canonical_path = observation.canonical_path().to_path_buf();
    let locator_identity = provider_path_identity(&canonical_path)?;
    let route_sha256 = route_sha256(&locator_identity);
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::KimiCodeCli,
        KIMI_CODE_CLI_SOURCE_FORMAT,
        &locator_identity,
    );
    let committed = load_committed_source(store, &context.machine_id, &stream)?;
    let admission_scope_revision = kimi_admission_scope_revision(&context);
    let source_revision = effective_source_revision(
        &observation.source_revision(&admission_scope_revision),
        options.inventory_observation_token.as_deref(),
    );
    let (mut checkpoint, start_offset, start_ordinal, mut hasher, unchanged) = plan_core_scan(
        &canonical_path,
        &observation,
        route_sha256,
        admission_scope_revision,
        committed.as_ref(),
        &source_revision,
    )?;
    if unchanged && !migrate_source_root {
        return Ok(replay_summary(&checkpoint));
    }

    let mut file = File::open(&canonical_path)?;
    if KimiFrozenFileMetadata::from_metadata(&file.metadata()?)? != *observation.wire() {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    file.seek(SeekFrom::Start(start_offset))?;
    let mut reader = BufReader::new(file);
    let mut offset = start_offset;
    let mut ordinal = start_ordinal;
    let mut page = KimiCorePage::default();
    let mut summary = ProviderImportSummary::default();
    let canonical_identity = provider_path_identity(&canonical_path)?;
    let content_revision =
        observation.complete_content_revision(&checkpoint.admission_scope_revision);
    let mut reached_eof = false;

    while !reached_eof {
        let checkpoint_before = checkpoint.clone();
        let hasher_before = hasher.clone();
        let raw = read_bounded_line(&mut reader, &mut hasher, MAX_PROVIDER_JSONL_LINE_BYTES)?;
        if raw.observed_bytes == 0 {
            reached_eof = true;
        } else if !raw.terminated {
            hasher = hasher_before;
            reached_eof = true;
        } else {
            let byte_start = offset;
            offset =
                offset
                    .checked_add(raw.observed_bytes)
                    .ok_or(CaptureError::SystemInvariant(
                        "Kimi NativePath byte offset overflowed",
                    ))?;
            let line_number = usize::try_from(ordinal)
                .ok()
                .and_then(|value| value.checked_add(1))
                .ok_or(CaptureError::SystemInvariant(
                    "Kimi NativePath line number overflowed",
                ))?;
            let next_ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Kimi NativePath ordinal overflowed",
            ))?;
            checkpoint.complete_offset = offset;
            checkpoint.next_ordinal = next_ordinal;
            checkpoint.committed_prefix_sha256 = prefix_digest(&hasher);
            checkpoint.observed_file_len = observation.wire().length;
            checkpoint.wire_revision = observation.wire().revision_component();
            checkpoint.terminal = false;
            checkpoint.retired = false;

            let (mut units, session_first_observed) = if raw.oversized {
                checkpoint.rejected_records = checkpoint.rejected_records.saturating_add(1);
                (
                    vec![KimiCoreUnit::Rejection {
                        line: line_number,
                        reason: format!(
                            "provider record exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit (observed {} bytes)",
                            raw.observed_bytes
                        ),
                    }],
                    false,
                )
            } else {
                project_core_record(
                    &observation,
                    &context,
                    &canonical_identity,
                    &content_revision,
                    ordinal,
                    line_number,
                    byte_start,
                    offset,
                    json_record_bytes(&raw.bytes),
                    &mut checkpoint,
                )?
            };
            let singleton = KimiCorePage {
                session_first_observed,
                units: units.clone(),
            };
            if units.len() > KIMI_NATIVE_PAGE_MAX_UNITS
                || core_page_retained_bytes(&checkpoint, &singleton)?
                    > NATIVE_PATH_MAX_RETAINED_PAGE_BYTES
            {
                checkpoint.accepted_events = checkpoint_before.accepted_events;
                checkpoint.accepted_file_touches = checkpoint_before.accepted_file_touches;
                checkpoint.rejected_records = checkpoint_before.rejected_records.saturating_add(1);
                units = vec![KimiCoreUnit::Rejection {
                    line: line_number,
                    reason: "Kimi normalized record exceeds the NativePath page bound".to_owned(),
                }];
            }
            if !page.can_push(&checkpoint, &units, session_first_observed)? && !page.is_empty() {
                let pending = page.take();
                let page_summary = publish_core_page(
                    store,
                    committed_store,
                    bulk_guard,
                    &canonical_path,
                    &observation,
                    &context,
                    &source_revision,
                    &stream,
                    &checkpoint_before,
                    options.history_record_id,
                    pending,
                    migrate_source_root,
                )?;
                if page_summary.work_result() == ProviderImportWorkResult::Changed {
                    *changed_groups = changed_groups.saturating_add(1);
                }
                summary.merge_from(page_summary);
                if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                    && *changed_groups != 0
                {
                    summary.work_remaining = true;
                    return Ok(summary);
                }
            }
            page.session_first_observed |= session_first_observed;
            if !page.can_push(&checkpoint, &units, session_first_observed)? {
                return Err(CaptureError::SystemInvariant(
                    "Kimi bounded singleton Core page exceeds its exact retained bound",
                ));
            }
            page.push(units);
            ordinal = next_ordinal;
            if page.units.len() >= KIMI_NATIVE_PAGE_MAX_UNITS {
                let pending = page.take();
                let page_summary = publish_core_page(
                    store,
                    committed_store,
                    bulk_guard,
                    &canonical_path,
                    &observation,
                    &context,
                    &source_revision,
                    &stream,
                    &checkpoint,
                    options.history_record_id,
                    pending,
                    migrate_source_root,
                )?;
                if page_summary.work_result() == ProviderImportWorkResult::Changed {
                    *changed_groups = changed_groups.saturating_add(1);
                }
                summary.merge_from(page_summary);
                if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                    && *changed_groups != 0
                {
                    summary.work_remaining = true;
                    return Ok(summary);
                }
            }
        }
    }

    checkpoint.terminal = offset == observation.wire().length;
    checkpoint.observed_file_len = observation.wire().length;
    let final_page = page.take();
    let page_summary = publish_core_page(
        store,
        committed_store,
        bulk_guard,
        &canonical_path,
        &observation,
        &context,
        &source_revision,
        &stream,
        &checkpoint,
        options.history_record_id,
        final_page,
        migrate_source_root,
    )?;
    if page_summary.work_result() == ProviderImportWorkResult::Changed {
        *changed_groups = changed_groups.saturating_add(1);
    }
    summary.merge_from(page_summary);
    if unchanged {
        summary.failed = summary
            .failed
            .max(usize::try_from(checkpoint.rejected_records).unwrap_or(usize::MAX));
    }
    Ok(summary)
}

pub(super) fn plan_core_scan(
    path: &Path,
    observation: &KimiWireObservation,
    route_sha256: [u8; 32],
    admission_scope_revision: String,
    committed: Option<&KimiCommittedSource>,
    source_revision: &str,
) -> Result<(KimiNativeCheckpoint, u64, u64, Sha256, bool)> {
    let Some(committed) = committed else {
        let checkpoint =
            KimiNativeCheckpoint::initial(route_sha256, observation, admission_scope_revision);
        return Ok((checkpoint, 0, 0, initial_prefix_hasher(), false));
    };
    let Some(previous) = committed.checkpoint.as_ref() else {
        let checkpoint =
            KimiNativeCheckpoint::initial(route_sha256, observation, admission_scope_revision);
        return Ok((checkpoint, 0, 0, initial_prefix_hasher(), false));
    };
    let physical = observation.wire().physical_identity();
    let identity_matches = previous.version == KIMI_NATIVE_CURSOR_VERSION
        && !previous.retired
        && previous.route_sha256 == route_sha256
        && previous.physical_device == physical.0
        && previous.physical_inode == physical.1
        && previous.auxiliary_revision == observation.session.auxiliary_revision
        && previous.admission_scope_revision == admission_scope_revision
        && previous.complete_offset <= observation.wire().length;
    if !identity_matches {
        let checkpoint =
            KimiNativeCheckpoint::initial(route_sha256, observation, admission_scope_revision);
        return Ok((checkpoint, 0, 0, initial_prefix_hasher(), false));
    }
    let Some(hasher) = verify_prefix(path, previous)? else {
        let checkpoint =
            KimiNativeCheckpoint::initial(route_sha256, observation, admission_scope_revision);
        return Ok((checkpoint, 0, 0, initial_prefix_hasher(), false));
    };
    let unchanged = previous.terminal
        && previous.complete_offset == observation.wire().length
        && previous.wire_revision == observation.wire().revision_component()
        && committed.source_revision == source_revision;
    Ok((
        previous.clone(),
        previous.complete_offset,
        previous.next_ordinal,
        hasher,
        unchanged,
    ))
}

pub(super) fn verify_prefix(
    path: &Path,
    checkpoint: &KimiNativeCheckpoint,
) -> Result<Option<Sha256>> {
    let mut file = File::open(path)?;
    let mut hasher = initial_prefix_hasher();
    let mut remaining = checkpoint.complete_offset;
    let mut buffer = [0_u8; 64 * 1024];
    while remaining != 0 {
        let limit = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| CaptureError::SystemInvariant("Kimi prefix length overflowed"))?;
        let read = file.read(&mut buffer[..limit])?;
        if read == 0 {
            return Ok(None);
        }
        hasher.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    Ok((prefix_digest(&hasher) == checkpoint.committed_prefix_sha256).then_some(hasher))
}

// These inputs are the explicit identity, source range, and checkpoint for one wire record;
// bundling them would obscure the provider projection boundary without simplifying ownership.
#[allow(clippy::too_many_arguments)]
pub(super) fn project_core_record(
    observation: &KimiWireObservation,
    context: &ProviderAdapterContext,
    canonical_identity: &str,
    content_revision: &str,
    ordinal: u64,
    line_number: usize,
    byte_start: u64,
    byte_end_exclusive: u64,
    bytes: &[u8],
    checkpoint: &mut KimiNativeCheckpoint,
) -> Result<(Vec<KimiCoreUnit>, bool)> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok((Vec::new(), false));
    }
    let value = match serde_json::from_slice::<Value>(bytes) {
        Ok(value) => value,
        Err(error) => {
            checkpoint.rejected_records = checkpoint.rejected_records.saturating_add(1);
            return Ok((
                vec![KimiCoreUnit::Rejection {
                    line: line_number,
                    reason: format!("malformed JSONL: {error}"),
                }],
                false,
            ));
        }
    };
    let record_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if checkpoint.started_at.is_none() {
        checkpoint.started_at = if record_type == "metadata" {
            value
                .get("created_at")
                .and_then(Value::as_i64)
                .and_then(DateTime::<Utc>::from_timestamp_millis)
        } else {
            kimi_record_timestamp(&value, context.imported_at)
        };
    }
    let session_first_observed = !checkpoint.emitted_session;
    checkpoint.emitted_session = true;
    if record_type == "metadata" {
        return Ok((Vec::new(), session_first_observed));
    }

    let occurred_at =
        kimi_record_timestamp(&value, checkpoint.started_at.unwrap_or(context.imported_at))
            .unwrap_or(context.imported_at);
    let path = observation.canonical_path();
    let event_type = kimi_event_type(record_type, &value);
    let mut event = kimi_event(line_number, &value, occurred_at, path);
    let mut units = Vec::new();
    if event_type == EventType::ToolOutput {
        let output = kimi_output_metadata(&value, line_number, observation.session.cwd.as_deref());
        let retained_failure = matches!(
            output.outcome.outcome,
            OutputOutcome::Failure | OutputOutcome::Timeout
        );
        let content = retained_failure
            .then(|| kimi_output_content(&value))
            .flatten()
            .unwrap_or_default();
        let touch_outcome =
            collect_output_touches(ordinal, line_number, occurred_at, &value, &mut units)?;
        checkpoint.accepted_file_touches = checkpoint
            .accepted_file_touches
            .saturating_add(touch_outcome as u64);
        if !retained_failure {
            return Ok((units, session_first_observed));
        }
        if output.kind == OutputObservationKind::Command {
            event.event_type = EventType::CommandOutput;
        }
        let (preview, _) = provider_local_preview(&content, PROVIDER_MAX_PREVIEW_CHARS);
        event.payload = json!({
            "result_outcome": "failure",
            "output_bytes": content.len(),
            "output_preview": preview,
            "call_id": output.call_id,
            "exit_code": output.outcome.exit_code,
            "duration_ms": output.outcome.duration_ms,
            "timed_out": output.outcome.outcome == OutputOutcome::Timeout,
            "tool": output.command.as_ref().map(|command| command.tool_name.clone()),
            "command": output.command.as_ref().map(|command| command.command.clone()),
            "cwd": output.command.as_ref().and_then(|command| command.working_directory.clone()),
        });
        checkpoint.accepted_events = checkpoint.accepted_events.saturating_add(1);
        units.insert(
            0,
            KimiCoreUnit::Event {
                raw_ordinal: ordinal,
                event,
            },
        );
        return Ok((units, session_first_observed));
    }

    attach_kimi_message_locator(
        &mut event,
        &value,
        bytes,
        byte_start,
        byte_end_exclusive,
        content_revision,
        canonical_identity,
    )?;
    let touch_outcome = kimi_file_touches(
        &value,
        event.event_type,
        event.occurred_at,
        Some(event.provider_event_index),
        event.provider_event_index << 16,
        event_type_supports_structured_file_touches(event.event_type),
    )?;
    if touch_outcome.limit_exceeded() {
        checkpoint.rejected_records = checkpoint.rejected_records.saturating_add(1);
        units.push(KimiCoreUnit::Rejection {
            line: line_number,
            reason: PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned(),
        });
    }
    checkpoint.accepted_events = checkpoint.accepted_events.saturating_add(1);
    checkpoint.accepted_file_touches = checkpoint
        .accepted_file_touches
        .saturating_add(touch_outcome.emitted() as u64);
    units.push(KimiCoreUnit::Event {
        raw_ordinal: ordinal,
        event,
    });
    units.extend(
        touch_outcome
            .touches
            .into_iter()
            .map(KimiCoreUnit::FileTouch),
    );
    Ok((units, session_first_observed))
}

pub(super) fn collect_output_touches(
    ordinal: u64,
    line_number: usize,
    occurred_at: DateTime<Utc>,
    value: &Value,
    units: &mut Vec<KimiCoreUnit>,
) -> Result<usize> {
    let outcome = kimi_file_touches(
        value,
        EventType::ToolOutput,
        occurred_at,
        Some(ordinal),
        ordinal << 16,
        false,
    )?;
    if outcome.limit_exceeded() {
        units.push(KimiCoreUnit::Rejection {
            line: line_number,
            reason: PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned(),
        });
    }
    let emitted = outcome.emitted();
    units.extend(outcome.touches.into_iter().map(KimiCoreUnit::FileTouch));
    Ok(emitted)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    path: &Path,
    observation: &KimiWireObservation,
    context: &ProviderAdapterContext,
    source_revision: &str,
    stream: &str,
    checkpoint: &KimiNativeCheckpoint,
    history_record_id: Option<Uuid>,
    page: KimiCorePage,
    migrate_source_root: bool,
) -> Result<ProviderImportSummary> {
    if !observation.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let current = store.get_sync_cursor(None, &context.machine_id, stream)?;
    let next = kimi_sync_cursor(
        &context.machine_id,
        stream.to_owned(),
        source_revision,
        checkpoint,
        context.imported_at,
    )?;
    let transition =
        NativePathCursorTransition::new(current.as_ref().map(|cursor| cursor.cursor.clone()), next);
    let publication_id = if migrate_source_root {
        format!(
            "{}:source-root-migration-v1",
            core_publication_id(path, &transition, checkpoint)
        )
    } else {
        core_publication_id(path, &transition, checkpoint)
    };
    let retained_bytes = core_page_retained_bytes(checkpoint, &page)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(
        admission,
        NativePathGroupAccounting::new(1, 1, retained_bytes)?,
    )?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = replay_page_summary(&page);
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }

    let raw_source_path = path.display().to_string();
    let source_root = context
        .source_root_display()
        .unwrap_or_else(|| raw_source_path.clone());
    let locator_identity = provider_path_identity(path)?;
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::KimiCodeCli,
        KIMI_CODE_CLI_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Kimi NativePath source has no canonical identity",
    ))?;
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::KimiCodeCli,
            source_format: KIMI_CODE_CLI_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity,
            cursor_stream: stream.to_owned(),
            proposed_source_identity,
            raw_source_path: Some(raw_source_path.clone()),
            source_revision: source_revision.to_owned(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    if !checkpoint.emitted_session {
        let mut summary = ProviderImportSummary::default();
        for unit in &page.units {
            if let KimiCoreUnit::Rejection { line, reason } = unit {
                summary.record_failure(ProviderImportFailure {
                    line: *line,
                    error: reason.clone(),
                });
            }
        }
        if !observation.revalidate(path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        group.prepare_journal_checkpoint()?;
        group.publish_cursor_set()?;
        group.commit()?;
        summary.set_work_result(ProviderImportWorkResult::Changed);
        return Ok(summary);
    }
    let provider_session_id = &observation.session.provider_session_id;
    let source_id = committed_store
        .capture_source_by_canonical_identity_session(
            CaptureProvider::KimiCodeCli,
            KIMI_CODE_CLI_SOURCE_FORMAT,
            &context.machine_id,
            &resolution.canonical_source_identity,
            provider_session_id,
        )?
        .map(|source| source.id)
        .unwrap_or_else(|| {
            native_source_id(&resolution.canonical_source_identity, provider_session_id)
        });
    group.upsert_capture_source(&kimi_capture_source(
        context,
        &observation.session,
        checkpoint,
        source_id,
        &raw_source_path,
        &source_root,
        &resolution.canonical_source_identity,
        source_revision,
    ))?;
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;

    let mut summary = ProviderImportSummary::default();
    let session = canonical_kimi_session(
        committed_store,
        context,
        &observation.session,
        checkpoint,
        history_record_id,
        source_id,
        &resolution.canonical_source_identity,
    )?;
    for (id, external_session_id) in relationship_placeholders(&session, &observation.session) {
        if committed_store.get_session(id).is_err() {
            group.upsert_session(&relationship_placeholder(
                context,
                source_id,
                id,
                external_session_id,
                history_record_id,
                &resolution.canonical_source_identity,
            ))?;
            summary.imported_sessions = summary.imported_sessions.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
    }
    let session_existed = committed_store.get_session(session.id).is_ok();
    group.upsert_session(&session)?;
    if session_existed {
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    } else {
        summary.imported_sessions = summary.imported_sessions.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    if let Some(parent_id) = session.parent_session_id {
        let edge = relationship_edge(
            context,
            source_id,
            &session,
            parent_id,
            &resolution.canonical_source_identity,
        );
        let edge_existed = committed_store.session_edge_exists(edge.id)?;
        group.upsert_projection_neutral_session_edge(&actor(&session), &edge)?;
        if edge_existed {
            summary.skipped_edges = summary.skipped_edges.saturating_add(1);
        } else {
            summary.imported_edges = summary.imported_edges.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
    }

    let mut event_ids = BTreeMap::<u64, Uuid>::new();
    for unit in &page.units {
        match unit {
            KimiCoreUnit::Event { raw_ordinal, event } => {
                let event_id = publish_kimi_event(
                    &mut group,
                    committed_store,
                    context,
                    source_id,
                    &session,
                    history_record_id,
                    *raw_ordinal,
                    event,
                    &mut summary,
                )?;
                event_ids.insert(event.provider_event_index, event_id);
            }
            KimiCoreUnit::FileTouch(touch) => {
                publish_kimi_file_touch(
                    &mut group,
                    committed_store,
                    context,
                    source_id,
                    &session,
                    history_record_id,
                    touch,
                    touch
                        .provider_event_index
                        .and_then(|index| event_ids.get(&index).copied()),
                )?;
                summary.accepted_content_records =
                    summary.accepted_content_records.saturating_add(1);
            }
            KimiCoreUnit::Rejection { line, reason } => {
                summary.record_failure(ProviderImportFailure {
                    line: *line,
                    error: reason.clone(),
                });
            }
        }
    }
    if !observation.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

pub(super) fn kimi_sync_cursor(
    machine_id: &str,
    stream: String,
    source_revision: &str,
    checkpoint: &KimiNativeCheckpoint,
    observed_at: DateTime<Utc>,
) -> Result<SyncCursor> {
    let position = NativePosition::new(
        KIMI_NATIVE_POSITION_KIND,
        serde_json::to_vec(&checkpoint.frontier())?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let cursor = CertifiedProviderCursor::new(
        source_revision,
        KIMI_NATIVE_CAPTURE_REVISION,
        KIMI_NATIVE_POLICY_REVISION,
        position,
        BoundedParserCheckpoint::from_serializable(checkpoint)?,
    )?
    .with_rejected_records(checkpoint.rejected_records);
    certified_provider_sync_cursor(
        CaptureProvider::KimiCodeCli,
        machine_id,
        stream,
        &cursor,
        observed_at,
    )
}

pub(super) fn core_publication_id(
    path: &Path,
    transition: &NativePathCursorTransition,
    checkpoint: &KimiNativeCheckpoint,
) -> String {
    let mut digest = Sha256::new();
    digest.update(KIMI_PUBLICATION_DOMAIN);
    digest.update(path.as_os_str().as_encoded_bytes());
    digest.update(transition.key().stream().as_bytes());
    if let Some(expected) = transition.expected_cursor() {
        digest.update((expected.len() as u64).to_be_bytes());
        digest.update(expected.as_bytes());
    } else {
        digest.update(0_u64.to_be_bytes());
    }
    digest.update((transition.next().cursor.len() as u64).to_be_bytes());
    digest.update(transition.next().cursor.as_bytes());
    digest.update(checkpoint.complete_offset.to_be_bytes());
    format!("kimi-nativepath-v1:{:x}", digest.finalize())
}

pub(super) fn native_source_id(source_identity: &str, provider_session_id: &str) -> Uuid {
    stable_capture_uuid(
        &serde_json::to_string(&(
            "native-path-provider-source-v1",
            CaptureProvider::KimiCodeCli.as_str(),
            KIMI_CODE_CLI_SOURCE_FORMAT,
            source_identity,
            provider_session_id,
        ))
        .expect("Kimi native source identity is serializable"),
        "source",
    )
}
