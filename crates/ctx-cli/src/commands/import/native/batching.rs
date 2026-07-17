fn import_one_source_inner_batched(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    full_rescan: bool,
    preinventory: &SourcePreinventory,
    selection: Option<&SelectedImportWork>,
) -> Result<ProviderImportBatchOutcome> {
    if source.provider == CaptureProvider::Codex {
        super::catalog::codex_catalog_root_identity(&source.path)?;
    }
    let record = import_record_for_source(source);
    if let Some(selection) = selection {
        if let Some(outcome) =
            import_selected_fresh_new_group(store, source, preinventory, selection, &record)?
        {
            return Ok(outcome);
        }
    }
    let record_id = record.id;
    let record_existed =
        history_record_exists(store, record_id).context("inspect source import history record")?;
    let selected_append_publication = match selection {
        Some(SelectedImportWork::Catalog(_)) => true,
        Some(SelectedImportWork::SourceFiles(_)) => {
            provider_file_mutation_contract(source.provider, source.source_format)
                == ProviderFileMutationContract::AppendOnlyNewlineDelimited
        }
        None => false,
    };
    if !record_existed && !selected_append_publication {
        store.upsert_record(&record)?;
    }
    if !source_uses_import_file_manifest(source)
        && provider_file_mutation_contract(source.provider, source.source_format)
            == ProviderFileMutationContract::AppendOnlyNewlineDelimited
    {
        if let Some(SelectedImportWork::SourceFiles(work)) = selection {
            if work.len() != 1 {
                return Err(anyhow::Error::new(CaptureError::SystemInvariant(
                    "single-file append source selected more than one inventory unit",
                )));
            }
            let import_result = import_append_capable_source_file_work(
                store,
                source,
                &work[0],
                preinventory.inventory_generation().ok_or_else(|| {
                    anyhow::Error::new(CaptureError::SystemInvariant(
                        "selected append source has no inventory generation",
                    ))
                })?,
                &record,
            )
            .context("run selected append publication");
            return match import_result {
                Ok(AppendImportOutcome::Imported(summary)) => {
                    let status = provider_summary_import_status(&summary);
                    let error = (summary.failed > 0).then(|| source_import_file_failure(&summary));
                    let post_import = persist_reobserved_source_root_result(
                        store,
                        source,
                        preinventory,
                        status,
                        error.as_deref().unwrap_or(""),
                    )?;
                    Ok(ProviderImportBatchOutcome {
                        summary,
                        completed_units: post_import
                            .as_ref()
                            .map_or(0, |observation| observation.persisted_current_outcomes),
                        completed_bytes: post_import.as_ref().map_or(0, |observation| {
                            if observation.persisted_current_outcomes > 0 {
                                work[0].estimated_bytes
                            } else {
                                0
                            }
                        }),
                        deferred_units: 0,
                        durable_progress: false,
                        stop_admission: false,
                        post_import_inventory_generation: post_import
                            .as_ref()
                            .map(|observation| observation.inventory_generation),
                        post_import_preinventory: post_import
                            .map(|observation| observation.preinventory),
                    })
                }
                Ok(AppendImportOutcome::Deferred { durable_progress }) => {
                    if !record_existed && !durable_progress {
                        store.delete_orphan_record(record_id)?;
                    }
                    let post_import = if durable_progress {
                        None
                    } else {
                        persist_reobserved_source_root_result(
                            store,
                            source,
                            preinventory,
                            CatalogIndexedStatus::Pending,
                            "",
                        )?
                    };
                    Ok(ProviderImportBatchOutcome {
                        summary: ProviderImportSummary::default(),
                        completed_units: 0,
                        completed_bytes: 0,
                        deferred_units: 1,
                        durable_progress,
                        stop_admission: false,
                        post_import_inventory_generation: post_import
                            .as_ref()
                            .map(|observation| observation.inventory_generation),
                        post_import_preinventory: post_import
                            .map(|observation| observation.preinventory),
                    })
                }
                Err(error) => {
                    if publication_recovery_required(&error) {
                        return Err(error);
                    }
                    let observation_result = persist_reobserved_source_root_result(
                        store,
                        source,
                        preinventory,
                        import_error_status(&error),
                        &error.to_string(),
                    );
                    if let Some(observation_error) =
                        final_observation_system_error(observation_result)
                    {
                        return Err(observation_error);
                    }
                    cleanup_rejected_history_record(store, record_id, record_existed)?;
                    Err(error)
                }
            };
        }
    }
    let mut completed_units = 0;
    let mut completed_bytes = 0_u64;
    let mut deferred_units = 0;
    let mut durable_progress = false;
    let mut post_import_inventory_generation = None;
    let mut post_import_preinventory = None;
    let summary = if source_uses_import_file_manifest(source)
        && (source.path.is_dir() || preinventory.source_import_files().is_some())
    {
        import_manifested_source(
            store,
            source,
            progress,
            ManifestedImportOptions::new(
                preinventory.source_import_files(),
                preinventory.inventory_generation(),
                full_rescan,
                selection,
            ),
        )
        .map(|outcome| {
            completed_units = outcome.completed_units;
            completed_bytes = outcome.completed_bytes;
            deferred_units = outcome.deferred_units;
            durable_progress = outcome.durable_progress;
            post_import_inventory_generation = outcome.post_import_inventory_generation;
            post_import_preinventory = outcome.post_import_preinventory.or_else(|| {
                (outcome.deferred_units == 0
                    && outcome.post_import_inventory_generation
                        == preinventory.inventory_generation())
                .then(|| preinventory.clone())
            });
            outcome.summary
        })
    } else {
        match source.provider {
            CaptureProvider::Codex => {
                if source.path.is_dir() {
                    import_incremental_codex_session_tree(
                        store,
                        source,
                        &record,
                        progress.clone(),
                        preinventory.codex_session_catalog(),
                        preinventory.inventory_generation(),
                        full_rescan,
                        selection,
                    )
                    .map(|outcome| {
                        completed_units = outcome.completed_units;
                        completed_bytes = outcome.completed_bytes;
                        deferred_units = outcome.deferred_units;
                        durable_progress = outcome.durable_progress;
                        post_import_inventory_generation = outcome.post_import_inventory_generation;
                        post_import_preinventory = outcome.post_import_preinventory;
                        outcome.summary
                    })
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
        Ok(mut summary) => {
            if deferred_units > 0 {
                return Ok(ProviderImportBatchOutcome {
                    summary,
                    completed_units,
                    completed_bytes,
                    deferred_units,
                    durable_progress,
                    stop_admission: false,
                    post_import_inventory_generation,
                    post_import_preinventory,
                });
            }
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
            if retained_existing_content {
                summary.mark_retained_existing_content();
            }
            if summary.failed > 0
                && !provider_summary_has_imported_content(&summary)
                && !retained_existing_content
            {
                persist_reobserved_source_root_result(
                    store,
                    source,
                    preinventory,
                    CatalogIndexedStatus::Rejected,
                    &format!("provider import reported {} failure(s)", summary.failed),
                )?;
                cleanup_rejected_history_record(store, record_id, record_existed)?;
                return Err(provider_import_summary_failure(source, &summary));
            }
            let status = provider_summary_import_status(&summary);
            let error = (summary.failed > 0).then(|| source_import_file_failure(&summary));
            if let Some(post_import) = persist_reobserved_source_root_result(
                store,
                source,
                preinventory,
                status,
                error.as_deref().unwrap_or(""),
            )? {
                completed_units = post_import.persisted_current_outcomes;
                post_import_inventory_generation = Some(post_import.inventory_generation);
                post_import_preinventory = Some(post_import.preinventory);
            }
            if !record_existed && summary == ProviderImportSummary::default() {
                store.delete_orphan_record(record_id)?;
            }
            summary
        }
        Err(err) => {
            if err.downcast_ref::<ProviderImportBatchError>().is_some() {
                return Err(err);
            }
            if publication_recovery_required(&err) {
                return Err(err);
            }
            let failure_scope = import_error_scope(&err);
            let status = import_error_status(&err);
            let observation_result = persist_reobserved_source_root_result(
                store,
                source,
                preinventory,
                status,
                &err.to_string(),
            );
            if let Some(observation_error) = final_observation_system_error(observation_result) {
                return Err(observation_error);
            }
            let deleted = store.delete_orphan_record(record_id).with_context(|| {
                format!("clean up history record after provider import failed: {err:#}")
            })?;
            if failure_scope == ImportFailureScope::Source
                && !deleted
                && !record_existed
                && !source_uses_import_file_manifest(source)
                && history_record_exists(store, record_id)?
            {
                return Err(anyhow::Error::new(CaptureError::SystemInvariant(
                    "failed source import left content attached to its history record",
                )));
            }
            return Err(err);
        }
    };
    Ok(ProviderImportBatchOutcome {
        summary,
        completed_units,
        completed_bytes,
        deferred_units,
        durable_progress,
        stop_admission: false,
        post_import_inventory_generation,
        post_import_preinventory,
    })
}

#[derive(Debug)]
struct PostImportInventoryObservation {
    inventory_generation: u64,
    persisted_current_outcomes: usize,
    preinventory: SourcePreinventory,
}

fn persist_reobserved_source_root_result(
    store: &Store,
    source: &SourceInfo,
    preinventory: &SourcePreinventory,
    status: CatalogIndexedStatus,
    error: &str,
) -> Result<Option<PostImportInventoryObservation>> {
    let Some((observed, _)) = preinventory.source_root_observation() else {
        return Ok(None);
    };
    ctx_history_capture::pace_current_filesystem_operation(source.path.as_os_str().len() as u64);
    let metadata = fs::symlink_metadata(&source.path)
        .with_context(|| format!("stat import source {}", source.path.display()))?;
    if metadata.file_type().is_dir() {
        return Err(anyhow::Error::new(CaptureError::InventorySuperseded)
            .context("directory source post-import observation requires bounded inventory"));
    }
    let (_, current) = observe_source_root(source)?;
    let outcomes = same_source_import_observation(observed, &current)
        .then_some(SourceImportObservationOutcome {
            file: &current,
            status,
            error: (!error.is_empty()).then_some(error),
        })
        .into_iter()
        .collect::<Vec<_>>();
    let persisted = persist_source_import_observation_with_outcomes(
        store,
        source,
        std::slice::from_ref(&current),
        &outcomes,
    )?;
    Ok(Some(PostImportInventoryObservation {
        inventory_generation: persisted.inventory_generation,
        persisted_current_outcomes: outcomes.len(),
        preinventory: SourcePreinventory::SourceRoot {
            file: current,
            inventory_generation: persisted.inventory_generation,
        },
    }))
}

fn final_observation_system_error<T>(observation_result: Result<T>) -> Option<anyhow::Error> {
    observation_result
        .err()
        .filter(|error| import_error_scope(error) == ImportFailureScope::System)
}

#[cfg(test)]
fn mark_source_import_file_result(
    store: &Store,
    file: &SourceImportFile,
    inventory_generation: u64,
    status: CatalogIndexedStatus,
    error: Option<&str>,
) -> Result<()> {
    store.record_source_import_file_result(
        file.provider,
        SourceImportFileIndexUpdate {
            source_root: &file.source_root,
            source_path: &file.source_path,
            file_size_bytes: file.file_size_bytes,
            file_modified_at_ms: file.file_modified_at_ms,
            import_revision: file.import_revision,
            inventory_generation,
            metadata: &file.metadata,
            indexed_at_ms: utc_now().timestamp_millis(),
        },
        status,
        error,
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
