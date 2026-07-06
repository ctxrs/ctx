use super::*;
use crate::commands::import::native::{collect_source_import_paths, merge_provider_import_summary};

pub(crate) fn system_time_ms(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

pub(crate) fn import_incremental_codex_session_tree(
    store: &mut Store,
    source: &SourceInfo,
    record_id: Uuid,
    tool_output_mode: CodexToolOutputMode,
    event_mode: CodexEventImportMode,
    include_notices: bool,
    progress: Option<CodexSessionImportProgressCallback>,
) -> Result<ProviderImportSummary> {
    let source_root = source.path.display().to_string();
    catalog_codex_session_tree(
        &source.path,
        store,
        CodexSessionCatalogOptions {
            source_root: Some(source.path.clone()),
            allow_partial_failures: true,
            ..CodexSessionCatalogOptions::default()
        },
    )
    .with_context(|| format!("catalog Codex sessions from {}", source.path.display()))?;

    let pending = store.list_pending_catalog_sessions(CaptureProvider::Codex, &source_root)?;
    if pending.is_empty() {
        return Ok(ProviderImportSummary::default());
    }

    let mut summary = ProviderImportSummary::default();
    let mut full_import_sessions = Vec::new();
    for session in &pending {
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
            if !catalog_import_checkpoint_matches(
                Path::new(&session.source_path),
                start_offset,
                checkpoint_hash,
            )? {
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
                    allow_partial_failures: true,
                    tool_output_mode,
                    event_mode,
                    include_notices,
                    progress: progress.clone(),
                    ..CodexSessionImportOptions::default()
                },
            )
            .map_err(anyhow::Error::from)
            {
                Ok(summary) => summary,
                Err(err) => {
                    mark_catalog_sessions_failed(
                        store,
                        std::slice::from_ref(session),
                        &err.to_string(),
                    )?;
                    return Err(err);
                }
            };
            if tail_summary.failed > 0 {
                mark_catalog_sessions_failed(
                    store,
                    std::slice::from_ref(session),
                    "tail import failed for one or more appended events",
                )?;
                merge_provider_import_summary(&mut summary, tail_summary);
                continue;
            }
            let tail_event_count = tail_summary
                .imported_events
                .saturating_add(tail_summary.skipped_events)
                as u64;
            let event_count = state
                .and_then(|state| state.last_imported_event_count)
                .map(|event_count| event_count.saturating_add(tail_event_count));
            mark_catalog_session_indexed(
                store,
                session,
                event_count,
                utc_now().timestamp_millis(),
            )?;
            merge_provider_import_summary(&mut summary, tail_summary);
        } else {
            full_import_sessions.push(session.clone());
        }
    }

    if !full_import_sessions.is_empty() {
        let paths = full_import_sessions
            .iter()
            .map(|session| PathBuf::from(&session.source_path))
            .collect::<Vec<_>>();
        let full_summary = match import_codex_session_paths(
            paths,
            store,
            CodexSessionImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                tool_output_mode,
                event_mode,
                include_notices,
                progress,
                ..CodexSessionImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from)
        {
            Ok(summary) => summary,
            Err(err) => {
                mark_catalog_sessions_failed(store, &full_import_sessions, &err.to_string())?;
                return Err(err);
            }
        };
        mark_catalog_sessions_indexed(store, &full_import_sessions, &full_summary)?;
        merge_provider_import_summary(&mut summary, full_summary);
    }
    Ok(summary)
}

pub(crate) fn mark_catalog_sessions_indexed(
    store: &Store,
    sessions: &[CatalogSession],
    summary: &ProviderImportSummary,
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
    for session in sessions {
        mark_catalog_session_indexed(store, session, event_count, indexed_at_ms)?;
    }
    Ok(())
}

