#[allow(unused_imports)]
use super::*;

pub(crate) fn import_codex_session_paths_fast(
    paths: Vec<PathBuf>,
    store: &mut Store,
    options: CodexSessionImportOptions,
    skipped_by_bounds: usize,
) -> Result<ProviderImportSummary> {
    let mut summary = ProviderImportSummary::default();
    summary.skipped_sessions += skipped_by_bounds;
    summary.skipped += skipped_by_bounds;
    let mut caches = ProviderImportCaches::default();
    let mut in_transaction = false;
    let mut files_in_transaction = 0usize;
    let total_files = paths.len();
    let total_bytes = codex_session_paths_total_bytes(&paths);
    let mut completed_files = 0usize;
    let mut completed_bytes = 0u64;
    report_codex_import_progress(
        &options,
        total_files,
        total_bytes,
        completed_files,
        completed_bytes,
        &summary,
        false,
    );

    for path in paths {
        let file_bytes = fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if !in_transaction {
            store.begin_immediate_batch()?;
            in_transaction = true;
            files_in_transaction = 0;
        }
        if let Err(err) =
            import_codex_session_path_fast(&path, store, &options, &mut summary, &mut caches)
        {
            if in_transaction {
                let _ = store.rollback_batch();
            }
            return Err(err);
        }
        files_in_transaction += 1;
        if files_in_transaction >= CODEX_FAST_IMPORT_TRANSACTION_FILES {
            if let Err(err) = store.commit_batch() {
                let _ = store.rollback_batch();
                return Err(err.into());
            }
            in_transaction = false;
            store.checkpoint_wal_passive_if_larger_than(
                CODEX_FAST_IMPORT_PASSIVE_CHECKPOINT_MIN_BYTES,
            )?;
        }
        completed_files += 1;
        completed_bytes = completed_bytes.saturating_add(file_bytes);
        report_codex_import_progress(
            &options,
            total_files,
            total_bytes,
            completed_files,
            completed_bytes,
            &summary,
            false,
        );
    }

    if !in_transaction {
        store.begin_immediate_batch()?;
        in_transaction = true;
    }
    if let Err(err) = resolve_pending_provider_edges(store, &mut summary, &mut caches) {
        if in_transaction {
            let _ = store.rollback_batch();
        }
        return Err(err);
    }

    if let Err(err) = store.commit_batch() {
        let _ = store.rollback_batch();
        return Err(err.into());
    }
    store.checkpoint_wal_passive_if_larger_than(CODEX_FAST_IMPORT_PASSIVE_CHECKPOINT_MIN_BYTES)?;
    report_codex_import_progress(
        &options,
        total_files,
        total_bytes,
        completed_files,
        completed_bytes,
        &summary,
        true,
    );
    Ok(summary)
}

pub(crate) fn codex_session_paths_total_bytes(paths: &[PathBuf]) -> u64 {
    paths
        .iter()
        .filter_map(|path| fs::metadata(path).ok())
        .fold(0u64, |total, metadata| total.saturating_add(metadata.len()))
}

pub(crate) fn report_codex_import_progress(
    options: &CodexSessionImportOptions,
    total_files: usize,
    total_bytes: u64,
    completed_files: usize,
    completed_bytes: u64,
    summary: &ProviderImportSummary,
    done: bool,
) {
    let Some(callback) = &options.progress else {
        return;
    };
    callback(CodexSessionImportProgress {
        source_path: options.source_path.clone(),
        total_files,
        total_bytes,
        completed_files,
        completed_bytes,
        imported_sessions: summary.imported_sessions,
        imported_events: summary.imported_events,
        imported_edges: summary.imported_edges,
        skipped: summary.skipped,
        failed: summary.failed,
        done,
    });
}

