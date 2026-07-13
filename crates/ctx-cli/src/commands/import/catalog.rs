use super::*;
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
    let source_root = source.path.display().to_string();
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
    store.record_catalog_source_import_result(
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
        store.record_catalog_source_import_result(
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
        add_source_stat(
            &mut stats,
            &mut change_entries,
            path.parent().unwrap_or(path),
            path,
            &metadata,
            true,
            true,
        );
        // WAL and rollback-journal files can hold committed changes that have
        // not reached the main database. The shared-memory file is excluded
        // because read-only SQLite clients may update it.
        for suffix in ["-wal", "-journal"] {
            let mut sidecar = path.as_os_str().to_os_string();
            sidecar.push(suffix);
            let sidecar = PathBuf::from(sidecar);
            match fs::symlink_metadata(&sidecar) {
                Ok(metadata) if metadata.file_type().is_file() => add_source_stat(
                    &mut stats,
                    &mut change_entries,
                    path.parent().unwrap_or(path),
                    &sidecar,
                    &metadata,
                    false,
                    true,
                ),
                Ok(_) => {
                    return Err(anyhow!(
                        "import source sidecar is not a regular file: {}",
                        sidecar.display()
                    ))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("stat import source sidecar {}", sidecar.display())
                    })
                }
            }
        }
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
                let metadata = entry
                    .metadata()
                    .with_context(|| format!("stat import source file {}", entry_path.display()))?;
                let include_in_token = !entry_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with("-shm"));
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

struct SourceChangeEntry {
    path: PathBuf,
    len: u64,
    modified_secs: u64,
    modified_nanos: u32,
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
    let modified = metadata
        .modified()
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    change_entries.push(SourceChangeEntry {
        path: path.strip_prefix(base).unwrap_or(path).to_path_buf(),
        len: metadata.len(),
        modified_secs: modified.as_secs(),
        modified_nanos: modified.subsec_nanos(),
    });
}

fn source_change_token(mut entries: Vec<SourceChangeEntry>) -> [u8; 32] {
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let mut hasher = Sha256::new();
    for entry in entries {
        let path = entry.path.as_os_str().as_encoded_bytes();
        hasher.update((path.len() as u64).to_le_bytes());
        hasher.update(path);
        hasher.update(entry.len.to_le_bytes());
        hasher.update(entry.modified_secs.to_le_bytes());
        hasher.update(entry.modified_nanos.to_le_bytes());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_sources::explicit_path_source;
    use ctx_history_capture::provider_source_specs;

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
}
