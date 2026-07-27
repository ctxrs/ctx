use std::path::Path;

use anyhow::{anyhow, Result};
use uuid::Uuid;

use ctx_history_capture::{
    import_antigravity_cli_history, import_astrbot_sqlite, import_auggie_history,
    import_claude_projects_jsonl_tree, import_cline_task_json_history, import_codebuddy_history,
    import_codex_history_jsonl, import_codex_session_jsonl, import_codex_session_tree,
    import_continue_cli_sessions, import_copilot_cli_session_events, import_crush_sqlite,
    import_cursor_native_history, import_deepagents_sqlite, import_factory_ai_droid_sessions,
    import_firebender_sqlite, import_forgecode_sqlite, import_gemini_cli_history,
    import_goose_sessions_sqlite, import_hermes_sqlite, import_junie_history, import_kilo_sqlite,
    import_kimi_code_cli_history, import_kiro_sqlite, import_lingma_sqlite, import_mimocode_sqlite,
    import_mistral_vibe_history, import_mux_history, import_nanoclaw_project,
    import_openclaw_history, import_opencode_sqlite, import_openhands_file_events,
    import_pi_session_jsonl, import_qoder_history, import_qwen_code_history,
    import_roo_task_json_history, import_rovodev_history, import_shelley_sqlite,
    import_tabnine_cli_history, import_trae_history, import_warp_sqlite,
    import_windsurf_cascade_hook_transcripts, import_zed_threads_sqlite,
    AntigravityCliImportOptions, AstrBotSqliteImportOptions, AuggieImportOptions, CaptureWorkLimit,
    ClaudeProjectsImportOptions, ClineTaskJsonImportOptions, CodeBuddyImportOptions,
    CodexHistoryImportOptions, CodexSessionImportOptions, CodexSessionImportProgressCallback,
    ContinueCliImportOptions, CopilotCliImportOptions, CrushSqliteImportOptions,
    CursorNativeImportOptions, DeepAgentsSqliteImportOptions, FactoryAiDroidImportOptions,
    FirebenderSqliteImportOptions, ForgeCodeSqliteImportOptions, GeminiCliImportOptions,
    GooseSessionsSqliteImportOptions, HermesSqliteImportOptions, ImportProfile, JunieImportOptions,
    KiloSqliteImportOptions, KimiCodeCliImportOptions, KiroSqliteImportOptions,
    LingmaSqliteImportOptions, MiMoCodeSqliteImportOptions, MistralVibeImportOptions,
    MuxImportOptions, NanoClawImportOptions, OpenClawImportOptions, OpenCodeSqliteImportOptions,
    OpenHandsImportOptions, PiSessionImportOptions, ProviderImportSummary, QoderImportOptions,
    QwenCodeImportOptions, RooTaskJsonImportOptions, RovoDevImportOptions,
    ShelleySqliteImportOptions, TabnineCliImportOptions, TraeImportOptions,
    WarpSqliteImportOptions, WindsurfCascadeHookImportOptions, ZedThreadsSqliteImportOptions,
};
use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;

use crate::commands::import::SourcePreinventory;
use crate::provider_sources::SourceInfo;

