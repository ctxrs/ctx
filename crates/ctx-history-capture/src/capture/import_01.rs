#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderImportSummary {
    pub imported: usize,
    pub skipped: usize,
    pub failed: usize,
    pub redacted: usize,
    pub imported_sessions: usize,
    pub skipped_sessions: usize,
    pub imported_events: usize,
    pub skipped_events: usize,
    pub imported_edges: usize,
    pub skipped_edges: usize,
    pub failures: Vec<ProviderImportFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderImportFailure {
    pub line: usize,
    pub error: String,
}

#[derive(Debug, Clone, Default)]
pub struct ProviderNormalizationResult {
    pub summary: ProviderImportSummary,
    pub captures: Vec<(usize, ProviderCaptureEnvelope)>,
    pub files_touched: Vec<(usize, ProviderFileTouchedEnvelope)>,
}

impl ProviderImportSummary {
    pub(crate) fn merge(&mut self, other: ProviderImportSummary) {
        self.imported += other.imported;
        self.skipped += other.skipped;
        self.failed += other.failed;
        self.redacted += other.redacted;
        self.imported_sessions += other.imported_sessions;
        self.skipped_sessions += other.skipped_sessions;
        self.imported_events += other.imported_events;
        self.skipped_events += other.skipped_events;
        self.imported_edges += other.imported_edges;
        self.skipped_edges += other.skipped_edges;
        self.failures.extend(other.failures);
    }
}

pub(crate) fn import_parallelism(path_count: usize) -> usize {
    if path_count <= 1 {
        return 1;
    }
    thread::available_parallelism()
        .ok()
        .map(usize::from)
        .unwrap_or(1)
        .min(path_count)
        .min(8)
}

pub fn import_continue_cli_sessions(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: ContinueCliImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = ContinueCliSessionsAdapter.normalize_path(
        path,
        &ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            imported_at: options.imported_at,
            tool_output_mode: CodexToolOutputMode::Full,
            event_mode: CodexEventImportMode::Rich,
            include_notices: true,
        },
    )?;
    import_normalized_provider_captures(
        store,
        normalization,
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
        },
    )
}

pub fn import_normalized_provider_captures(
    store: &mut Store,
    normalization: ProviderNormalizationResult,
    options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let ProviderNormalizationResult {
        summary,
        captures,
        files_touched,
    } = normalization;
    import_provider_capture_lines(store, options, summary, captures, files_touched)
}

pub(crate) fn push_provider_import_failure(
    summary: &mut ProviderImportSummary,
    line: usize,
    error: String,
) {
    summary.failed += 1;
    summary.failures.push(ProviderImportFailure { line, error });
}

pub(crate) fn read_task_json_optional(
    summary: &mut ProviderImportSummary,
    task_dir: &Path,
    file_name: &str,
    context: &ProviderAdapterContext,
    line: usize,
) -> Option<Value> {
    let path = task_dir.join(file_name);
    if !path.exists() {
        return None;
    }
    match read_task_json_value(&path, context) {
        Ok(value) => Some(value),
        Err(err) => {
            summary.failed += 1;
            summary.failures.push(ProviderImportFailure {
                line,
                error: format!("{file_name}: {err}"),
            });
            None
        }
    }
}

