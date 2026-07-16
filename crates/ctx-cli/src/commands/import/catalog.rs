use super::*;
use ctx_history_capture::{
    observe_ordinary_file, observe_sqlite_source_generation, OrdinaryFileObservation,
    SqliteObservedFile,
};
use sha2::{Digest, Sha256};

pub(crate) fn system_time_ms(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn catalog_batch_outcome(
    summary: &ProviderImportSummary,
    completed_units: usize,
    completed_bytes: u64,
    deferred_units: usize,
) -> super::native::ProviderImportBatchOutcome {
    super::native::ProviderImportBatchOutcome {
        summary: summary.clone(),
        completed_units,
        completed_bytes,
        deferred_units,
        durable_progress: false,
        post_import_inventory_generation: None,
        post_import_preinventory: None,
    }
}

fn catalog_batch_error_with_progress(
    summary: &ProviderImportSummary,
    completed_units: usize,
    completed_bytes: u64,
    deferred_units: usize,
    durable_progress: bool,
    error: anyhow::Error,
) -> anyhow::Error {
    super::native::provider_import_batch_error(
        super::native::ProviderImportBatchOutcome {
            summary: summary.clone(),
            completed_units,
            completed_bytes,
            deferred_units,
            durable_progress,
            post_import_inventory_generation: None,
            post_import_preinventory: None,
        },
        error,
    )
}

fn catalog_batch_error(
    summary: &ProviderImportSummary,
    completed_units: usize,
    completed_bytes: u64,
    deferred_units: usize,
    error: anyhow::Error,
) -> anyhow::Error {
    super::native::provider_import_batch_error(
        catalog_batch_outcome(summary, completed_units, completed_bytes, deferred_units),
        error,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn import_incremental_codex_session_tree(
    store: &mut Store,
    source: &SourceInfo,
    record: &HistoryRecord,
    progress: Option<CodexSessionImportProgressCallback>,
    preinventory_catalog: Option<&CatalogSummary>,
    preinventory_generation: Option<u64>,
    force_selection: bool,
    selection: Option<&SelectedImportWork>,
) -> Result<super::native::ProviderImportBatchOutcome> {
    let record_id = record.id;
    let source_root = codex_catalog_root_identity(&source.path)?.to_owned();
    let inventory_generation = match preinventory_generation {
        Some(generation) => generation,
        None => {
            store.allocate_catalog_inventory_generation(CaptureProvider::Codex, &source_root)?
        }
    };
    let mut summary = ProviderImportSummary::default();
    let mut completed_units = 0;
    let mut completed_bytes = 0_u64;
    let mut deferred_units = 0;
    let mut durable_progress = false;
    if let Some(catalog) = preinventory_catalog {
        summary.failed += catalog.failed_sessions;
        summary.failures.extend(catalog.failures.clone());
    } else {
        let catalog = catalog_codex_session_tree(
            &source.path,
            store,
            CodexSessionCatalogOptions {
                source_root: Some(source.path.clone()),
                observation_generation: Some(inventory_generation),
                ..CodexSessionCatalogOptions::default()
            },
        )
        .with_context(|| format!("inventory Codex sessions from {}", source.path.display()))?;
        summary.failed += catalog.failed_sessions;
        summary.failures.extend(catalog.failures);
    }

    if let Some(SelectedImportWork::Catalog(work)) = selection {
        for unit in work {
            let outcome = match super::native::import_append_capable_catalog_work(
                store,
                source,
                unit,
                inventory_generation,
                record,
            ) {
                Ok(outcome) => outcome,
                Err(error) => {
                    if super::native::publication_recovery_required(&error) {
                        return Err(catalog_batch_error_with_progress(
                            &summary,
                            completed_units,
                            completed_bytes,
                            deferred_units,
                            durable_progress,
                            error,
                        ));
                    }
                    let rejected_summary = rejected_source_summary(&error);
                    let status = if rejected_summary.is_some() {
                        CatalogIndexedStatus::Rejected
                    } else {
                        catalog_import_error_status(&error)
                    };
                    if let Err(persist_error) = store.record_observed_catalog_source_import_result(
                        unit.session.provider,
                        CatalogSourceIndexUpdate {
                            source_root: &unit.session.source_root,
                            source_path: &unit.session.source_path,
                            file_size_bytes: unit.session.file_size_bytes,
                            file_modified_at_ms: unit.session.file_modified_at_ms,
                            import_revision: unit.session.import_revision,
                            inventory_generation,
                            file_sha256: None,
                            event_count: None,
                            indexed_at_ms: utc_now().timestamp_millis(),
                        },
                        &unit.session.metadata,
                        status,
                        Some(&error_summary(&error)),
                    ) {
                        return Err(catalog_batch_error_with_progress(
                            &summary,
                            completed_units,
                            completed_bytes,
                            deferred_units,
                            durable_progress,
                            persist_error.into(),
                        ));
                    }
                    if let Some(mut rejected_summary) = rejected_summary {
                        completed_units += 1;
                        completed_bytes = completed_bytes.saturating_add(unit.estimated_bytes);
                        for failure in &mut rejected_summary.failures {
                            failure.error =
                                format!("{}: {}", unit.session.source_path, failure.error);
                        }
                        summary.merge_from(rejected_summary);
                        continue;
                    }
                    return Err(catalog_batch_error_with_progress(
                        &summary,
                        completed_units,
                        completed_bytes,
                        deferred_units,
                        durable_progress,
                        error,
                    ));
                }
            };
            match outcome {
                super::native::AppendImportOutcome::Imported(unit_summary) => {
                    summary.merge_from(unit_summary);
                    completed_units += 1;
                    completed_bytes = completed_bytes.saturating_add(unit.estimated_bytes);
                }
                super::native::AppendImportOutcome::Deferred {
                    durable_progress: unit_progress,
                } => {
                    deferred_units += 1;
                    durable_progress |= unit_progress;
                    break;
                }
            }
        }
        return Ok(super::native::ProviderImportBatchOutcome {
            summary,
            completed_units,
            completed_bytes,
            deferred_units,
            durable_progress,
            post_import_inventory_generation: None,
            post_import_preinventory: None,
        });
    }

    let selected_sessions = match selection {
        Some(SelectedImportWork::Catalog(work)) => {
            work.iter().map(|work| work.session.clone()).collect()
        }
        Some(SelectedImportWork::SourceFiles(_)) => {
            return Err(anyhow::Error::new(CaptureError::SystemInvariant(
                "source-file work selected for a catalog source",
            )))
        }
        None if force_selection => {
            store.list_active_catalog_sessions_for_source(CaptureProvider::Codex, &source_root)?
        }
        None => store.list_pending_catalog_sessions(CaptureProvider::Codex, &source_root)?,
    };
    if selected_sessions.is_empty() {
        if !store.catalog_inventory_generation_is_complete(
            CaptureProvider::Codex,
            &source_root,
            inventory_generation,
        )? {
            return Err(anyhow::Error::new(CaptureError::InventorySuperseded));
        }
        return Ok(super::native::ProviderImportBatchOutcome::completed(
            summary, 0,
        ));
    }

    let mut full_import_sessions = Vec::new();
    for session in &selected_sessions {
        if force_selection {
            full_import_sessions.push(session.clone());
            continue;
        }
        let state = match store.catalog_source_index_state(
            CaptureProvider::Codex,
            &source_root,
            &session.source_path,
        ) {
            Ok(state) => state,
            Err(error) => {
                return Err(catalog_batch_error(
                    &summary,
                    completed_units,
                    completed_bytes,
                    deferred_units,
                    error.into(),
                ))
            }
        };
        let tail_start = state
            .as_ref()
            .and_then(|state| state.last_imported_file_size_bytes)
            .filter(|indexed_size| *indexed_size > 0 && *indexed_size < session.file_size_bytes);
        if let Some(start_offset) = tail_start {
            let checkpoint_hash = state
                .as_ref()
                .and_then(|state| state.last_imported_file_sha256.as_deref());
            let checkpoint_matches = match catalog_import_checkpoint_matches(
                Path::new(&session.source_path),
                start_offset,
                checkpoint_hash,
            ) {
                Ok(matches) => matches,
                Err(err) => {
                    let error = error_summary(&err);
                    if let Err(persist_error) = mark_catalog_sessions_error(
                        store,
                        std::slice::from_ref(session),
                        &error,
                        catalog_import_error_status(&err),
                        inventory_generation,
                    ) {
                        return Err(catalog_batch_error(
                            &summary,
                            completed_units,
                            completed_bytes,
                            deferred_units,
                            persist_error,
                        ));
                    }
                    return Err(catalog_batch_error(
                        &summary,
                        completed_units,
                        completed_bytes,
                        deferred_units,
                        err,
                    ));
                }
            };
            if !checkpoint_matches {
                full_import_sessions.push(session.clone());
                continue;
            }
            let tail_summary = match import_codex_session_jsonl_tail(
                PathBuf::from(&session.source_path),
                start_offset,
                store,
                CodexSessionImportOptions {
                    source_path: Some(source.path.clone()),
                    history_record_id: Some(record_id),
                    progress: progress.clone(),
                    ..CodexSessionImportOptions::default()
                },
            )
            .map_err(anyhow::Error::from)
            {
                Ok(summary) => summary,
                Err(err) => {
                    if let Err(persist_error) = mark_catalog_sessions_error(
                        store,
                        std::slice::from_ref(session),
                        &err.to_string(),
                        catalog_import_error_status(&err),
                        inventory_generation,
                    ) {
                        return Err(catalog_batch_error(
                            &summary,
                            completed_units,
                            completed_bytes,
                            deferred_units,
                            persist_error,
                        ));
                    }
                    return Err(catalog_batch_error(
                        &summary,
                        completed_units,
                        completed_bytes,
                        deferred_units,
                        err,
                    ));
                }
            };
            let tail_event_count = tail_summary
                .imported_events
                .saturating_add(tail_summary.skipped_events)
                as u64;
            let event_count = state
                .and_then(|state| state.last_imported_event_count)
                .map(|event_count| event_count.saturating_add(tail_event_count));
            let status = if tail_summary.failed == 0 {
                CatalogIndexedStatus::Indexed
            } else {
                CatalogIndexedStatus::CompletedWithRejections
            };
            let error =
                (tail_summary.failed > 0).then(|| catalog_session_import_failure(&tail_summary));
            if let Err(persist_error) = mark_catalog_session_result(
                store,
                session,
                event_count,
                utc_now().timestamp_millis(),
                status,
                error.as_deref(),
                inventory_generation,
            ) {
                let mut partial_summary = summary.clone();
                partial_summary.merge_from(tail_summary);
                return Err(catalog_batch_error(
                    &partial_summary,
                    completed_units,
                    completed_bytes,
                    deferred_units,
                    persist_error,
                ));
            }
            completed_units += 1;
            completed_bytes = completed_bytes.saturating_add(session.file_size_bytes);
            summary.merge_from(tail_summary);
        } else {
            full_import_sessions.push(session.clone());
        }
    }

    if !full_import_sessions.is_empty() {
        for session in &full_import_sessions {
            let paths = vec![PathBuf::from(&session.source_path)];
            let file_summary = match import_codex_session_paths(
                paths,
                store,
                CodexSessionImportOptions {
                    source_path: Some(source.path.clone()),
                    history_record_id: Some(record_id),
                    progress: progress.clone(),
                    ..CodexSessionImportOptions::default()
                },
            )
            .map_err(anyhow::Error::from)
            {
                Ok(file_summary) => file_summary,
                Err(err) => {
                    let failure_scope = import_error_scope(&err);
                    let error = error_summary(&err);
                    if let Err(persist_error) = mark_catalog_sessions_error(
                        store,
                        std::slice::from_ref(session),
                        &error,
                        catalog_import_error_status(&err),
                        inventory_generation,
                    ) {
                        return Err(catalog_batch_error(
                            &summary,
                            completed_units,
                            completed_bytes,
                            deferred_units,
                            persist_error,
                        ));
                    }
                    if failure_scope == ImportFailureScope::System {
                        return Err(catalog_batch_error(
                            &summary,
                            completed_units,
                            completed_bytes,
                            deferred_units,
                            err,
                        ));
                    }
                    completed_units += 1;
                    completed_bytes = completed_bytes.saturating_add(session.file_size_bytes);
                    summary.failed += 1;
                    summary
                        .failures
                        .push(ProviderImportFailure { line: 0, error });
                    continue;
                }
            };
            if let Err(persist_error) = mark_catalog_sessions_result(
                store,
                std::slice::from_ref(session),
                &file_summary,
                inventory_generation,
            ) {
                let mut partial_summary = summary.clone();
                partial_summary.merge_from(file_summary);
                return Err(catalog_batch_error(
                    &partial_summary,
                    completed_units,
                    completed_bytes,
                    deferred_units,
                    persist_error,
                ));
            }
            completed_units += 1;
            completed_bytes = completed_bytes.saturating_add(session.file_size_bytes);
            summary.merge_from(file_summary);
        }
    }
    Ok(catalog_batch_outcome(
        &summary,
        completed_units,
        completed_bytes,
        deferred_units,
    ))
}

pub(crate) fn codex_catalog_root_identity(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow!("Codex catalog source root is not valid UTF-8"))
}

fn catalog_session_import_failure(summary: &ProviderImportSummary) -> String {
    summary
        .failures
        .first()
        .map(|failure| {
            if failure.line == 0 {
                failure.error.clone()
            } else {
                format!("line {}: {}", failure.line, failure.error)
            }
        })
        .unwrap_or_else(|| "session import failed".to_owned())
}

pub(crate) fn mark_catalog_sessions_result(
    store: &Store,
    sessions: &[CatalogSession],
    summary: &ProviderImportSummary,
    inventory_generation: u64,
) -> Result<()> {
    let indexed_at_ms = utc_now().timestamp_millis();
    let event_count = if sessions.len() == 1 {
        Some(
            summary
                .imported_events
                .saturating_add(summary.skipped_events) as u64,
        )
    } else {
        None
    };
    let status = provider_summary_import_status(summary);
    let error = (summary.failed > 0).then(|| catalog_session_import_failure(summary));
    for session in sessions {
        mark_catalog_session_result(
            store,
            session,
            event_count,
            indexed_at_ms,
            status,
            error.as_deref(),
            inventory_generation,
        )?;
    }
    Ok(())
}

pub(crate) fn mark_catalog_session_result(
    store: &Store,
    session: &CatalogSession,
    event_count: Option<u64>,
    indexed_at_ms: i64,
    status: CatalogIndexedStatus,
    error: Option<&str>,
    inventory_generation: u64,
) -> Result<()> {
    let file_sha256 = if status == CatalogIndexedStatus::Indexed {
        let hash = sha256_file_prefix_hex(Path::new(&session.source_path), session.file_size_bytes)
            .with_context(|| format!("hash checkpoint prefix for {}", session.source_path));
        match hash {
            Ok(hash) => Some(hash),
            Err(err) => {
                let durable_error = error_summary(&err);
                mark_catalog_sessions_error(
                    store,
                    std::slice::from_ref(session),
                    &durable_error,
                    catalog_import_error_status(&err),
                    inventory_generation,
                )?;
                return Err(err);
            }
        }
    } else {
        None
    };
    let changed = store.record_observed_catalog_source_import_result(
        session.provider,
        CatalogSourceIndexUpdate {
            source_root: &session.source_root,
            source_path: &session.source_path,
            file_size_bytes: session.file_size_bytes,
            file_modified_at_ms: session.file_modified_at_ms,
            import_revision: session.import_revision,
            inventory_generation,
            file_sha256: file_sha256.as_deref(),
            event_count,
            indexed_at_ms,
        },
        &session.metadata,
        status,
        error,
    )?;
    if changed != 1 {
        return Err(anyhow::Error::new(CaptureError::InventorySuperseded));
    }
    Ok(())
}

pub(crate) fn catalog_import_checkpoint_matches(
    path: &Path,
    byte_count: u64,
    expected_sha256: Option<&str>,
) -> Result<bool> {
    let Some(expected_sha256) = expected_sha256 else {
        return Ok(true);
    };
    let actual_sha256 = sha256_file_prefix_hex(path, byte_count)?;
    Ok(actual_sha256 == expected_sha256)
}

pub(crate) fn sha256_file_prefix_hex(path: &Path, byte_count: u64) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut remaining = byte_count;
    let mut buffer = [0_u8; 8192];
    while remaining > 0 {
        let to_read = buffer.len().min(remaining as usize);
        let read = file.read(&mut buffer[..to_read])?;
        if read == 0 {
            return Err(anyhow!(
                "file ended before checkpoint byte offset {byte_count}: {}",
                path.display()
            ));
        }
        hasher.update(&buffer[..read]);
        remaining -= read as u64;
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub(crate) fn mark_catalog_sessions_error(
    store: &Store,
    sessions: &[CatalogSession],
    error: &str,
    status: CatalogIndexedStatus,
    inventory_generation: u64,
) -> Result<()> {
    let indexed_at_ms = utc_now().timestamp_millis();
    for session in sessions {
        let changed = store.record_observed_catalog_source_import_result(
            session.provider,
            CatalogSourceIndexUpdate {
                source_root: &session.source_root,
                source_path: &session.source_path,
                file_size_bytes: session.file_size_bytes,
                file_modified_at_ms: session.file_modified_at_ms,
                import_revision: session.import_revision,
                inventory_generation,
                file_sha256: None,
                event_count: None,
                indexed_at_ms,
            },
            &session.metadata,
            status,
            Some(error),
        )?;
        if changed != 1 {
            return Err(anyhow::Error::new(CaptureError::InventorySuperseded));
        }
    }
    Ok(())
}

fn catalog_import_error_status(error: &anyhow::Error) -> CatalogIndexedStatus {
    match import_error_retryability(error) {
        ImportRetryability::Retryable => CatalogIndexedStatus::Failed,
        ImportRetryability::Terminal => CatalogIndexedStatus::Rejected,
    }
}

#[cfg(test)]
pub(crate) fn source_uses_incremental_event_search(source: &SourceInfo) -> bool {
    // Every importable provider persists events through Store APIs that update
    // the event-search projection transactionally. Unsupported sources have no
    // importer and therefore cannot make that guarantee.
    source.import_support.is_importable()
}

pub(crate) fn source_stats(path: &Path) -> Result<SourceStats> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("stat import source {}", path.display()))?;
    let mut stats = SourceStats::default();
    let mut change_entries = Vec::new();
    if metadata.file_type().is_file() {
        if is_sqlite_main_path(path) {
            let generation = observe_sqlite_source_generation(path)
                .with_context(|| format!("observe SQLite import source {}", path.display()))?;
            stats.files = 1;
            stats.bytes = generation.main().len();
            change_entries.extend(generation.files().into_iter().map(|file| {
                SourceChangeEntry::from_sqlite_observed(path.parent().unwrap_or(path), file)
            }));
            stats.change_token = Some(source_change_token(change_entries));
            return Ok(stats);
        }
        let observation = observe_ordinary_file(path)
            .with_context(|| format!("observe import source {}", path.display()))?;
        add_source_observation(
            &mut stats,
            &mut change_entries,
            path.parent().unwrap_or(path),
            path,
            &observation,
            true,
        );
        stats.change_token = Some(source_change_token(change_entries));
        return Ok(stats);
    }
    if !metadata.file_type().is_dir() {
        return Ok(SourceStats::default());
    }

    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)
            .with_context(|| format!("read import source directory {}", dir.display()))?
        {
            let entry = entry
                .with_context(|| format!("read import source entry under {}", dir.display()))?;
            let entry_path = entry.path();
            let file_type = entry
                .file_type()
                .with_context(|| format!("stat import source entry {}", entry_path.display()))?;
            if file_type.is_dir() {
                stack.push(entry_path);
            } else if file_type.is_file() {
                let metadata = match entry.metadata() {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!("stat import source file {}", entry_path.display())
                        })
                    }
                };
                if is_sqlite_sidecar_path(&entry_path) {
                    stats.files += 1;
                    stats.bytes = stats.bytes.saturating_add(metadata.len());
                    continue;
                }
                if is_sqlite_main_path(&entry_path) {
                    stats.files += 1;
                    stats.bytes = stats.bytes.saturating_add(metadata.len());
                    match observe_sqlite_source_generation(&entry_path) {
                        Ok(generation) => change_entries.extend(
                            generation
                                .files()
                                .into_iter()
                                .map(|file| SourceChangeEntry::from_sqlite_observed(path, file)),
                        ),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(error) => {
                            return Err(error).with_context(|| {
                                format!("observe SQLite import source {}", entry_path.display())
                            })
                        }
                    }
                    continue;
                }
                let observation = observe_ordinary_file(&entry_path).with_context(|| {
                    format!("observe import source file {}", entry_path.display())
                })?;
                add_source_observation(
                    &mut stats,
                    &mut change_entries,
                    path,
                    &entry_path,
                    &observation,
                    true,
                );
            }
        }
    }
    stats.change_token = Some(source_change_token(change_entries));
    Ok(stats)
}