pub(crate) fn mark_catalog_session_indexed(
    store: &Store,
    session: &CatalogSession,
    event_count: Option<u64>,
    indexed_at_ms: i64,
) -> Result<()> {
    let file_sha256 =
        sha256_file_prefix_hex(Path::new(&session.source_path), session.file_size_bytes)
            .with_context(|| format!("hash checkpoint prefix for {}", session.source_path))?;
    store.mark_catalog_source_indexed(
        session.provider,
        CatalogSourceIndexUpdate {
            source_root: &session.source_root,
            source_path: &session.source_path,
            file_size_bytes: session.file_size_bytes,
            file_modified_at_ms: session.file_modified_at_ms,
            file_sha256: Some(&file_sha256),
            event_count,
            indexed_at_ms,
        },
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

pub(crate) fn mark_catalog_sessions_failed(
    store: &Store,
    sessions: &[CatalogSession],
    error: &str,
) -> Result<()> {
    let indexed_at_ms = utc_now().timestamp_millis();
    for session in sessions {
        store.mark_catalog_source_failed(
            session.provider,
            &session.source_root,
            &session.source_path,
            error,
            indexed_at_ms,
        )?;
    }
    Ok(())
}

pub(crate) fn source_uses_incremental_event_search(source: &SourceInfo) -> bool {
    matches!(
        source.provider,
        CaptureProvider::Codex
            | CaptureProvider::Claude
            | CaptureProvider::Pi
            | CaptureProvider::Cursor
            | CaptureProvider::OpenCode
            | CaptureProvider::Kilo
            | CaptureProvider::KiroCli
            | CaptureProvider::Crush
            | CaptureProvider::Goose
            | CaptureProvider::Warp
            | CaptureProvider::Antigravity
            | CaptureProvider::Gemini
            | CaptureProvider::Tabnine
            | CaptureProvider::Windsurf
            | CaptureProvider::Qoder
            | CaptureProvider::CopilotCli
            | CaptureProvider::FactoryAiDroid
            | CaptureProvider::Continue
            | CaptureProvider::QwenCode
            | CaptureProvider::KimiCodeCli
            | CaptureProvider::Auggie
            | CaptureProvider::Junie
            | CaptureProvider::Firebender
            | CaptureProvider::ForgeCode
            | CaptureProvider::DeepAgents
            | CaptureProvider::MistralVibe
            | CaptureProvider::Mux
            | CaptureProvider::RovoDev
            | CaptureProvider::Cline
            | CaptureProvider::RooCode
            | CaptureProvider::CodeBuddy
            | CaptureProvider::Trae
    )
}

pub(crate) fn codex_tool_output_mode() -> Result<CodexToolOutputMode> {
    if let Some(raw) = env::var_os("CTX_CODEX_TOOL_OUTPUT_MODE") {
        let raw = raw.to_string_lossy();
        return match raw.as_ref() {
            "full" => Ok(CodexToolOutputMode::Full),
            "metadata" => Ok(CodexToolOutputMode::Metadata),
            "failures" | "failure" | "errors" | "error" => Ok(CodexToolOutputMode::Failures),
            "skip" => Ok(CodexToolOutputMode::Skip),
            other => Err(anyhow!(
                "unsupported CTX_CODEX_TOOL_OUTPUT_MODE={other:?}; expected full, metadata, failures, or skip"
            )),
        };
    }
    if env::var_os("CTX_EXPERIMENTAL_SKIP_TOOL_OUTPUTS").is_some() {
        return Ok(CodexToolOutputMode::Skip);
    }
    Ok(CodexToolOutputMode::Skip)
}

pub(crate) fn codex_event_import_mode() -> Result<CodexEventImportMode> {
    if let Some(raw) = env::var_os("CTX_CODEX_EVENT_MODE") {
        let raw = raw.to_string_lossy();
        return match raw.as_ref() {
            "search" | "message" | "messages" => Ok(CodexEventImportMode::Search),
            "rich" | "full" => Ok(CodexEventImportMode::Rich),
            other => Err(anyhow!(
                "unsupported CTX_CODEX_EVENT_MODE={other:?}; expected search or rich"
            )),
        };
    }
    Ok(CodexEventImportMode::Search)
}

pub(crate) fn codex_include_notices() -> bool {
    env::var_os("CTX_CODEX_INCLUDE_NOTICES").is_some()
}

pub(crate) fn source_stats(path: &Path) -> Result<SourceStats> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("stat import source {}", path.display()))?;
    if metadata.file_type().is_file() {
        return Ok(SourceStats {
            files: 1,
            bytes: metadata.len(),
        });
    }
    if !metadata.file_type().is_dir() {
        return Ok(SourceStats::default());
    }

    let mut stats = SourceStats::default();
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
                stats.files += 1;
                stats.bytes = stats.bytes.saturating_add(metadata.len());
            }
        }
    }
    Ok(stats)
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