pub(crate) fn import_provider_capture_lines(
    store: &mut Store,
    options: NormalizedProviderImportOptions,
    mut summary: ProviderImportSummary,
    captures: Vec<(usize, ProviderCaptureEnvelope)>,
    mut files_touched: Vec<(usize, ProviderFileTouchedEnvelope)>,
) -> Result<ProviderImportSummary> {
    let mut caches = ProviderImportCaches::default();
    let supplied_file_touch_lines = files_touched
        .iter()
        .map(|(line_number, _)| *line_number)
        .collect::<BTreeSet<_>>();
    for (line_number, capture) in &captures {
        if capture.provider == CaptureProvider::Codex {
            continue;
        }
        if supplied_file_touch_lines.contains(line_number) {
            continue;
        }
        if let Some(event) = &capture.event {
            files_touched.extend(provider_file_touches_from_event(
                capture.provider,
                &capture.session.provider_session_id,
                &capture.source.source_format,
                capture.source.raw_source_path.as_deref(),
                event,
                *line_number,
            ));
        }
    }
    let has_captures = !captures.is_empty() || !files_touched.is_empty();

    if summary.failed > 0 && !options.allow_partial_failures {
        return Ok(summary);
    }

    if has_captures && options.wrap_transaction {
        store.begin_immediate_batch()?;
    }
    for (line_number, capture) in captures {
        match import_provider_capture_line(store, &capture, &options, line_number, &mut caches) {
            Ok(line_summary) => summary.merge(line_summary),
            Err(err) => {
                summary.failed += 1;
                summary.failures.push(ProviderImportFailure {
                    line: line_number,
                    error: err.to_string(),
                });
            }
        }
    }
    if let Err(err) = resolve_pending_provider_edges(store, &mut summary, &mut caches) {
        if has_captures && options.wrap_transaction {
            let _ = store.rollback_batch();
        }
        return Err(err);
    }
    for (line_number, file) in files_touched {
        if let Err(err) = import_provider_file_touched_line(store, &file, &options) {
            summary.failed += 1;
            summary.failures.push(ProviderImportFailure {
                line: line_number,
                error: err.to_string(),
            });
        }
    }
    if summary.failed > 0 && !options.allow_partial_failures {
        if has_captures && options.wrap_transaction {
            let _ = store.rollback_batch();
        }
        return Ok(summary);
    }
    if has_captures && options.wrap_transaction {
        if let Err(err) = store.commit_batch() {
            let _ = store.rollback_batch();
            return Err(err.into());
        }
    }

    Ok(summary)
}