pub(crate) struct SourceChangeEntry {
    path: PathBuf,
    len: u64,
    modified_secs: u64,
    modified_nanos: u32,
    sentinel: Vec<u8>,
}

impl SourceChangeEntry {
    pub(crate) fn from_metadata(base: &Path, path: &Path, metadata: &fs::Metadata) -> Self {
        let modified = metadata
            .modified()
            .unwrap_or(UNIX_EPOCH)
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self {
            path: path.strip_prefix(base).unwrap_or(path).to_path_buf(),
            len: metadata.len(),
            modified_secs: modified.as_secs(),
            modified_nanos: modified.subsec_nanos(),
            sentinel: Vec::new(),
        }
    }

    pub(crate) fn from_observation(
        base: &Path,
        path: &Path,
        observation: &OrdinaryFileObservation,
    ) -> Self {
        let modified = observation
            .modified_at()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self {
            path: path.strip_prefix(base).unwrap_or(path).to_path_buf(),
            len: observation.len(),
            modified_secs: modified.as_secs(),
            modified_nanos: modified.subsec_nanos(),
            sentinel: observation.token().to_vec(),
        }
    }

    pub(crate) fn from_sqlite_observed(base: &Path, file: &SqliteObservedFile) -> Self {
        Self {
            path: file
                .path()
                .strip_prefix(base)
                .unwrap_or(file.path())
                .to_path_buf(),
            len: file.len(),
            modified_secs: file.modified_secs(),
            modified_nanos: file.modified_nanos(),
            sentinel: file.sentinel().to_vec(),
        }
    }
}

