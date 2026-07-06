#[allow(unused_imports)]
use super::*;

pub(crate) fn import_manifested_source(
    store: &mut Store,
    source: &SourceInfo,
    record_id: Uuid,
    tool_output_mode: CodexToolOutputMode,
    event_mode: CodexEventImportMode,
    include_notices: bool,
    progress: Option<CodexSessionImportProgressCallback>,
) -> Result<ProviderImportSummary> {
    let source_root = source.path.display().to_string();
    let files = collect_source_import_files(source)
        .with_context(|| format!("catalog import files from {}", source.path.display()))?;
    if files.is_empty() {
        return Err(anyhow!(
            "no importable {} history files found under {}",
            source.provider.as_str(),
            source.path.display()
        ));
    }
    let current_paths = files
        .iter()
        .map(|file| file.source_path.clone())
        .collect::<Vec<_>>();
    let observed_at_ms = utc_now().timestamp_millis();
    store.begin_immediate_batch()?;
    let persist = (|| -> Result<()> {
        store.upsert_source_import_files(&files)?;
        store.mark_source_import_missing_paths_stale(
            source.provider,
            &source_root,
            &current_paths,
            observed_at_ms,
        )?;
        Ok(())
    })();
    match persist {
        Ok(()) => store.commit_batch()?,
        Err(err) => {
            let _ = store.rollback_batch();
            return Err(err);
        }
    }

    let pending = store.list_pending_source_import_files(source.provider, &source_root)?;
    if pending.is_empty() {
        return Ok(ProviderImportSummary::default());
    }

    let mut summary = ProviderImportSummary::default();
    for pending_file in pending {
        let path = PathBuf::from(&pending_file.source_path);
        let mut pending_source = explicit_path_source(source.provider, path);
        pending_source.source_format = source.source_format;
        let imported =
            import_one_source_inner(store, &pending_source, progress.clone(), false, true);
        match imported {
            Ok(file_summary) => {
                store.mark_source_import_file_indexed(
                    source.provider,
                    SourceImportFileIndexUpdate {
                        source_root: &source_root,
                        source_path: &pending_file.source_path,
                        file_size_bytes: pending_file.file_size_bytes,
                        file_modified_at_ms: pending_file.file_modified_at_ms,
                        indexed_at_ms: utc_now().timestamp_millis(),
                    },
                )?;
                merge_provider_import_summary(&mut summary, file_summary);
            }
            Err(err) => {
                store.mark_source_import_file_failed(
                    source.provider,
                    &source_root,
                    &pending_file.source_path,
                    &err.to_string(),
                    utc_now().timestamp_millis(),
                )?;
                return Err(err);
            }
        }
    }

    let _ = record_id;
    let _ = tool_output_mode;
    let _ = event_mode;
    let _ = include_notices;
    Ok(summary)
}

pub(crate) fn source_uses_import_file_manifest(source: &SourceInfo) -> bool {
    !matches!(
        source.source_format,
        "codex_session_jsonl_tree"
            | "openclaw_session_jsonl_tree"
            | "openhands_file_events"
            | "hermes_state_sqlite"
            | "nanoclaw_project"
            | "astrbot_data_v4_sqlite"
            | "shelley_sqlite"
            | "cline_task_directory_json"
            | "roo_task_directory_json"
            | "firebender_chat_history_sqlite"
            | "codebuddy_history_json"
    )
}

