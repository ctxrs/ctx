use super::*;
use crate::commands::import::catalog::{
    codex_event_import_mode, codex_include_notices, codex_tool_output_mode, system_time_ms,
};

pub(crate) fn validate_source_import_supported(source: &SourceInfo) -> Result<()> {
    match source.import_support {
        ProviderImportSupport::Native => Ok(()),
        ProviderImportSupport::Explicit => Ok(()),
        ProviderImportSupport::Unsupported => {
            let reason = source
                .unsupported_reason
                .unwrap_or("no native local-history parser is implemented");
            Err(anyhow!(
                "{} native import is unsupported: {reason}",
                source.provider.as_str()
            ))
        }
    }
}

pub(crate) fn import_one_source(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    full_rescan: bool,
) -> Result<ProviderImportSummary> {
    let event_search_needs_backfill = store.event_search_projection_needs_backfill()?;
    let refresh_search_after_import =
        event_search_needs_backfill || !source_uses_incremental_event_search(source);
    import_one_source_inner(
        store,
        source,
        progress,
        refresh_search_after_import,
        full_rescan,
    )
}

pub(crate) fn import_one_source_without_search_refresh(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    full_rescan: bool,
) -> Result<ProviderImportSummary> {
    import_one_source_inner(store, source, progress, false, full_rescan)
}