#[allow(clippy::too_many_arguments)]
pub(super) fn import_direct_source(
    store: &mut Store,
    source: &SourceInfo,
    input_path: &Path,
    record_id: Uuid,
    progress: Option<CodexSessionImportProgressCallback>,
    full_rescan: bool,
    preinventory: &SourcePreinventory,
    capture_work_limit: CaptureWorkLimit,
    inventory_observation_token: Option<String>,
    import_profile: &ImportProfile,
) -> Result<ProviderImportSummary> {
    match source.provider {
        CaptureProvider::Codex => {
            if input_path.is_dir() {
                let _ = (full_rescan, preinventory);
                import_codex_session_tree(
                    input_path,
                    store,
                    CodexSessionImportOptions {
                        source_path: Some(source.path.clone()),
                        history_record_id: Some(record_id),
                        capture_work_limit,
                        inventory_observation_token: inventory_observation_token.clone(),
                        import_profile: import_profile.clone(),
                        progress: progress.clone(),
                        ..CodexSessionImportOptions::default()
                    },
                )
                .map_err(anyhow::Error::from)
            } else if input_path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == "history.jsonl")
            {
                import_codex_history_jsonl(
                    input_path,
                    store,
                    CodexHistoryImportOptions {
                        source_path: Some(source.path.clone()),
                        history_record_id: Some(record_id),
                        capture_work_limit,
                        inventory_observation_token: inventory_observation_token.clone(),
                        import_profile: import_profile.clone(),
                        ..CodexHistoryImportOptions::default()
                    },
                )
                .map_err(anyhow::Error::from)
            } else {
                import_codex_session_jsonl(
                    input_path,
                    store,
                    CodexSessionImportOptions {
                        source_path: Some(source.path.clone()),
                        history_record_id: Some(record_id),
                        capture_work_limit,
                        inventory_observation_token: inventory_observation_token.clone(),
                        import_profile: import_profile.clone(),
                        progress,
                        ..CodexSessionImportOptions::default()
                    },
                )
                .map_err(anyhow::Error::from)
            }
        }
        CaptureProvider::Pi => import_pi_session_jsonl(
            input_path,
            store,
            PiSessionImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..PiSessionImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Claude => import_claude_projects_jsonl_tree(
            input_path,
            store,
            ClaudeProjectsImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..ClaudeProjectsImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Cline => import_cline_task_json_history(
            input_path,
            store,
            ClineTaskJsonImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..ClineTaskJsonImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::RooCode => import_roo_task_json_history(
            input_path,
            store,
            RooTaskJsonImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..RooTaskJsonImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::CodeBuddy => import_codebuddy_history(
            input_path,
            store,
            CodeBuddyImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..CodeBuddyImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Trae => import_trae_history(
            input_path,
            store,
            TraeImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..TraeImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::OpenCode => import_opencode_sqlite(
            input_path,
            store,
            OpenCodeSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..OpenCodeSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Kilo => import_kilo_sqlite(
            input_path,
            store,
            KiloSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..KiloSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::MiMoCode => import_mimocode_sqlite(
            input_path,
            store,
            MiMoCodeSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..MiMoCodeSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::KiroCli => import_kiro_sqlite(
            input_path,
            store,
            KiroSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..KiroSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::ForgeCode => import_forgecode_sqlite(
            input_path,
            store,
            ForgeCodeSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..ForgeCodeSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::DeepAgents => import_deepagents_sqlite(
            input_path,
            store,
            DeepAgentsSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..DeepAgentsSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Crush => import_crush_sqlite(
            input_path,
            store,
            CrushSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..CrushSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Goose => import_goose_sessions_sqlite(
            input_path,
            store,
            GooseSessionsSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..GooseSessionsSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::OpenClaw => import_openclaw_history(
            input_path,
            store,
            OpenClawImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..OpenClawImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Hermes => import_hermes_sqlite(
            input_path,
            store,
            HermesSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..HermesSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::NanoClaw => import_nanoclaw_project(
            input_path,
            store,
            NanoClawImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..NanoClawImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::AstrBot => import_astrbot_sqlite(
            input_path,
            store,
            AstrBotSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..AstrBotSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Shelley => import_shelley_sqlite(
            input_path,
            store,
            ShelleySqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..ShelleySqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Continue => import_continue_cli_sessions(
            input_path,
            store,
            ContinueCliImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..ContinueCliImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::OpenHands => import_openhands_file_events(
            input_path,
            store,
            OpenHandsImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..OpenHandsImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Lingma => import_lingma_sqlite(
            input_path,
            store,
            LingmaSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..LingmaSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Qoder => import_qoder_history(
            input_path,
            store,
            QoderImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..QoderImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Warp => import_warp_sqlite(
            input_path,
            store,
            WarpSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..WarpSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Gemini => import_gemini_cli_history(
            input_path,
            store,
            GeminiCliImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..GeminiCliImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Tabnine => import_tabnine_cli_history(
            input_path,
            store,
            TabnineCliImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..TabnineCliImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Cursor => import_cursor_native_history(
            input_path,
            store,
            CursorNativeImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..CursorNativeImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Windsurf => import_windsurf_cascade_hook_transcripts(
            input_path,
            store,
            WindsurfCascadeHookImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..WindsurfCascadeHookImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Zed => import_zed_threads_sqlite(
            input_path,
            store,
            ZedThreadsSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..ZedThreadsSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::CopilotCli => import_copilot_cli_session_events(
            input_path,
            store,
            CopilotCliImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..CopilotCliImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::FactoryAiDroid => import_factory_ai_droid_sessions(
            input_path,
            store,
            FactoryAiDroidImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..FactoryAiDroidImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::QwenCode => import_qwen_code_history(
            input_path,
            store,
            QwenCodeImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..QwenCodeImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::KimiCodeCli => import_kimi_code_cli_history(
            input_path,
            store,
            KimiCodeCliImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..KimiCodeCliImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Auggie => import_auggie_history(
            input_path,
            store,
            AuggieImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..AuggieImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Junie => import_junie_history(
            input_path,
            store,
            JunieImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..JunieImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Firebender => import_firebender_sqlite(
            input_path,
            store,
            FirebenderSqliteImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..FirebenderSqliteImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::RovoDev => import_rovodev_history(
            input_path,
            store,
            RovoDevImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..RovoDevImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::MistralVibe => import_mistral_vibe_history(
            input_path,
            store,
            MistralVibeImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..MistralVibeImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Mux => import_mux_history(
            input_path,
            store,
            MuxImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..MuxImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        CaptureProvider::Antigravity => import_antigravity_cli_history(
            input_path,
            store,
            AntigravityCliImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                capture_work_limit,
                inventory_observation_token: inventory_observation_token.clone(),
                import_profile: import_profile.clone(),
                ..AntigravityCliImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
        other => Err(anyhow!(
            "{} is not registered for provider history import",
            other.as_str()
        )),
    }
}