pub(crate) fn import_codex_session_path_fast(
    path: &Path,
    store: &mut Store,
    options: &CodexSessionImportOptions,
    summary: &mut ProviderImportSummary,
    caches: &mut ProviderImportCaches,
) -> Result<()> {
    ensure_regular_provider_transcript_file(path)?;
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let context = ProviderAdapterContext {
        machine_id: options.machine_id.clone(),
        source_path: Some(path.to_path_buf()),
        imported_at: options.imported_at,
        tool_output_mode: options.tool_output_mode,
        event_mode: options.event_mode,
        include_notices: options.include_notices,
    };
    let import_options = NormalizedProviderImportOptions {
        history_record_id: options.history_record_id,
        allow_partial_failures: options.allow_partial_failures,
        persist_cursors: false,
        wrap_transaction: false,
        fast_event_inserts: true,
    };
    let raw_source_path = context
        .source_path
        .as_ref()
        .map(|path| path.display().to_string());

    let mut header = None;
    let mut call_contexts: BTreeMap<String, CodexToolCallContext> = BTreeMap::new();
    let mut line_number = 0usize;
    let mut line = Vec::new();
    while read_provider_jsonl_line(&mut reader, &mut line)? {
        line_number += 1;
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if !should_parse_codex_session_line(&line, options.event_mode) {
            continue;
        }
        if should_skip_codex_tool_output_line(&line, options.tool_output_mode) {
            summary.skipped += 1;
            summary.skipped_events += 1;
            continue;
        }

        let value: Value = match serde_json::from_slice(&line) {
            Ok(value) => value,
            Err(err) => {
                summary.failed += 1;
                summary.failures.push(ProviderImportFailure {
                    line: line_number,
                    error: err.to_string(),
                });
                if !options.allow_partial_failures {
                    return Ok(());
                }
                continue;
            }
        };
        let entry_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        if entry_type == "session_meta" {
            match codex_session_header(value) {
                Ok(parsed) => {
                    let capture = codex_session_capture(
                        &parsed,
                        None,
                        line_number,
                        parsed.timestamp,
                        &context,
                    );
                    let line_summary = import_provider_capture_line(
                        store,
                        &capture,
                        &import_options,
                        line_number,
                        caches,
                    )?;
                    summary.merge(line_summary);
                    call_contexts.clear();
                    header = Some(parsed);
                }
                Err(err) => {
                    summary.failed += 1;
                    summary.failures.push(ProviderImportFailure {
                        line: line_number,
                        error: err.to_string(),
                    });
                    if !options.allow_partial_failures {
                        return Ok(());
                    }
                }
            }
            continue;
        }

        let Some(header) = header.as_ref() else {
            summary.failed += 1;
            summary.failures.push(ProviderImportFailure {
                line: line_number,
                error: "codex session entry appeared before session_meta".to_owned(),
            });
            if !options.allow_partial_failures {
                return Ok(());
            }
            continue;
        };
        let occurred_at = match codex_session_line_timestamp(&value, header.timestamp) {
            Ok(occurred_at) => occurred_at,
            Err(err) => {
                summary.failed += 1;
                summary.failures.push(ProviderImportFailure {
                    line: line_number,
                    error: err.to_string(),
                });
                if !options.allow_partial_failures {
                    return Ok(());
                }
                continue;
            }
        };
        let mut line_capture = codex_session_line_capture(
            header,
            &value,
            &mut call_contexts,
            CodexSessionLineContext {
                line_number,
                occurred_at,
                tool_output_mode: options.tool_output_mode,
                event_mode: options.event_mode,
                raw_source_path: raw_source_path.as_deref(),
            },
        );
        if let Some(event) = line_capture.event.take() {
            if !options.include_notices && event.event_type == EventType::Notice {
                summary.skipped += 1;
                summary.skipped_events += 1;
            } else {
                let line_summary = import_codex_provider_event_fast(
                    store,
                    header,
                    &event,
                    options.history_record_id,
                    line_number,
                    context.imported_at,
                    raw_source_path.as_deref(),
                )?;
                summary.merge(line_summary);
            }
        }
        for (_, file) in line_capture.files_touched {
            import_provider_file_touched_line(store, &file, &import_options)?;
        }
    }
    Ok(())
}