pub(crate) fn import_provider_capture_line(
    store: &mut Store,
    capture: &ProviderCaptureEnvelope,
    options: &NormalizedProviderImportOptions,
    line_number: usize,
    caches: &mut ProviderImportCaches,
) -> Result<ProviderImportSummary> {
    if capture.schema_version != PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION {
        return Err(CaptureError::InvalidPayload(format!(
            "unsupported provider capture envelope schema version {} on line {line_number}",
            capture.schema_version
        )));
    }

    let mut summary = ProviderImportSummary::default();
    let provider = capture.provider;
    let session = &capture.session;
    let source = &capture.source;
    let imported_at = source.observed_at;
    let session_id = provider_session_uuid(provider, &session.provider_session_id);
    let source_identity_key = provider_scoped_source_identity_key(
        provider,
        &session.provider_session_id,
        &source.source_format,
        source.raw_source_path.as_deref(),
    );
    let source_id = stable_capture_uuid(&source_identity_key, "source");
    let requested_parent_session_id = session
        .parent_provider_session_id
        .as_ref()
        .map(|id| provider_session_uuid(provider, id));
    let parent_session_id = match requested_parent_session_id {
        Some(parent_id)
            if provider_session_exists_cached(store, parent_id, &mut caches.session_exists)? =>
        {
            Some(parent_id)
        }
        _ => None,
    };
    let requested_root_session_id = session
        .root_provider_session_id
        .as_ref()
        .map(|id| provider_session_uuid(provider, id))
        .or_else(|| requested_parent_session_id.map(|_| session_id));
    let root_session_id = match requested_root_session_id {
        Some(root_id)
            if root_id == session_id
                || provider_session_exists_cached(store, root_id, &mut caches.session_exists)? =>
        {
            Some(root_id)
        }
        _ => None,
    };
    let (source_metadata, redacted_source_metadata) = sanitize_value(source.metadata.clone());
    let (session_metadata, redacted_session_metadata) = sanitize_value(session.metadata.clone());

    let source_record = CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider,
            machine_id: source.machine_id.clone(),
            process_id: None,
            cwd: session.cwd.clone(),
            raw_source_path: source.raw_source_path.clone(),
            external_session_id: Some(session.provider_session_id.clone()),
        },
        started_at: session.started_at,
        ended_at: session.ended_at,
        sync: provider_sync_metadata(
            source.fidelity,
            json!({
                "provider_session_id": session.provider_session_id,
                "source_format": source.source_format,
                "source_trust": source.trust,
                "raw_retention": source.raw_retention,
                "redaction_boundary": source.redaction_boundary,
                "cursor": source.cursor,
                "fixture_line": line_number,
                "imported_at": imported_at,
                "source_idempotency_key": source.idempotency_key,
                "source_identity_key": source_identity_key,
                "source_metadata": source_metadata,
                "session_metadata": session_metadata,
            }),
        ),
    };
    if caches.processed_sources.insert(source_id) {
        store.upsert_capture_source(&source_record)?;
        if redacted_source_metadata {
            summary.redacted += 1;
        }
    }

    let process_session = caches.processed_sessions.insert(session_id);
    let is_new_session = if process_session {
        !provider_session_exists_cached(store, session_id, &mut caches.session_exists)?
    } else {
        false
    };
    let normalized_session = Session {
        id: session_id,
        history_record_id: options.history_record_id,
        parent_session_id,
        root_session_id,
        capture_source_id: Some(source_id),
        provider,
        external_session_id: Some(session.provider_session_id.clone()),
        external_agent_id: session.external_agent_id.clone(),
        agent_type: session.agent_type,
        role_hint: session.role_hint.clone(),
        is_primary: session.is_primary,
        status: session.status,
        transcript_blob_id: None,
        started_at: session.started_at,
        ended_at: session.ended_at,
        timestamps: timestamps(imported_at),
        sync: provider_sync_metadata(
            session.fidelity,
            json!({
                "provider_session_id": session.provider_session_id,
                "parent_provider_session_id": session.parent_provider_session_id,
                "root_provider_session_id": session.root_provider_session_id,
                "source_format": source.source_format,
                "source_trust": source.trust,
                "fixture_line": line_number,
                "imported_at": imported_at,
                "session_idempotency_key": session.idempotency_key,
                "artifacts": session.artifacts,
                "metadata": session_metadata,
            }),
        ),
    };
    if process_session {
        store.upsert_session(&normalized_session)?;
        caches.session_exists.insert(session_id, true);
        if redacted_session_metadata {
            summary.redacted += 1;
        }
        if is_new_session && caches.imported_sessions.insert(session_id) {
            summary.imported_sessions += 1;
            summary.imported += 1;
        } else {
            summary.skipped_sessions += 1;
            summary.skipped += 1;
        }
    }

    if let Some(parent_id) = parent_session_id {
        let edge_id = provider_edge_uuid(provider, &session.provider_session_id, "parent_child");
        if caches.processed_edges.insert(edge_id) {
            let was_present = store.session_edge_exists(edge_id)?;
            let edge = SessionEdge {
                id: edge_id,
                from_session_id: parent_id,
                to_session_id: session_id,
                edge_type: SessionEdgeType::ParentChild,
                confidence: Confidence::Explicit,
                source_id: Some(source_id),
                timestamps: timestamps(imported_at),
                sync: provider_sync_metadata(
                    session.fidelity,
                    json!({
                        "provider_session_id": session.provider_session_id,
                        "parent_provider_session_id": session.parent_provider_session_id,
                        "source_format": source.source_format,
                        "fixture_line": line_number,
                        "imported_at": imported_at,
                    }),
                ),
            };
            store.upsert_session_edge(&edge)?;
            if !was_present && caches.imported_edges.insert(edge_id) {
                summary.imported_edges += 1;
                summary.imported += 1;
            } else {
                summary.skipped_edges += 1;
                summary.skipped += 1;
            }
        }
    } else if requested_parent_session_id.is_some() {
        let edge_id = provider_edge_uuid(provider, &session.provider_session_id, "parent_child");
        if let Some(parent_session_id) = requested_parent_session_id {
            caches
                .pending_edges
                .entry(edge_id)
                .or_insert_with(|| PendingProviderEdge {
                    provider_session_id: session.provider_session_id.clone(),
                    parent_provider_session_id: session.parent_provider_session_id.clone(),
                    session_id,
                    parent_session_id,
                    root_session_id: requested_root_session_id,
                    source_id,
                    source_format: source.source_format.clone(),
                    imported_at,
                    fidelity: session.fidelity,
                    line_number,
                });
        }
    }

    if let Some(event) = &capture.event {
        let (payload, redacted_payload) = sanitize_value(event.payload.clone());
        let (event_metadata, redacted_metadata) = sanitize_value(event.metadata.clone());
        let event_hash = event
            .provider_event_hash
            .clone()
            .unwrap_or(compute_payload_hash(&payload)?);
        let pi_entry_id = event
            .metadata
            .get("entry_id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty());
        let legacy_provider_event_index = event
            .metadata
            .get("legacy_provider_event_index")
            .and_then(Value::as_u64)
            .filter(|_| !(provider == CaptureProvider::Pi && pi_entry_id.is_some()));
        let provider_event_identity_index = event
            .metadata
            .get("provider_event_identity_index")
            .and_then(Value::as_u64)
            .unwrap_or(event.provider_event_index);
        let event_identity = match pi_existing_event_identity_by_entry_id(
            store,
            provider,
            session_id,
            pi_entry_id,
            caches,
        )? {
            Some(identity) => identity,
            None => provider_event_import_identity(
                store,
                provider,
                &session.provider_session_id,
                source_id,
                provider_event_identity_index,
                event.provider_event_index,
                &event_hash,
                legacy_provider_event_index,
            )?,
        };
        let command_run = provider_command_run_from_event(ProviderCommandRunInput {
            provider,
            provider_session_id: &session.provider_session_id,
            session_id,
            source_id,
            run_source_id: event_identity.run_source_id,
            history_record_id: options.history_record_id,
            event,
            payload: &payload,
            event_hash: &event_hash,
        })?;
        let normalized_event = Event {
            id: event_identity.id,
            seq: event_identity.seq,
            history_record_id: options.history_record_id,
            session_id: Some(session_id),
            run_id: command_run.as_ref().map(|run| run.id),
            event_type: event.event_type,
            role: event.role,
            occurred_at: event.occurred_at,
            capture_source_id: Some(source_id),
            payload: json!({
                "provider": provider.as_str(),
                "provider_session_id": session.provider_session_id,
                "provider_event_index": event.provider_event_index,
                "provider_event_hash": event_hash,
                "cursor": event.cursor,
                "artifacts": event.artifacts,
                "body": payload,
            }),
            payload_blob_id: None,
            dedupe_key: Some(event_identity.dedupe_key.clone()),
            redaction_state: effective_event_redaction_state(
                event.redaction_state,
                redacted_payload || redacted_metadata,
            ),
            sync: provider_sync_metadata(
                event.fidelity,
                json!({
                    "provider_session_id": session.provider_session_id,
                    "provider_event_index": event.provider_event_index,
                    "provider_event_hash": event_hash,
                    "cursor": event.cursor,
                    "source_format": source.source_format,
                    "source_trust": source.trust,
                    "fixture_line": line_number,
                    "imported_at": imported_at,
                    "event_idempotency_key": event.idempotency_key,
                    "metadata": event_metadata,
                }),
            ),
        };
        let was_present = if options.fast_event_inserts {
            if let Some(run) = &command_run {
                store.insert_run_if_absent(run)?;
            }
            !store.insert_event_if_absent(&normalized_event)?
        } else {
            let was_present = provider_event_exists(store, &event_identity.dedupe_key)?;
            if let Some(run) = &command_run {
                store.upsert_run(run)?;
            }
            match store.upsert_event(&normalized_event) {
                Ok(_) => {}
                Err(StoreError::Sql(rusqlite::Error::QueryReturnedNoRows)) => {}
                Err(StoreError::ProviderEventConflict { .. }) => {
                    summary.skipped_events += 1;
                    summary.skipped += 1;
                    if redacted_payload || redacted_metadata {
                        summary.redacted += 1;
                    }
                    if options.persist_cursors {
                        persist_provider_cursor(store, capture)?;
                    }
                    return Ok(summary);
                }
                Err(err) => return Err(CaptureError::Store(err)),
            }
            was_present
        };
        if redacted_payload || redacted_metadata {
            summary.redacted += 1;
        }
        if was_present {
            summary.skipped_events += 1;
            summary.skipped += 1;
        } else {
            summary.imported_events += 1;
            summary.imported += 1;
        }
    }

    if options.persist_cursors {
        persist_provider_cursor(store, capture)?;
    }

    Ok(summary)
}