fn is_sqlite_main_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("db" | "sqlite" | "sqlite3" | "vscdb")
    )
}

fn is_sqlite_sidecar_path(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        [
            b"-wal".as_slice(),
            b"-journal".as_slice(),
            b"-shm".as_slice(),
        ]
        .iter()
        .any(|suffix| name.as_encoded_bytes().ends_with(suffix))
    })
}

fn add_source_observation(
    stats: &mut SourceStats,
    change_entries: &mut Vec<SourceChangeEntry>,
    base: &Path,
    path: &Path,
    observation: &OrdinaryFileObservation,
    include_in_token: bool,
) {
    stats.files += 1;
    stats.bytes = stats.bytes.saturating_add(observation.len());
    if !include_in_token {
        return;
    }
    change_entries.push(SourceChangeEntry::from_observation(base, path, observation));
}

pub(crate) fn source_change_token(mut entries: Vec<SourceChangeEntry>) -> [u8; 32] {
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let mut hasher = Sha256::new();
    for entry in entries {
        let path = entry.path.as_os_str().as_encoded_bytes();
        hasher.update((path.len() as u64).to_le_bytes());
        hasher.update(path);
        hasher.update(entry.len.to_le_bytes());
        hasher.update(entry.modified_secs.to_le_bytes());
        hasher.update(entry.modified_nanos.to_le_bytes());
        if !entry.sentinel.is_empty() {
            hasher.update((entry.sentinel.len() as u64).to_le_bytes());
            hasher.update(entry.sentinel);
        }
    }
    hasher.finalize().into()
}