pub(crate) fn import_codex_provider_event_fast(
    store: &mut Store,
    header: &CodexSessionHeader,
    event: &ProviderEventEnvelope,
    history_record_id: Option<Uuid>,
    line_number: usize,
    imported_at: DateTime<Utc>,
    raw_source_path: Option<&str>,
) -> Result<ProviderImportSummary> {
    let mut summary = ProviderImportSummary::default();
    let provider = CaptureProvider::Codex;
    let session_id = provider_session_uuid(provider, &header.id);
    let source_id = provider_scoped_source_uuid(
        provider,
        &header.id,
        CODEX_SESSION_SOURCE_FORMAT,
        raw_source_path,
    );
    let (payload, redacted_payload) = sanitize_value(event.payload.clone());
    let (event_metadata, redacted_metadata) = sanitize_value(event.metadata.clone());
    let event_hash = event
        .provider_event_hash
        .clone()
        .unwrap_or(compute_payload_hash(&payload)?);
    let event_identity = provider_event_import_identity(
        store,
        provider,
        &header.id,
        source_id,
        event.provider_event_index,
        event.provider_event_index,
        &event_hash,
        None,
    )?;
    let command_run = provider_command_run_from_event(ProviderCommandRunInput {
        provider,
        provider_session_id: &header.id,
        session_id,
        source_id,
        run_source_id: event_identity.run_source_id,
        history_record_id,
        event,
        payload: &payload,
        event_hash: &event_hash,
    })?;
    let normalized_event = Event {
        id: event_identity.id,
        seq: event_identity.seq,
        history_record_id,
        session_id: Some(session_id),
        run_id: command_run.as_ref().map(|run| run.id),
        event_type: event.event_type,
        role: event.role,
        occurred_at: event.occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": provider.as_str(),
            "provider_session_id": header.id,
            "provider_event_index": event.provider_event_index,
            "provider_event_hash": event_hash,
            "cursor": event.cursor,
            "artifacts": event.artifacts,
            "body": payload,
        }),
        payload_blob_id: None,
        dedupe_key: Some(event_identity.dedupe_key),
        redaction_state: effective_event_redaction_state(
            event.redaction_state,
            redacted_payload || redacted_metadata,
        ),
        sync: provider_sync_metadata(
            event.fidelity,
            json!({
                "provider_session_id": header.id,
                "provider_event_index": event.provider_event_index,
                "provider_event_hash": event_hash,
                "cursor": event.cursor,
                "source_format": CODEX_SESSION_SOURCE_FORMAT,
                "source_trust": ProviderSourceTrust::ProviderExport,
                "fixture_line": line_number,
                "imported_at": imported_at,
                "event_idempotency_key": event.idempotency_key,
                "metadata": event_metadata,
            }),
        ),
    };

    if let Some(run) = &command_run {
        store.insert_run_if_absent(run)?;
    }
    let inserted = store.insert_event_if_absent(&normalized_event)?;
    if redacted_payload || redacted_metadata {
        summary.redacted += 1;
    }
    if inserted {
        summary.imported_events += 1;
        summary.imported += 1;
    } else {
        summary.skipped_events += 1;
        summary.skipped += 1;
    }
    Ok(summary)
}

