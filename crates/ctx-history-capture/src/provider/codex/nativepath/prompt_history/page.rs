use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PromptHistoryPageOutcome {
    RetainedContent,
    RejectedOnly,
    IgnoredOnly,
    Empty,
}

impl PromptHistoryPageOutcome {
    fn from_page(
        retained_rows: usize,
        failures: usize,
        physical_records: u64,
    ) -> PromptHistoryPageOutcome {
        if retained_rows > 0 {
            Self::RetainedContent
        } else if failures > 0 {
            Self::RejectedOnly
        } else if physical_records > 0 {
            Self::IgnoredOnly
        } else {
            Self::Empty
        }
    }

    pub(super) const fn has_retained_content(self) -> bool {
        matches!(self, Self::RetainedContent)
    }
}

pub(super) struct PromptHistoryScanner {
    reader: BufReader<File>,
    prefix: Sha256,
    offset: u64,
    ordinal: u64,
}

impl PromptHistoryScanner {
    pub(super) fn open(
        authority: &SourceAuthority,
        digest: &SourceDigest,
        start_offset: u64,
        start_ordinal: u64,
        expected_prefix: [u8; 32],
    ) -> Result<Self> {
        let file = open_prompt_history_source(&authority.physical_path)?;
        if FileObservation::from_metadata(&file.metadata()?)? != digest.observation {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let mut reader = BufReader::new(file);
        let mut prefix = Sha256::new();
        hash_prefix_and_seek(&mut reader, &mut prefix, start_offset)?;
        let actual_prefix: [u8; 32] = prefix.clone().finalize().into();
        if actual_prefix != expected_prefix {
            return Err(CaptureError::InvalidPayload(
                "Codex prompt-history committed prefix no longer matches the source".to_owned(),
            ));
        }
        Ok(Self {
            reader,
            prefix,
            offset: start_offset,
            ordinal: start_ordinal,
        })
    }

    pub(super) fn validate_frontier(
        &self,
        expected_offset: u64,
        expected_ordinal: u64,
        expected_prefix: [u8; 32],
    ) -> Result<()> {
        let actual_prefix: [u8; 32] = self.prefix.clone().finalize().into();
        if self.offset != expected_offset
            || self.ordinal != expected_ordinal
            || actual_prefix != expected_prefix
        {
            return Err(CaptureError::SystemInvariant(
                "Codex prompt-history Drain scanner diverged from its committed cursor",
            ));
        }
        Ok(())
    }
}

pub(super) fn prepare_page(
    scanner: &mut PromptHistoryScanner,
    digest: &SourceDigest,
    cursor: &PromptHistoryCursor,
) -> Result<PreparedPage> {
    let start_ordinal = scanner.ordinal;
    let mut rows = Vec::new();
    let mut failures = Vec::new();
    let mut retained_bytes = 0_usize;
    let mut accepted_events = cursor.accepted_events;
    let mut session_runs = cursor.session_runs;
    let mut rejected_records = cursor.rejected_records;
    let mut ignored_records = cursor.ignored_records;
    let mut last_session_hash = cursor.last_session_hash;

    while scanner.ordinal.saturating_sub(start_ordinal) < MAX_PAGE_RECORDS as u64
        && retained_bytes.saturating_add(PAGE_OVERHEAD_BYTES) < MAX_PAGE_BYTES
    {
        let record_start = scanner.offset;
        let prefix_before = scanner.prefix.clone();
        let Some(record) = read_record(&mut scanner.reader, &mut scanner.prefix)? else {
            break;
        };
        scanner.offset = scanner
            .offset
            .checked_add(u64::try_from(record.observed_bytes).map_err(|_| {
                CaptureError::SystemInvariant("Codex prompt-history record length exceeds u64")
            })?)
            .ok_or(CaptureError::SystemInvariant(
                "Codex prompt-history source offset overflowed",
            ))?;
        let line_number = usize::try_from(scanner.ordinal)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "Codex prompt-history line number exceeds platform limits",
            ))?;
        if record.observed_bytes > MAX_PROVIDER_JSONL_LINE_BYTES {
            reject(
                &mut failures,
                &mut rejected_records,
                line_number,
                format!(
                    "provider record exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit (observed {} bytes)",
                    record.observed_bytes
                ),
            )?;
            scanner.ordinal = next_ordinal(scanner.ordinal)?;
            continue;
        }
        if record.bytes.iter().all(u8::is_ascii_whitespace) {
            ignored_records =
                ignored_records
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Codex prompt-history ignored count overflowed",
                    ))?;
            scanner.ordinal = next_ordinal(scanner.ordinal)?;
            continue;
        }
        let parsed = match serde_json::from_slice::<PromptLine>(&record.bytes) {
            Ok(parsed) => parsed,
            Err(error) => {
                reject(
                    &mut failures,
                    &mut rejected_records,
                    line_number,
                    format!(
                        "malformed Codex prompt-history JSON{}: {error}",
                        if record.terminated { "" } else { " at EOF" }
                    ),
                )?;
                scanner.ordinal = next_ordinal(scanner.ordinal)?;
                continue;
            }
        };
        if parsed.session_id.trim().is_empty() {
            reject(
                &mut failures,
                &mut rejected_records,
                line_number,
                "codex history line has empty session_id".to_owned(),
            )?;
            scanner.ordinal = next_ordinal(scanner.ordinal)?;
            continue;
        }
        let Some(occurred_at) = DateTime::from_timestamp(parsed.ts, 0) else {
            reject(
                &mut failures,
                &mut rejected_records,
                line_number,
                format!(
                    "codex history line has invalid unix timestamp {}",
                    parsed.ts
                ),
            )?;
            scanner.ordinal = next_ordinal(scanner.ordinal)?;
            continue;
        };
        let event = prompt_event(scanner.ordinal, line_number, occurred_at, parsed.text);
        let event_hash = compute_payload_hash(&event.payload)?;
        let conservative_bytes = serde_json::to_vec(&event)?.len().saturating_add(2048);
        if conservative_bytes > MAX_PAGE_BYTES {
            reject(
                &mut failures,
                &mut rejected_records,
                line_number,
                "Codex prompt-history event exceeds the bounded Core page".to_owned(),
            )?;
            scanner.ordinal = next_ordinal(scanner.ordinal)?;
            continue;
        }
        if !rows.is_empty()
            && retained_bytes
                .saturating_add(conservative_bytes)
                .saturating_add(PAGE_OVERHEAD_BYTES)
                > MAX_PAGE_BYTES
        {
            scanner.reader.seek(SeekFrom::Start(record_start))?;
            scanner.prefix = prefix_before;
            scanner.offset = record_start;
            break;
        }
        let session_hash: [u8; 32] = Sha256::digest(parsed.session_id.as_bytes()).into();
        if last_session_hash != Some(session_hash) {
            session_runs = session_runs
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Codex prompt-history session-run count overflowed",
                ))?;
            last_session_hash = Some(session_hash);
        }
        accepted_events = accepted_events
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Codex prompt-history accepted count overflowed",
            ))?;
        retained_bytes = retained_bytes.saturating_add(conservative_bytes);
        rows.push(PromptRow {
            session_id: parsed.session_id,
            event,
            event_hash,
        });
        scanner.ordinal = next_ordinal(scanner.ordinal)?;
    }
    let terminal = scanner.reader.fill_buf()?.is_empty();
    if rows.is_empty() && failures.is_empty() && scanner.ordinal == start_ordinal && !terminal {
        return Err(CaptureError::SystemInvariant(
            "Codex prompt-history page reader made no progress",
        ));
    }
    let prefix_sha256: [u8; 32] = scanner.prefix.clone().finalize().into();
    if terminal && prefix_sha256 != revision_bytes(&digest.revision)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let physical_records = scanner.ordinal.saturating_sub(start_ordinal);
    let outcome = PromptHistoryPageOutcome::from_page(rows.len(), failures.len(), physical_records);
    Ok(PreparedPage {
        rows,
        failures,
        outcome,
        retained_bytes,
        next_offset: scanner.offset,
        next_ordinal: scanner.ordinal,
        prefix_sha256,
        accepted_events,
        session_runs,
        rejected_records,
        ignored_records,
        last_session_hash,
        terminal,
    })
}

