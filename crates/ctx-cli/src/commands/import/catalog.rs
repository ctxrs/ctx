use super::*;
use crate::commands::import::manifest::collect_source_import_paths;
use ctx_history_capture::{observe_sqlite_source_generation, SqliteObservedFile};
use sha2::{Digest, Sha256};

pub(crate) fn system_time_ms(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn import_incremental_codex_session_tree(
    store: &mut Store,
    source: &SourceInfo,
    record_id: Uuid,
    progress: Option<CodexSessionImportProgressCallback>,
    preinventory_catalog: Option<&CatalogSummary>,
    preinventory_generation: Option<u64>,
    force_selection: bool,
) -> Result<ProviderImportSummary> {
    let source_root = codex_catalog_root_identity(&source.path)?.to_owned();
    let inventory_generation = match preinventory_generation {
        Some(generation) => generation,
        None => {
            store.allocate_catalog_inventory_generation(CaptureProvider::Codex, &source_root)?
        }
    };
    let mut summary = ProviderImportSummary::default();
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

    let selected_sessions = if force_selection {
        store.list_active_catalog_sessions_for_source(CaptureProvider::Codex, &source_root)?
    } else {
        store.list_pending_catalog_sessions(CaptureProvider::Codex, &source_root)?
    };
    if selected_sessions.is_empty() {
        if !store.catalog_inventory_generation_is_complete(
            CaptureProvider::Codex,
            &source_root,
            inventory_generation,
        )? {
            return Err(anyhow::Error::new(CaptureError::InventorySuperseded));
        }
        return Ok(summary);
    }

    let mut full_import_sessions = Vec::new();
    for session in &selected_sessions {
        if force_selection {
            full_import_sessions.push(session.clone());
            continue;
        }
        let state = store.catalog_source_index_state(
            CaptureProvider::Codex,
            &source_root,
            &session.source_path,
        )?;
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
                    mark_catalog_sessions_error(
                        store,
                        std::slice::from_ref(session),
                        &error,
                        catalog_import_error_status(&err),
                        inventory_generation,
                    )?;
                    return Err(err);
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
                    mark_catalog_sessions_error(
                        store,
                        std::slice::from_ref(session),
                        &err.to_string(),
                        catalog_import_error_status(&err),
                        inventory_generation,
                    )?;
                    return Err(err);
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
            mark_catalog_session_result(
                store,
                session,
                event_count,
                utc_now().timestamp_millis(),
                status,
                error.as_deref(),
                inventory_generation,
            )?;
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
                    mark_catalog_sessions_error(
                        store,
                        std::slice::from_ref(session),
                        &error,
                        catalog_import_error_status(&err),
                        inventory_generation,
                    )?;
                    if failure_scope == ImportFailureScope::System {
                        return Err(err);
                    }
                    summary.failed += 1;
                    summary
                        .failures
                        .push(ProviderImportFailure { line: 0, error });
                    continue;
                }
            };
            mark_catalog_sessions_result(
                store,
                std::slice::from_ref(session),
                &file_summary,
                inventory_generation,
            )?;
            summary.merge_from(file_summary);
        }
    }
    Ok(summary)
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
    let changed = store.record_catalog_source_import_result(
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
        let changed = store.record_catalog_source_import_result(
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
        add_source_stat(
            &mut stats,
            &mut change_entries,
            path.parent().unwrap_or(path),
            path,
            &metadata,
            true,
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
                let include_in_token = !is_sqlite_sidecar_path(&entry_path);
                add_source_stat(
                    &mut stats,
                    &mut change_entries,
                    path,
                    &entry_path,
                    &metadata,
                    true,
                    include_in_token,
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

fn add_source_stat(
    stats: &mut SourceStats,
    change_entries: &mut Vec<SourceChangeEntry>,
    base: &Path,
    path: &Path,
    metadata: &fs::Metadata,
    include_in_totals: bool,
    include_in_token: bool,
) {
    if include_in_totals {
        stats.files += 1;
        stats.bytes = stats.bytes.saturating_add(metadata.len());
    }
    if !include_in_token {
        return;
    }
    change_entries.push(SourceChangeEntry::from_metadata(base, path, metadata));
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

pub(crate) fn source_import_stats(source: &SourceInfo) -> Result<SourceStats> {
    let mut stats = SourceStats::default();
    for path in collect_source_import_paths(source)? {
        let metadata = fs::metadata(&path)
            .with_context(|| format!("stat import source file {}", path.display()))?;
        stats.files += 1;
        stats.bytes = stats.bytes.saturating_add(metadata.len());
    }
    Ok(stats)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_sources::explicit_path_source;
    use ctx_history_capture::provider_source_specs;
    use ctx_history_core::AgentType;

    #[test]
    fn every_importable_provider_uses_incremental_event_search() {
        for spec in provider_source_specs() {
            let source = explicit_path_source(
                spec.provider,
                PathBuf::from(format!("{}-history", spec.provider.as_str())),
            );

            assert_eq!(source.import_support, spec.import_support);
            assert!(
                source_uses_incremental_event_search(&source),
                "{} import must maintain event search incrementally",
                spec.provider
            );
        }
    }

    #[test]
    fn unsupported_source_does_not_claim_incremental_event_search() {
        let source = explicit_path_source(CaptureProvider::Shell, PathBuf::from("shell-history"));

        assert!(!source.import_support.is_importable());
        assert!(!source_uses_incremental_event_search(&source));
    }

    #[test]
    fn codex_result_rejects_a_generation_superseded_after_normalization() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("work.sqlite");
        let source_path = temp.path().join("session.jsonl");
        fs::write(&source_path, b"{}\n").unwrap();
        let source_root = temp.path().join("sessions").display().to_string();
        let source_path = source_path.display().to_string();
        let session = CatalogSession {
            provider: CaptureProvider::Codex,
            source_format: "codex_session_jsonl".to_owned(),
            source_root: source_root.clone(),
            source_path,
            external_session_id: Some("superseded-result".to_owned()),
            parent_external_session_id: None,
            agent_type: AgentType::Primary,
            role_hint: None,
            external_agent_id: None,
            cwd: None,
            session_started_at_ms: Some(1),
            file_size_bytes: 3,
            file_modified_at_ms: 1,
            import_revision: 1,
            cataloged_at_ms: 1,
            metadata: serde_json::json!({}),
        };
        let store = Store::open(db_path).unwrap();
        let superseded = store
            .allocate_catalog_inventory_generation(CaptureProvider::Codex, &source_root)
            .unwrap();
        store
            .upsert_catalog_sessions(superseded, std::slice::from_ref(&session))
            .unwrap();
        store
            .complete_catalog_inventory_generation(CaptureProvider::Codex, &source_root, superseded)
            .unwrap();
        store
            .allocate_catalog_inventory_generation(CaptureProvider::Codex, &source_root)
            .unwrap();

        let error = mark_catalog_session_result(
            &store,
            &session,
            Some(1),
            2,
            CatalogIndexedStatus::Indexed,
            None,
            superseded,
        )
        .unwrap_err();

        assert!(error.chain().any(|cause| matches!(
            cause.downcast_ref::<CaptureError>(),
            Some(CaptureError::InventorySuperseded)
        )));
    }

    #[test]
    fn sqlite_source_stats_observe_durable_sidecars_but_ignore_shm() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("state.db");
        fs::write(&db, b"main").unwrap();
        let initial = source_stats(&db).unwrap().change_token.unwrap();

        fs::write(sqlite_sidecar(&db, "-shm"), b"volatile coordination state").unwrap();
        assert_eq!(source_stats(&db).unwrap().change_token.unwrap(), initial);

        fs::write(sqlite_sidecar(&db, "-wal"), b"committed wal frame").unwrap();
        assert_ne!(source_stats(&db).unwrap().change_token.unwrap(), initial);

        let root = temp.path().join("project");
        fs::create_dir(&root).unwrap();
        let nested_db = root.join("session.db");
        fs::write(&nested_db, b"main").unwrap();
        let root_initial = source_stats(&root).unwrap().change_token.unwrap();
        fs::write(sqlite_sidecar(&nested_db, "-shm"), b"volatile").unwrap();
        assert_eq!(
            source_stats(&root).unwrap().change_token.unwrap(),
            root_initial
        );
        fs::write(sqlite_sidecar(&nested_db, "-journal"), b"committed journal").unwrap();
        assert_ne!(
            source_stats(&root).unwrap().change_token.unwrap(),
            root_initial
        );
    }

    #[test]
    fn sqlite_source_stats_detect_same_stat_wal_generation_and_disappearance() {
        use std::fs::FileTimes;

        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("state.db");
        let first_fixture = real_wal_generation(temp.path(), "first", "omega");
        let second_fixture = real_wal_generation(temp.path(), "second", "sigma");
        assert_eq!(first_fixture.0, second_fixture.0);
        assert_eq!(first_fixture.1.len(), second_fixture.1.len());
        fs::write(&db, first_fixture.0).unwrap();
        let wal = sqlite_sidecar(&db, "-wal");
        fs::write(&wal, first_fixture.1).unwrap();
        let original_metadata = fs::metadata(&wal).unwrap();
        let original_modified = original_metadata.modified().unwrap();
        let first = source_stats(&db).unwrap().change_token.unwrap();

        fs::write(&wal, second_fixture.1).unwrap();
        fs::File::options()
            .write(true)
            .open(&wal)
            .unwrap()
            .set_times(FileTimes::new().set_modified(original_modified))
            .unwrap();
        let replacement_metadata = fs::metadata(&wal).unwrap();
        assert_eq!(replacement_metadata.len(), original_metadata.len());
        assert_eq!(replacement_metadata.modified().unwrap(), original_modified);
        let replaced = source_stats(&db).unwrap().change_token.unwrap();
        assert_ne!(replaced, first);

        fs::remove_file(&wal).unwrap();
        let disappeared = source_stats(&db).unwrap().change_token.unwrap();
        assert_ne!(disappeared, replaced);
    }

    #[test]
    fn ordinary_source_change_tokens_keep_the_stat_only_encoding() {
        let path = PathBuf::from("session.jsonl");
        let entry = SourceChangeEntry {
            path: path.clone(),
            len: 42,
            modified_secs: 123,
            modified_nanos: 456,
            sentinel: Vec::new(),
        };
        let mut expected = Sha256::new();
        let path = path.as_os_str().as_encoded_bytes();
        expected.update((path.len() as u64).to_le_bytes());
        expected.update(path);
        expected.update(42_u64.to_le_bytes());
        expected.update(123_u64.to_le_bytes());
        expected.update(456_u32.to_le_bytes());
        let expected: [u8; 32] = expected.finalize().into();

        assert_eq!(source_change_token(vec![entry]), expected);
    }

    #[cfg(unix)]
    #[test]
    fn source_change_tokens_distinguish_lossy_non_utf8_path_labels() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let path_a = PathBuf::from(OsString::from_vec(b"session-\x80.jsonl".to_vec()));
        let path_b = PathBuf::from(OsString::from_vec(b"session-\x81.jsonl".to_vec()));
        assert_eq!(path_a.display().to_string(), path_b.display().to_string());

        let entry = |path| SourceChangeEntry {
            path,
            len: 42,
            modified_secs: 123,
            modified_nanos: 456,
            sentinel: Vec::new(),
        };
        assert_ne!(
            source_change_token(vec![entry(path_a)]),
            source_change_token(vec![entry(path_b)])
        );
    }

    fn sqlite_sidecar(path: &Path, suffix: &str) -> PathBuf {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        PathBuf::from(sidecar)
    }

    fn real_wal_generation(root: &Path, name: &str, value: &str) -> (Vec<u8>, Vec<u8>) {
        let path = root.join(format!("{name}.db"));
        let writer = rusqlite::Connection::open(&path).unwrap();
        writer
            .execute_batch(
                "PRAGMA page_size = 512;
                 VACUUM;
                 CREATE TABLE entries (id INTEGER PRIMARY KEY, value TEXT);
                 INSERT INTO entries VALUES (1, 'alpha');
                 PRAGMA journal_mode = WAL;
                 PRAGMA wal_autocheckpoint = 0;
                 PRAGMA wal_checkpoint(TRUNCATE);",
            )
            .unwrap();
        writer
            .execute("UPDATE entries SET value = ?1 WHERE id = 1", [value])
            .unwrap();
        (
            fs::read(&path).unwrap(),
            fs::read(sqlite_sidecar(&path, "-wal")).unwrap(),
        )
    }
}