pub(crate) fn import_record_for_source(source: &SourceInfo) -> HistoryRecord {
    let key = format!(
        "agent-history:{}:{}",
        source.provider.as_str(),
        source.path.display()
    );
    let mut record = HistoryRecord::new(
        format!("{} agent history", source.provider.as_str()),
        format!(
            "Indexed local agent history from {} ({})",
            source.path.display(),
            source.source_format
        ),
        vec!["agent-history".into(), source.provider.as_str().into()],
        "agent_history",
        source.path.parent().map(|path| path.display().to_string()),
    );
    record.id = stable_capture_uuid(&key, "record");
    record
}

pub(crate) fn import_record_for_custom_history(
    path: &Path,
    format: ImportFormatArg,
) -> HistoryRecord {
    let key = format!("custom-history:{}:{}", format.as_str(), path.display());
    let mut record = HistoryRecord::new(
        "custom agent history".to_owned(),
        format!(
            "Indexed custom agent history from {} ({})",
            path.display(),
            format.as_str()
        ),
        vec![
            "agent-history".into(),
            "custom".into(),
            format.as_str().into(),
        ],
        "agent_history",
        path.parent().map(|path| path.display().to_string()),
    );
    record.id = stable_capture_uuid(&key, "record");
    record
}

pub(crate) fn import_record_for_history_source_plugin(
    source: &HistorySourcePluginSource,
) -> HistoryRecord {
    let key = format!(
        "history-source-plugin:{}:{}:{}:{}:{}",
        source.plugin_name, source.id, source.provider_key, source.source_id, source.source_format
    );
    let mut record = HistoryRecord::new(
        format!("history source plugin {}", source.label()),
        format!(
            "Indexed custom agent history from history source plugin {} ({})",
            source.label(),
            source.source_format
        ),
        vec![
            "agent-history".into(),
            "custom".into(),
            "history-source-plugin".into(),
            source.provider_key.clone(),
            source.source_format.clone(),
        ],
        "agent_history",
        source
            .manifest_path
            .parent()
            .map(|path| path.display().to_string()),
    );
    record.id = stable_capture_uuid(&key, "record");
    record
}

include!("catalog_tests.rs");