pub fn catalog_codex_session_tree(
    root: impl AsRef<Path>,
    store: &Store,
    options: CodexSessionCatalogOptions,
) -> Result<CatalogSummary> {
    let root = root.as_ref();
    let source_root = options
        .source_root
        .as_deref()
        .unwrap_or(root)
        .display()
        .to_string();
    let cataloged_at_ms = options.cataloged_at.timestamp_millis();
    let mut paths = Vec::new();
    collect_jsonl_paths(root, &mut paths)?;
    let skipped_by_bounds = apply_codex_session_import_bounds(
        &mut paths,
        options.max_session_files,
        options.max_total_bytes,
    )?;

    let mut summary = CatalogSummary {
        skipped_sessions: skipped_by_bounds,
        ..CatalogSummary::default()
    };
    let existing = store
        .list_catalog_sessions_for_source(CaptureProvider::Codex, &source_root)?
        .into_iter()
        .map(|session| (session.source_path.clone(), session))
        .collect::<BTreeMap<_, _>>();
    let mut current_paths = Vec::with_capacity(paths.len());
    let mut cached_sessions = Vec::new();
    let mut paths_to_parse = Vec::new();
    let mut metadata_failures = Vec::new();
    for path in paths {
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(err) => {
                summary.failed_sessions += 1;
                metadata_failures.push(format!("{}: {err}", path.display()));
                continue;
            }
        };
        summary.source_files += 1;
        summary.source_bytes = summary.source_bytes.saturating_add(metadata.len());
        let source_path = path.display().to_string();
        current_paths.push(source_path.clone());
        if let Some(session) = cached_catalog_session_if_unchanged(
            existing.get(&source_path),
            &metadata,
            cataloged_at_ms,
        ) {
            summary.cached_sessions += 1;
            cached_sessions.push(session);
        } else {
            paths_to_parse.push(path);
        }
    }
    if !options.allow_partial_failures && !metadata_failures.is_empty() {
        return Err(CaptureError::InvalidPayload(format!(
            "catalog failed: {}",
            metadata_failures.remove(0)
        )));
    }
    let stale_session_count =
        store.catalog_source_stale_session_count(CaptureProvider::Codex, &source_root)?;
    let current_path_set = current_paths.iter().cloned().collect::<BTreeSet<_>>();
    let has_missing_existing_paths = existing
        .keys()
        .any(|source_path| !current_path_set.contains(source_path));
    if paths_to_parse.is_empty()
        && metadata_failures.is_empty()
        && cached_sessions.len() == current_paths.len()
        && existing.len() == current_paths.len()
        && !has_missing_existing_paths
        && stale_session_count == 0
    {
        summary.cataloged_sessions = cached_sessions.len();
        return Ok(summary);
    }
    let (scan_summary, sessions) = catalog_codex_session_paths(
        paths_to_parse,
        &source_root,
        cataloged_at_ms,
        options.allow_partial_failures,
        options.parallelism,
    )?;
    summary.failed_sessions += scan_summary.failed_sessions;
    summary.parsed_sessions += scan_summary.parsed_sessions;
    let parsed_session_count = sessions.len();
    let cached_session_count = cached_sessions.len();
    let mut sessions_to_persist = sessions;
    if stale_session_count > 0 {
        sessions_to_persist.extend(cached_sessions);
    }
    summary.cataloged_sessions = parsed_session_count.saturating_add(cached_session_count);

    store.begin_immediate_batch()?;
    let persist = (|| -> Result<()> {
        if !sessions_to_persist.is_empty() {
            store.upsert_catalog_sessions(&sessions_to_persist)?;
        }
        if stale_session_count > 0 || has_missing_existing_paths {
            store.mark_catalog_source_missing_paths_stale(
                CaptureProvider::Codex,
                &source_root,
                &current_paths,
                cataloged_at_ms,
            )?;
        }
        Ok(())
    })();
    match persist {
        Ok(()) => {
            store.commit_batch()?;
        }
        Err(err) => {
            let _ = store.rollback_batch();
            return Err(err);
        }
    }
    Ok(summary)
}

pub(crate) fn catalog_codex_session_paths(
    paths: Vec<PathBuf>,
    source_root: &str,
    cataloged_at_ms: i64,
    allow_partial_failures: bool,
    requested_parallelism: Option<usize>,
) -> Result<(CatalogSummary, Vec<CatalogSession>)> {
    let parallelism = catalog_parallelism(paths.len(), requested_parallelism);
    let batches = if parallelism <= 1 {
        vec![catalog_codex_session_chunk(
            paths,
            source_root.to_owned(),
            cataloged_at_ms,
        )]
    } else {
        let chunk_size = paths.len().div_ceil(parallelism).max(1);
        thread::scope(|scope| {
            let mut handles = Vec::new();
            for chunk in paths.chunks(chunk_size) {
                let chunk = chunk.to_vec();
                let source_root = source_root.to_owned();
                handles.push(scope.spawn(move || {
                    catalog_codex_session_chunk(chunk, source_root, cataloged_at_ms)
                }));
            }
            let mut batches = Vec::with_capacity(handles.len());
            for handle in handles {
                batches.push(handle.join().unwrap_or_else(|_| {
                    let mut batch = CatalogWorkerBatch::default();
                    batch
                        .failures
                        .push("catalog worker thread panicked".to_owned());
                    batch.summary.failed_sessions += 1;
                    batch
                }));
            }
            batches
        })
    };

    let mut summary = CatalogSummary::default();
    let mut sessions = Vec::new();
    let mut failures = Vec::new();
    for mut batch in batches {
        summary.source_files += batch.summary.source_files;
        summary.source_bytes = summary
            .source_bytes
            .saturating_add(batch.summary.source_bytes);
        summary.parsed_sessions += batch.summary.parsed_sessions;
        summary.failed_sessions += batch.summary.failed_sessions;
        sessions.append(&mut batch.sessions);
        failures.append(&mut batch.failures);
    }
    if !allow_partial_failures && !failures.is_empty() {
        return Err(CaptureError::InvalidPayload(format!(
            "catalog failed: {}",
            failures.remove(0)
        )));
    }
    Ok((summary, sessions))
}