pub(crate) fn import_one_source_inner(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    refresh_search_after_import: bool,
    full_rescan: bool,
) -> Result<ProviderImportSummary> {
    let record = import_record_for_source(source);
    let record_id = record.id;
    store.upsert_record(&record)?;
    let tool_output_mode = codex_tool_output_mode()?;
    let event_mode = codex_event_import_mode()?;
    let include_notices = codex_include_notices();
    if !full_rescan && source_uses_import_file_manifest(source) {
        return import_manifested_source(
            store,
            source,
            record_id,
            tool_output_mode,
            event_mode,
            include_notices,
            progress,
        );
    }
    let summary = match source.provider {
        CaptureProvider::Codex => {
            if source.path.is_dir() {
                if full_rescan {
                    import_codex_session_tree(
                        &source.path,
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
                } else {
                    import_incremental_codex_session_tree(
                        store,
                        source,
                        record_id,
                        tool_output_mode,
                        event_mode,
                        include_notices,
                        progress.clone(),
                    )
                }
            } else if source
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == "history.jsonl")
            {
                import_codex_history_jsonl(
                    &source.path,
                    store,
                    CodexHistoryImportOptions {
                        source_path: Some(source.path.clone()),
                        history_record_id: Some(record_id),
                        allow_partial_failures: true,
                        ..CodexHistoryImportOptions::default()
                    },
                )
                .map_err(anyhow::Error::from)
            } else {
                import_codex_session_jsonl(
                    &source.path,
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
            }
        }
        CaptureProvider::Pi => import_pi_session_jsonl(
            &source.path,
            store,
            PiSessionImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..PiSessionImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Claude => import_claude_projects_jsonl_tree(
            &source.path,
            store,
            ClaudeProjectsImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..ClaudeProjectsImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Cline => import_cline_task_json_history(
            &source.path,
            store,
            ClineTaskJsonImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..ClineTaskJsonImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::RooCode => import_roo_task_json_history(
            &source.path,
            store,
            RooTaskJsonImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..RooTaskJsonImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::CodeBuddy => import_codebuddy_history(
            &source.path,
            store,
            CodeBuddyImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..CodeBuddyImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Trae => import_trae_history(
            &source.path,
            store,
            TraeImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..TraeImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::OpenCode => import_opencode_sqlite(
            &source.path,
            store,
            OpenCodeSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..OpenCodeSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Kilo => import_kilo_sqlite(
            &source.path,
            store,
            KiloSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..KiloSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::KiroCli => import_kiro_sqlite(
            &source.path,
            store,
            KiroSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..KiroSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::ForgeCode => import_forgecode_sqlite(
            &source.path,
            store,
            ForgeCodeSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..ForgeCodeSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::DeepAgents => import_deepagents_sqlite(
            &source.path,
            store,
            DeepAgentsSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..DeepAgentsSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Crush => import_crush_sqlite(
            &source.path,
            store,
            CrushSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..CrushSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Goose => import_goose_sessions_sqlite(
            &source.path,
            store,
            GooseSessionsSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..GooseSessionsSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::OpenClaw => import_openclaw_history(
            &source.path,
            store,
            OpenClawImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..OpenClawImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Hermes => import_hermes_sqlite(
            &source.path,
            store,
            HermesSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..HermesSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::NanoClaw => import_nanoclaw_project(
            &source.path,
            store,
            NanoClawImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..NanoClawImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::AstrBot => import_astrbot_sqlite(
            &source.path,
            store,
            AstrBotSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..AstrBotSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Shelley => import_shelley_sqlite(
            &source.path,
            store,
            ShelleySqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..ShelleySqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Continue => import_continue_cli_sessions(
            &source.path,
            store,
            ContinueCliImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..ContinueCliImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::OpenHands => import_openhands_file_events(
            &source.path,
            store,
            OpenHandsImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..OpenHandsImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Lingma => import_lingma_sqlite(
            &source.path,
            store,
            LingmaSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..LingmaSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Qoder => import_qoder_history(
            &source.path,
            store,
            QoderImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..QoderImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Warp => import_warp_sqlite(
            &source.path,
            store,
            WarpSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..WarpSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Gemini => import_gemini_cli_history(
            &source.path,
            store,
            GeminiCliImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..GeminiCliImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Tabnine => import_tabnine_cli_history(
            &source.path,
            store,
            TabnineCliImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..TabnineCliImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Cursor => import_cursor_native_history(
            &source.path,
            store,
            CursorNativeImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..CursorNativeImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Windsurf => import_windsurf_cascade_hook_transcripts(
            &source.path,
            store,
            WindsurfCascadeHookImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..WindsurfCascadeHookImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Zed => import_zed_threads_sqlite(
            &source.path,
            store,
            ZedThreadsSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..ZedThreadsSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::CopilotCli => import_copilot_cli_session_events(
            &source.path,
            store,
            CopilotCliImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..CopilotCliImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::FactoryAiDroid => import_factory_ai_droid_sessions(
            &source.path,
            store,
            FactoryAiDroidImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..FactoryAiDroidImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::QwenCode => import_qwen_code_history(
            &source.path,
            store,
            QwenCodeImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..QwenCodeImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::KimiCodeCli => import_kimi_code_cli_history(
            &source.path,
            store,
            KimiCodeCliImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..KimiCodeCliImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Auggie => import_auggie_history(
            &source.path,
            store,
            AuggieImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..AuggieImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Junie => import_junie_history(
            &source.path,
            store,
            JunieImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..JunieImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Firebender => import_firebender_sqlite(
            &source.path,
            store,
            FirebenderSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..FirebenderSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::RovoDev => import_rovodev_history(
            &source.path,
            store,
            RovoDevImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..RovoDevImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::MistralVibe => import_mistral_vibe_history(
            &source.path,
            store,
            MistralVibeImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..MistralVibeImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Mux => import_mux_history(
            &source.path,
            store,
            MuxImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..MuxImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Antigravity => import_antigravity_cli_history(
            &source.path,
            store,
            AntigravityCliImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: true,
                ..AntigravityCliImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        other => Err(anyhow!(
            "{} is not registered for provider history import",
            other.as_str()
        )),
    }?;
    if refresh_search_after_import {
        store.refresh_search_index()?;
    }
    Ok(summary)
}

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

pub(crate) fn merge_provider_import_summary(
    summary: &mut ProviderImportSummary,
    other: ProviderImportSummary,
) {
    summary.imported += other.imported;
    summary.skipped += other.skipped;
    summary.failed += other.failed;
    summary.redacted += other.redacted;
    summary.imported_sessions += other.imported_sessions;
    summary.skipped_sessions += other.skipped_sessions;
    summary.imported_events += other.imported_events;
    summary.skipped_events += other.skipped_events;
    summary.imported_edges += other.imported_edges;
    summary.skipped_edges += other.skipped_edges;
    summary.failures.extend(other.failures);
}

pub(crate) fn collect_source_import_files(source: &SourceInfo) -> Result<Vec<SourceImportFile>> {
    let paths = collect_source_import_paths(source)?;
    let source_root = source.path.display().to_string();
    let observed_at_ms = utc_now().timestamp_millis();
    let mut files = Vec::with_capacity(paths.len());
    for path in paths {
        let metadata = fs::metadata(&path)
            .with_context(|| format!("stat import source file {}", path.display()))?;
        files.push(SourceImportFile {
            provider: source.provider,
            source_format: source.source_format.to_owned(),
            source_root: source_root.clone(),
            source_path: path.display().to_string(),
            file_size_bytes: metadata.len(),
            file_modified_at_ms: system_time_ms(metadata.modified().unwrap_or(UNIX_EPOCH)),
            observed_at_ms,
            metadata: json!({}),
        });
    }
    Ok(files)
}

pub(crate) fn collect_source_import_paths(source: &SourceInfo) -> Result<Vec<PathBuf>> {
    let metadata = fs::symlink_metadata(&source.path)
        .with_context(|| format!("stat import source {}", source.path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(anyhow!(
            "symlinked provider transcript roots are rejected: {}",
            source.path.display()
        ));
    }
    if metadata.file_type().is_file() {
        return Ok(if source_import_file_matches(source, &source.path) {
            vec![source.path.clone()]
        } else {
            Vec::new()
        });
    }
    if !metadata.file_type().is_dir() {
        return Ok(Vec::new());
    }

    let mut paths = Vec::new();
    let mut stack = vec![source.path.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)
            .with_context(|| format!("read import source directory {}", dir.display()))?
        {
            let entry = entry
                .with_context(|| format!("read import source entry under {}", dir.display()))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .with_context(|| format!("stat import source entry {}", path.display()))?;
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() && source_import_file_matches(source, &path) {
                paths.push(path);
            }
        }
    }
    paths.sort();
    Ok(paths)
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