pub(crate) fn source_import_file_matches(source: &SourceInfo, path: &Path) -> bool {
    match source.provider {
        CaptureProvider::Codex | CaptureProvider::Pi | CaptureProvider::FactoryAiDroid => {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
        }
        CaptureProvider::Claude => {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && path.starts_with(&source.path)
        }
        CaptureProvider::OpenCode
        | CaptureProvider::Kilo
        | CaptureProvider::KiroCli
        | CaptureProvider::ForgeCode
        | CaptureProvider::DeepAgents
        | CaptureProvider::Crush
        | CaptureProvider::Goose
        | CaptureProvider::Lingma
        | CaptureProvider::Warp
        | CaptureProvider::Zed => path == source.path,
        CaptureProvider::MistralVibe => {
            path == source.path
                || (path.file_name().and_then(|name| name.to_str()) == Some("messages.jsonl")
                    && path.starts_with(&source.path))
        }
        CaptureProvider::Mux => {
            path == source.path
                || (matches!(
                    path.file_name().and_then(|name| name.to_str()),
                    Some("chat.jsonl" | "partial.json")
                ) && path.starts_with(&source.path))
        }
        CaptureProvider::RovoDev => {
            path.file_name().and_then(|name| name.to_str()) == Some("session_context.json")
        }
        CaptureProvider::CopilotCli => {
            path.file_name().and_then(|name| name.to_str()) == Some("events.jsonl")
        }
        CaptureProvider::Antigravity => matches!(
            path.file_name().and_then(|name| name.to_str()),
            Some("transcript_full.jsonl" | "transcript.jsonl")
        ),
        CaptureProvider::Gemini | CaptureProvider::Tabnine => {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "chats")
        }
        CaptureProvider::Cursor => {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "agent-transcripts")
        }
        CaptureProvider::Windsurf => path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"),
        CaptureProvider::Qoder => {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "transcript")
        }
        CaptureProvider::Continue => {
            path.extension().and_then(|ext| ext.to_str()) == Some("json")
                && path.file_name().and_then(|name| name.to_str()) != Some("sessions.json")
        }
        CaptureProvider::QwenCode => {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "chats")
        }
        CaptureProvider::CodeBuddy => {
            path.extension().and_then(|ext| ext.to_str()) == Some("json")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "history")
        }
        CaptureProvider::Trae => {
            path.file_name().and_then(|name| name.to_str()) == Some("state.vscdb")
                && (path == source.path || path.starts_with(&source.path))
        }
        CaptureProvider::KimiCodeCli => {
            path.file_name().and_then(|name| name.to_str()) == Some("wire.jsonl")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "agents")
        }
        CaptureProvider::Auggie => {
            path.extension().and_then(|ext| ext.to_str()) == Some("json")
                && path.starts_with(&source.path)
        }
        CaptureProvider::Junie => {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == "events.jsonl")
                && path.starts_with(&source.path)
        }
        CaptureProvider::Firebender => {
            path.file_name().and_then(|name| name.to_str()) == Some("chat_history.db")
                && (path == source.path || path.starts_with(&source.path))
        }
        CaptureProvider::OpenClaw => {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && path.starts_with(&source.path)
        }
        CaptureProvider::Hermes
        | CaptureProvider::NanoClaw
        | CaptureProvider::AstrBot
        | CaptureProvider::Shelley
        | CaptureProvider::OpenHands
        | CaptureProvider::Cline
        | CaptureProvider::RooCode
        | CaptureProvider::Shell
        | CaptureProvider::Git
        | CaptureProvider::Jj
        | CaptureProvider::Gh
        | CaptureProvider::Custom
        | CaptureProvider::Unknown => false,
    }
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

pub(crate) fn current_codex_provider_session_filter(
    store: Option<&Store>,
) -> Option<ctx_history_search::ProviderSessionFilter> {
    let provider_session_id = std::env::var("CODEX_THREAD_ID").ok()?;
    let provider_session_id = provider_session_id.trim();
    if provider_session_id.is_empty() {
        return None;
    }
    let session_id = store
        .and_then(|store| {
            store
                .session_by_external_session(CaptureProvider::Codex, provider_session_id)
                .ok()
                .flatten()
        })
        .map(|session| session.id);
    Some(ctx_history_search::ProviderSessionFilter {
        provider: CaptureProvider::Codex,
        provider_session_id: provider_session_id.to_owned(),
        session_id,
    })
}