// The publication boundary keeps cursor, source, page, and summary authorities explicit.
#[allow(clippy::too_many_arguments)]
pub(super) fn publish_core_page(
    store: &Store,
    guard: &ctx_history_store::EventSearchBulkGuard,
    authority: &SourceAuthority,
    digest: &SourceDigest,
    options: &CodexHistoryImportOptions,
    cursor: &mut PromptHistoryCursor,
    page: PreparedPage,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    debug_assert_eq!(page.outcome.has_retained_content(), !page.rows.is_empty());
    let expected = store
        .get_sync_cursor(None, &options.machine_id, &authority.cursor_stream)?
        .map(|value| value.cursor);
    let next_phase = if page.terminal {
        CursorPhase::Retiring {
            after: None,
            missing: false,
        }
    } else {
        CursorPhase::Core {
            next_offset: page.next_offset,
            next_ordinal: page.next_ordinal,
            prefix_sha256: page.prefix_sha256,
        }
    };
    let mut next_cursor = cursor.clone();
    next_cursor.phase = next_phase;
    next_cursor.accepted_events = page.accepted_events;
    next_cursor.session_runs = page.session_runs;
    next_cursor.rejected_records = page.rejected_records;
    next_cursor.ignored_records = page.ignored_records;
    next_cursor.last_session_hash = page.last_session_hash;
    let next = sync_cursor(
        options,
        authority.cursor_stream.clone(),
        next_cursor.encode()?,
    );
    let transition = NativePathCursorTransition::new(expected.clone(), next);
    let publication_id = publication_id(cursor, &transition, "core");
    let retained_bytes = page
        .retained_bytes
        .saturating_add(PAGE_OVERHEAD_BYTES)
        .min(NATIVE_INGESTION_PAGE_MAX_BYTES);
    let accounting = NativePathGroupAccounting::new(1, 1, retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    let classification =
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?;
    if classification == NativePathCursorSetClassification::AllExpected {
        let locator = locator_observation(authority, cursor, options.imported_at);
        let resolution = group.reconcile_provider_source_locator(&locator)?;
        if resolution.canonical_source_identity != cursor.canonical_source_identity {
            return Err(CaptureError::InvalidPayload(
                "Codex prompt-history relocation requires a fresh explicit route".to_owned(),
            ));
        }
        let source = capture_source(
            authority,
            cursor,
            options,
            page.rows.iter().map(|row| row.event.occurred_at).min(),
        );
        group.upsert_capture_source(&source)?;
        group.bind_capture_source_provider_route(
            cursor.capture_source_id,
            &resolution.route_binding(),
        )?;

        let mut sessions = BTreeMap::<String, DateTime<Utc>>::new();
        for row in &page.rows {
            sessions
                .entry(row.session_id.clone())
                .and_modify(|started| *started = (*started).min(row.event.occurred_at))
                .or_insert(row.event.occurred_at);
        }
        let new_session_count = sessions
            .keys()
            .map(|native_id| {
                let session_id =
                    provider_source_session_uuid(&cursor.canonical_source_identity, native_id);
                match store.get_session(session_id) {
                    Ok(_) => Ok(0_usize),
                    Err(StoreError::NotFound(_)) => Ok(1_usize),
                    Err(error) => Err(CaptureError::from(error)),
                }
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .sum::<usize>();
        let mut retained = NativePathRetainedSourceEntities {
            capture_source_ids: vec![cursor.capture_source_id],
            ..NativePathRetainedSourceEntities::default()
        };
        for (native_id, started_at) in sessions {
            let session_id =
                provider_source_session_uuid(&cursor.canonical_source_identity, &native_id);
            group.upsert_session(&session(
                cursor.capture_source_id,
                session_id,
                &native_id,
                started_at,
                options,
            ))?;
            retained.session_ids.push(session_id);
        }
        let mut imported_events = 0_usize;
        for row in &page.rows {
            let session_id =
                provider_source_session_uuid(&cursor.canonical_source_identity, &row.session_id);
            let ordinal = row.event.provider_event_index;
            let line_number = ordinal
                .checked_add(1)
                .and_then(|line| usize::try_from(line).ok())
                .ok_or(CaptureError::SystemInvariant(
                    "Codex prompt-history line number overflowed",
                ))?;
            // Keep the released provider-scoped event identity exactly stable.
            let event_identity_raw_source_path = cursor
                .event_identity_raw_source_path
                .as_deref()
                .unwrap_or(&authority.raw_source_path);
            let event_identity_source_id = provider_scoped_source_uuid(
                CaptureProvider::Codex,
                &row.session_id,
                SOURCE_FORMAT,
                Some(event_identity_raw_source_path),
            );
            let mut identity = provider_source_event_import_identity(
                event_identity_source_id,
                ordinal,
                &row.event_hash,
            );
            identity = avoid_provider_source_event_seq_collision(
                store,
                identity,
                event_identity_source_id,
                ordinal,
                ordinal,
            )?;
            let (event, run) = codex_canonical_event(
                &row.session_id,
                SOURCE_FORMAT,
                ProviderSourceTrust::ProviderExport,
                options.imported_at,
                options.history_record_id,
                cursor.capture_source_id,
                session_id,
                line_number,
                &row.event,
                &row.event_hash,
                ProviderEventHashAuthority::NormalizedPayloadFallback,
                &identity,
            )?;
            if run.is_some() {
                return Err(CaptureError::SystemInvariant(
                    "Codex prompt-history user prompt unexpectedly produced a run",
                ));
            }
            if group.reconcile_provider_event(
                &event,
                ProviderEventHashAuthority::NormalizedPayloadFallback,
            )? {
                imported_events = imported_events.saturating_add(1);
            }
            retained.event_ids.push(event.id);
        }
        group.stage_source_generation_page(&generation_key(authority, cursor), &retained)?;
        group.prepare_journal_checkpoint()?;
        if !digest.observation.revalidate(&authority.physical_path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        group.publish_cursor_set()?;
        summary.imported_events = summary.imported_events.saturating_add(imported_events);
        summary.imported = summary.imported.saturating_add(imported_events);
        summary.accepted_content_records = summary
            .accepted_content_records
            .saturating_add(page.rows.len());
        summary.imported_sessions = summary.imported_sessions.saturating_add(new_session_count);
        summary.imported = summary.imported.saturating_add(new_session_count);
        for failure in page.failures {
            summary.record_failure(failure);
        }
    } else if !digest.observation.revalidate(&authority.physical_path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.commit()?;
    *cursor = next_cursor;
    Ok(())
}

pub(super) fn absorb_fresh_page_without_authority(
    cursor: &mut PromptHistoryCursor,
    page: PreparedPage,
    summary: &mut ProviderImportSummary,
) -> bool {
    debug_assert!(!page.outcome.has_retained_content());
    cursor.phase = if page.terminal {
        CursorPhase::Complete { missing: false }
    } else {
        CursorPhase::Core {
            next_offset: page.next_offset,
            next_ordinal: page.next_ordinal,
            prefix_sha256: page.prefix_sha256,
        }
    };
    cursor.accepted_events = page.accepted_events;
    cursor.session_runs = page.session_runs;
    cursor.rejected_records = page.rejected_records;
    cursor.ignored_records = page.ignored_records;
    cursor.last_session_hash = page.last_session_hash;
    for failure in page.failures {
        summary.record_failure(failure);
    }
    page.terminal
}

pub(super) fn publish_retirement_page(
    store: &Store,
    guard: &ctx_history_store::EventSearchBulkGuard,
    authority: &SourceAuthority,
    options: &CodexHistoryImportOptions,
    cursor: &PromptHistoryCursor,
    after: Option<&RetirementFrontier>,
) -> Result<ctx_history_store::NativePathSourceRetirementPage> {
    let expected = store
        .get_sync_cursor(None, &options.machine_id, &authority.cursor_stream)?
        .ok_or(CaptureError::SystemInvariant(
            "Codex prompt-history retirement lost its cursor",
        ))?;
    let next = sync_cursor(options, authority.cursor_stream.clone(), cursor.encode()?);
    let transition = NativePathCursorTransition::new(Some(expected.cursor.clone()), next);
    let publication_id = publication_id(cursor, &transition, "retire-page");
    let accounting = NativePathGroupAccounting::new(1, 1, PAGE_OVERHEAD_BYTES)?;
    let admission = store.admit_event_search_bulk_group(guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    let classification =
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?;
    let store_after = after.map(RetirementFrontier::to_store);
    let page = if classification == NativePathCursorSetClassification::AllExpected {
        let page = group.retire_source_generation_page(
            &generation_key(authority, cursor),
            store_after.as_ref(),
            RETIREMENT_PAGE_LIMIT,
            options.imported_at.timestamp_millis(),
        )?;
        group.prepare_journal_checkpoint()?;
        group.publish_cursor_set()?;
        page
    } else {
        group.commit()?;
        return publish_retirement_page(store, guard, authority, options, cursor, after);
    };
    group.commit()?;
    Ok(page)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_cursor_advance(
    store: &Store,
    guard: &ctx_history_store::EventSearchBulkGuard,
    authority: &SourceAuthority,
    options: &CodexHistoryImportOptions,
    cursor: &mut PromptHistoryCursor,
    phase: CursorPhase,
    retire_route: bool,
) -> Result<()> {
    let expected = store
        .get_sync_cursor(None, &options.machine_id, &authority.cursor_stream)?
        .ok_or(CaptureError::SystemInvariant(
            "Codex prompt-history retirement lost its cursor",
        ))?;
    let mut next_cursor = cursor.clone();
    next_cursor.phase = phase;
    let missing = matches!(
        next_cursor.phase,
        CursorPhase::Retiring { missing: true, .. } | CursorPhase::Complete { missing: true }
    );
    next_cursor.generation_id = generation_id(
        next_cursor.generation,
        &next_cursor.source_revision,
        missing,
    );
    let next = sync_cursor(
        options,
        authority.cursor_stream.clone(),
        next_cursor.encode()?,
    );
    let transition = NativePathCursorTransition::new(Some(expected.cursor.clone()), next);
    let publication_id = publication_id(cursor, &transition, "retire");
    let accounting = NativePathGroupAccounting::new(1, 1, PAGE_OVERHEAD_BYTES)?;
    let admission = store.admit_event_search_bulk_group(guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    let classification =
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?;
    if classification == NativePathCursorSetClassification::AllExpected {
        if retire_route {
            group.retire_provider_source_route(&route_retirement(
                authority,
                cursor,
                options.imported_at,
            ))?;
        }
        group.prepare_journal_checkpoint()?;
        group.publish_cursor_set()?;
    }
    group.commit()?;
    *cursor = next_cursor;
    Ok(())
}

pub(super) fn retire_disappeared_source(
    store: &Store,
    authority: &SourceAuthority,
    stored: StoredCursor,
    options: &CodexHistoryImportOptions,
) -> Result<ProviderImportSummary> {
    let StoredCursor::Native { mut cursor } = stored else {
        return Err(CaptureError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "Codex prompt-history source does not exist: {}",
                authority.physical_path.display()
            ),
        )));
    };
    cursor.validate_route(authority)?;
    if matches!(cursor.phase, CursorPhase::Complete { missing: true }) {
        return Ok(replay_summary(&cursor));
    }
    ensure_active_journal(store)?;
    let guard = store.begin_event_search_bulk_mode()?;
    let result = publish_cursor_advance(
        store,
        &guard,
        authority,
        options,
        &mut cursor,
        CursorPhase::Complete { missing: true },
        true,
    );
    let finish = store.finish_event_search_bulk_mode(&guard);
    result?;
    finish?;
    let mut summary = ProviderImportSummary::default();
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}
