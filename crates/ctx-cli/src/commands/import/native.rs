use super::*;
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
    allow_partial_failures: bool,
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
        allow_partial_failures,
        preinventory,
    )
}

pub(crate) fn import_one_source_without_search_refresh(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    full_rescan: bool,
    allow_partial_failures: bool,
    preinventory: &SourcePreinventory,
) -> Result<ProviderImportSummary> {
    import_one_source_inner(
        store,
        source,
        progress,
        false,
        full_rescan,
        allow_partial_failures,
        preinventory,
    )
}

pub(crate) fn import_one_source_inner(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    refresh_search_after_import: bool,
    full_rescan: bool,
    allow_partial_failures: bool,
    preinventory: &SourcePreinventory,
) -> Result<ProviderImportSummary> {
    let record = import_record_for_source(source);
    let record_id = record.id;
    store.upsert_record(&record)?;
    // Non-manifest SQLite/tree sources still have a persisted root inventory.
    // Do not normalize an unchanged, already-indexed source just because a
    // background search discovered it again; those adapters may read the
    // entire provider database before deduplication can occur.
    if !full_rescan
        && !source_uses_import_file_manifest(source)
        && preinventory.source_root_file().is_some()
        && store
            .list_pending_source_import_files(source.provider, &source.path.display().to_string())?
            .is_empty()
    {
        return Ok(ProviderImportSummary::default());
    }
    let summary = if !full_rescan && source_uses_import_file_manifest(source) {
        import_manifested_source(
            store,
            source,
            record_id,
            progress,
            allow_partial_failures,
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
                                allow_partial_failures,
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
                            allow_partial_failures,
                            !preinventory.codex_session_tree_cataloged(),
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
                            allow_partial_failures,
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
                            allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
                    allow_partial_failures,
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
            if !allow_partial_failures && summary.failed > 0 {
                mark_source_root_inventory_failed(
                    store,
                    source,
                    preinventory,
                    &format!("provider import reported {} failure(s)", summary.failed),
                )?;
                let _ = store.delete_orphan_record(record_id);
                return Err(provider_import_summary_failure(source, &summary));
            }
            mark_source_root_inventory_indexed(store, preinventory)?;
            summary
        }
        Err(err) => {
            mark_source_root_inventory_failed(store, source, preinventory, &err.to_string())?;
            if !allow_partial_failures {
                let _ = store.delete_orphan_record(record_id);
            }
            return Err(err);
        }
    };
    if refresh_search_after_import {
        store.refresh_search_index()?;
    }
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
    anyhow!(
        "import {} source {} failed with {} failure(s); first failure: {detail}",
        source.provider.as_str(),
        source.path.display(),
        summary.failed
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn import_manifested_source(
    store: &mut Store,
    source: &SourceInfo,
    record_id: Uuid,
    progress: Option<CodexSessionImportProgressCallback>,
    allow_partial_failures: bool,
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

    if !allow_partial_failures {
        let imported = import_one_source_inner(
            store,
            source,
            progress,
            false,
            true,
            false,
            &SourcePreinventory::None,
        );
        match imported {
            Ok(summary) => {
                for pending_file in pending {
                    mark_source_import_file_indexed(
                        store,
                        source.provider,
                        &source_root,
                        &pending_file,
                    )?;
                }
                return Ok(summary);
            }
            Err(err) => {
                let error = err.to_string();
                for pending_file in pending {
                    mark_source_import_file_failed(
                        store,
                        source.provider,
                        &source_root,
                        &pending_file.source_path,
                        &error,
                    )?;
                }
                return Err(err);
            }
        }
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
            allow_partial_failures,
            &SourcePreinventory::None,
        );
        match imported {
            Ok(file_summary) => {
                if source_import_file_has_no_imported_content(&file_summary)
                    && file_summary.failed > 0
                {
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
                merge_provider_import_summary(&mut summary, file_summary);
            }
            Err(err) => {
                let error = error_summary(&err);
                mark_source_import_file_failed(
                    store,
                    source.provider,
                    &source_root,
                    &pending_file.source_path,
                    &error,
                )?;
                if import_error_is_systemic(&error) {
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

fn source_import_file_has_no_imported_content(summary: &ProviderImportSummary) -> bool {
    summary.imported_sessions == 0 && summary.imported_events == 0 && summary.imported_edges == 0
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

pub(crate) fn merge_provider_import_summary(
    summary: &mut ProviderImportSummary,
    other: ProviderImportSummary,
) {
    summary.imported += other.imported;
    summary.skipped += other.skipped;
    summary.failed += other.failed;
    summary.imported_sessions += other.imported_sessions;
    summary.skipped_sessions += other.skipped_sessions;
    summary.imported_events += other.imported_events;
    summary.skipped_events += other.skipped_events;
    summary.imported_edges += other.imported_edges;
    summary.skipped_edges += other.skipped_edges;
    summary.failures.extend(other.failures);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_sources::explicit_path_source;
    use ctx_history_store::{SourceImportFile, SourceImportFileIndexUpdate};
    use serde_json::json;

    #[test]
    fn unchanged_root_source_skips_provider_normalization() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("state.db");
        let source = explicit_path_source(CaptureProvider::Hermes, source_path.clone());
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let source_root = source_path.display().to_string();
        let file = SourceImportFile {
            provider: source.provider,
            source_format: source.source_format.to_owned(),
            source_root: source_root.clone(),
            source_path: source_root.clone(),
            file_size_bytes: 0,
            file_modified_at_ms: 0,
            observed_at_ms: 0,
            metadata: json!({}),
        };
        store
            .upsert_source_import_files(std::slice::from_ref(&file))
            .unwrap();
        store
            .mark_source_import_file_indexed(
                source.provider,
                SourceImportFileIndexUpdate {
                    source_root: &source_root,
                    source_path: &source_root,
                    file_size_bytes: file.file_size_bytes,
                    file_modified_at_ms: file.file_modified_at_ms,
                    indexed_at_ms: 1,
                },
            )
            .unwrap();

        let summary = import_one_source_inner(
            &mut store,
            &source,
            None,
            false,
            false,
            true,
            &SourcePreinventory::SourceRoot(file),
        )
        .unwrap();

        assert_eq!(summary.imported_events, 0);
        assert_eq!(summary.failed, 0);
    }
}
