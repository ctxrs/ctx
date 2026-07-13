use super::*;
use crate::commands::import::catalog::source_uses_incremental_event_search;
use crate::commands::import::manifest::{
    collect_source_import_files, persist_source_import_files, source_uses_import_file_manifest,
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
    preinventory: &SourcePreinventory,
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
        preinventory,
    )
}

pub(crate) fn import_one_source_without_search_refresh(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    full_rescan: bool,
    preinventory: &SourcePreinventory,
) -> Result<ProviderImportSummary> {
    import_one_source_inner(store, source, progress, false, full_rescan, preinventory)
}

pub(crate) fn import_one_source_for_search_refresh(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    preinventory: &SourcePreinventory,
) -> Result<ProviderImportSummary> {
    if !source_preinventory_has_pending_work(store, source, preinventory)? {
        if store.event_search_projection_needs_backfill()? {
            store.refresh_search_index()?;
        }
        return Ok(ProviderImportSummary::default());
    }
    import_one_source_without_search_refresh(store, source, progress, false, preinventory)
}

pub(crate) fn import_one_source_inner(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    refresh_search_after_import: bool,
    full_rescan: bool,
    preinventory: &SourcePreinventory,
) -> Result<ProviderImportSummary> {
    if !full_rescan
        && !refresh_search_after_import
        && !source_preinventory_has_pending_work(store, source, preinventory)?
    {
        return Ok(ProviderImportSummary::default());
    }
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let import_result =
        import_one_source_inner_batched(store, source, progress, full_rescan, preinventory);
    let finish_result = store.finish_event_search_bulk_mode(&bulk_guard);
    let summary = match (import_result, finish_result) {
        (Ok(summary), Ok(())) => Ok(summary),
        (_, Err(error)) => Err(error.into()),
        (Err(error), Ok(())) => Err(error),
    }?;
    if refresh_search_after_import {
        store.refresh_search_index()?;
    }
    Ok(summary)
}

fn source_preinventory_has_pending_work(
    store: &Store,
    source: &SourceInfo,
    preinventory: &SourcePreinventory,
) -> Result<bool> {
    let source_root = source.path.display().to_string();
    match preinventory {
        SourcePreinventory::CodexSessionCatalog(_) => Ok(!store
            .list_pending_catalog_sessions(source.provider, &source_root)?
            .is_empty()),
        SourcePreinventory::SourceImportFiles(_) | SourcePreinventory::SourceRoot(_) => Ok(!store
            .list_pending_source_import_files(source.provider, &source_root)?
            .is_empty()),
        SourcePreinventory::None => Ok(true),
    }
}

