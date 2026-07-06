#[allow(unused_imports)]
use super::*;

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