fn import_one_source_inner_batched(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    full_rescan: bool,
    preinventory: &SourcePreinventory,
) -> Result<ProviderImportSummary> {
    let record = import_record_for_source(source);
    let record_id = record.id;
    let record_existed = history_record_exists(store, record_id)?;
    store.upsert_record(&record)?;
    let summary = if !full_rescan && source_uses_import_file_manifest(source) {
        import_manifested_source(
            store,
            source,
            record_id,
            progress,
            preinventory.source_import_files(),
        )
    } else {
        match source.provider {
            CaptureProvider::Codex => {
                if source.path.is_dir() {
                    if full_rescan {
                        import_codex_session_tree(
                            &source.path,
                            store,
                            CodexSessionImportOptions {
                                source_path: Some(source.path.clone()),
                                history_record_id: Some(record_id),
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
                            progress.clone(),
                            preinventory.codex_session_catalog(),
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
                    ..KiloSqliteImportOptions::default()
                },
            )
            .map_err(anyhow::Error::from),
            CaptureProvider::MiMoCode => import_mimocode_sqlite(
                &source.path,
                store,
                MiMoCodeSqliteImportOptions {
                    source_path: Some(source.path.clone()),
                    history_record_id: Some(record_id),
                    ..MiMoCodeSqliteImportOptions::default()
                },
            )
            .map_err(anyhow::Error::from),
            CaptureProvider::KiroCli => import_kiro_sqlite(
                &source.path,
                store,
                KiroSqliteImportOptions {
                    source_path: Some(source.path.clone()),
                    history_record_id: Some(record_id),
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
                    ..AntigravityCliImportOptions::default()
                },
            )
            .map_err(anyhow::Error::from),
            other => Err(anyhow!(
                "{} is not registered for provider history import",
                other.as_str()
            )),
        }
    };
    let summary = match summary {
        Ok(summary) => {
            // A manifested-source retry can contain only rejected files even though earlier
            // files under the same stable history record are already indexed. Preserve that
            // as a completed source with rejections; an orphan record is still cleaned up and
            // remains an all-rejected source failure.
            let retained_existing_content = if summary.failed > 0
                && !provider_summary_has_imported_content(&summary)
                && record_existed
            {
                !store.delete_orphan_record(record_id)? && history_record_exists(store, record_id)?
            } else {
                false
            };
            if summary.failed > 0
                && !provider_summary_has_imported_content(&summary)
                && !retained_existing_content
            {
                mark_source_root_inventory_failed(
                    store,
                    source,
                    preinventory,
                    &format!("provider import reported {} failure(s)", summary.failed),
                )?;
                cleanup_rejected_history_record(store, record_id, record_existed)?;
                return Err(provider_import_summary_failure(source, &summary));
            }
            mark_source_root_inventory_indexed(store, preinventory)?;
            summary
        }
        Err(err) => {
            mark_source_root_inventory_failed(store, source, preinventory, &err.to_string())?;
            let deleted = store.delete_orphan_record(record_id)?;
            if import_error_scope(&err) == ImportFailureScope::Source
                && !deleted
                && !record_existed
                && history_record_exists(store, record_id)?
            {
                return Err(anyhow::Error::new(CaptureError::SystemInvariant(
                    "failed source import left content attached to its history record",
                )));
            }
            return Err(err);
        }
    };
    Ok(summary)
}

fn mark_source_root_inventory_indexed(
    store: &Store,
    preinventory: &SourcePreinventory,
) -> Result<()> {
    let Some(file) = preinventory.source_root_file() else {
        return Ok(());
    };
    mark_source_import_file_indexed(store, file.provider, &file.source_root, file)
}

fn mark_source_root_inventory_failed(
    store: &Store,
    source: &SourceInfo,
    preinventory: &SourcePreinventory,
    error: &str,
) -> Result<()> {
    let Some(file) = preinventory.source_root_file() else {
        return Ok(());
    };
    mark_source_import_file_failed(
        store,
        source.provider,
        &file.source_root,
        &file.source_path,
        error,
    )
}

fn mark_source_import_file_failed(
    store: &Store,
    provider: CaptureProvider,
    source_root: &str,
    source_path: &str,
    error: &str,
) -> Result<()> {
    store.mark_source_import_file_failed(
        provider,
        source_root,
        source_path,
        error,
        utc_now().timestamp_millis(),
    )?;
    Ok(())
}

fn mark_source_import_file_indexed(
    store: &Store,
    provider: CaptureProvider,
    source_root: &str,
    file: &SourceImportFile,
) -> Result<()> {
    store.mark_source_import_file_indexed(
        provider,
        SourceImportFileIndexUpdate {
            source_root,
            source_path: &file.source_path,
            file_size_bytes: file.file_size_bytes,
            file_modified_at_ms: file.file_modified_at_ms,
            indexed_at_ms: utc_now().timestamp_millis(),
        },
    )?;
    Ok(())
}

pub(crate) fn provider_import_summary_failure(
    source: &SourceInfo,
    summary: &ProviderImportSummary,
) -> anyhow::Error {
    let detail = summary
        .failures
        .first()
        .map(|failure| format!("line {}: {}", failure.line, failure.error))
        .unwrap_or_else(|| "unknown provider import failure".to_owned());
    rejected_source_error(
        format!(
            "import {} source {} failed with {} failure(s); first failure: {detail}",
            source.provider.as_str(),
            source.path.display(),
            summary.failed
        ),
        summary,
    )
}

pub(crate) fn import_manifested_source(
    store: &mut Store,
    source: &SourceInfo,
    record_id: Uuid,
    progress: Option<CodexSessionImportProgressCallback>,
    preinventoried_files: Option<&[SourceImportFile]>,
) -> Result<ProviderImportSummary> {
    let source_root = source.path.display().to_string();
    let collected_files;
    let files = match preinventoried_files {
        Some(files) => files,
        None => {
            collected_files = collect_source_import_files(source).with_context(|| {
                format!("inventory import files from {}", source.path.display())
            })?;
            persist_source_import_files(store, source, &collected_files)?;
            &collected_files
        }
    };
    if files.is_empty() {
        return Err(anyhow!(
            "no importable {} history files found under {}",
            source.provider.as_str(),
            source.path.display()
        ));
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
        let imported = import_one_source_inner(
            store,
            &pending_source,
            progress.clone(),
            false,
            true,
            &SourcePreinventory::None,
        );
        match imported {
            Ok(file_summary) => {
                if file_summary.failed > 0 {
                    mark_source_import_file_failed(
                        store,
                        source.provider,
                        &source_root,
                        &pending_file.source_path,
                        &source_import_file_failure(&file_summary),
                    )?;
                } else {
                    mark_source_import_file_indexed(
                        store,
                        source.provider,
                        &source_root,
                        &pending_file,
                    )?;
                }
                summary.merge_from(file_summary);
            }
            Err(err) => {
                let failure_scope = import_error_scope(&err);
                let error = error_summary(&err);
                mark_source_import_file_failed(
                    store,
                    source.provider,
                    &source_root,
                    &pending_file.source_path,
                    &error,
                )?;
                if failure_scope == ImportFailureScope::System {
                    return Err(err);
                }
                summary.failed += 1;
                summary
                    .failures
                    .push(ProviderImportFailure { line: 0, error });
            }
        }
    }
    let _ = record_id;
    Ok(summary)
}

fn source_import_file_failure(summary: &ProviderImportSummary) -> String {
    let Some(failure) = summary.failures.first() else {
        return "provider import failed".to_owned();
    };
    match failure.line {
        0 => failure.error.clone(),
        line => format!("line {line}: {}", failure.error),
    }
}

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;
