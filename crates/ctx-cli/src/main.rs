#![allow(unused_imports)]
use std::{
    env, fs,
    io::{Cursor, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
    thread,
    time::{Duration as StdDuration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context, Result};

use chrono::{Duration, Utc};

use clap::{Args, Parser, Subcommand, ValueEnum};

use serde_json::{json, Number, Value};

use sha2::{Digest, Sha256};

use uuid::Uuid;

mod analytics;

mod config;

mod docs;

mod history_source_plugins;

mod identity;

mod install_marker;

mod mcp;

mod net;

mod skill;

mod upgrade;

#[cfg(test)]
mod parser_prop_tests;

use analytics::{AnalyticsEvent, AnalyticsProperties};

use config::{AppConfig, CONFIG_FILE};

use ctx_history_capture::{
    catalog_codex_session_tree, discover_provider_sources, discover_provider_sources_for_provider,
    import_antigravity_cli_history, import_astrbot_sqlite, import_auggie_history,
    import_claude_projects_jsonl_tree, import_cline_task_json_history, import_codebuddy_history,
    import_codex_history_jsonl, import_codex_session_jsonl, import_codex_session_jsonl_tail,
    import_codex_session_paths, import_codex_session_tree, import_continue_cli_sessions,
    import_copilot_cli_session_events, import_crush_sqlite, import_cursor_native_history,
    import_custom_history_jsonl_v1, import_custom_history_jsonl_v1_reader,
    import_deepagents_sqlite, import_factory_ai_droid_sessions, import_firebender_sqlite,
    import_forgecode_sqlite, import_gemini_cli_history, import_goose_sessions_sqlite,
    import_hermes_sqlite, import_junie_history, import_kilo_sqlite, import_kimi_code_cli_history,
    import_kiro_sqlite, import_lingma_sqlite, import_mistral_vibe_history, import_mux_history,
    import_nanoclaw_project, import_openclaw_history, import_opencode_sqlite,
    import_openhands_file_events, import_pi_session_jsonl, import_qoder_history,
    import_qwen_code_history, import_roo_task_json_history, import_rovodev_history,
    import_shelley_sqlite, import_tabnine_cli_history, import_trae_history, import_warp_sqlite,
    import_windsurf_cascade_hook_transcripts, import_zed_threads_sqlite, provider_source_for_path,
    provider_source_spec, stable_capture_uuid, validate_custom_history_jsonl_v1,
    validate_custom_history_jsonl_v1_reader, AntigravityCliImportOptions,
    AstrBotSqliteImportOptions, AuggieImportOptions, CatalogSummary, ClaudeProjectsImportOptions,
    ClineTaskJsonImportOptions, CodeBuddyImportOptions, CodexEventImportMode,
    CodexHistoryImportOptions, CodexSessionCatalogOptions, CodexSessionImportOptions,
    CodexSessionImportProgress, CodexSessionImportProgressCallback, CodexToolOutputMode,
    ContinueCliImportOptions, CopilotCliImportOptions, CrushSqliteImportOptions,
    CursorNativeImportOptions, CustomHistoryJsonlV1ImportOptions, DeepAgentsSqliteImportOptions,
    FactoryAiDroidImportOptions, FirebenderSqliteImportOptions, ForgeCodeSqliteImportOptions,
    GeminiCliImportOptions, GooseSessionsSqliteImportOptions, HermesSqliteImportOptions,
    JunieImportOptions, KiloSqliteImportOptions, KimiCodeCliImportOptions, KiroSqliteImportOptions,
    LingmaSqliteImportOptions, MistralVibeImportOptions, MuxImportOptions, NanoClawImportOptions,
    OpenClawImportOptions, OpenCodeSqliteImportOptions, OpenHandsImportOptions,
    PiSessionImportOptions, ProviderImportSummary, ProviderImportSupport, ProviderSource,
    ProviderSourceStatus, QoderImportOptions, QwenCodeImportOptions, RooTaskJsonImportOptions,
    RovoDevImportOptions, ShelleySqliteImportOptions, TabnineCliImportOptions, TraeImportOptions,
    WarpSqliteImportOptions, WindsurfCascadeHookImportOptions, ZedThreadsSqliteImportOptions,
};

use ctx_history_core::{
    database_path, default_data_root, utc_now, CaptureProvider, ContextCitation,
    ContextCitationType, CtxHistoryJsonlRecord, Event, EventRole, EventType, HistoryRecord,
    ProviderRawRetention, RedactionState, Session,
};

use ctx_history_store::{
    CatalogSession, CatalogSourceIndexUpdate, RawSqlOptions, RawSqlResult, RawSqlValue,
    SourceImportFile, SourceImportFileIndexUpdate, Store, StoreError, RAW_SQL_DEFAULT_MAX_COLUMNS,
    RAW_SQL_DEFAULT_MAX_ROWS, RAW_SQL_DEFAULT_MAX_SQL_BYTES, RAW_SQL_DEFAULT_MAX_VALUE_BYTES,
    RAW_SQL_MAX_SQL_BYTES_CAP, RAW_SQL_MAX_TIMEOUT,
};

use history_source_plugins::{
    discover_history_source_plugins, discover_history_source_plugins_with_diagnostics,
    run_history_source_plugin, HistorySourcePluginManifestFailure, HistorySourcePluginRefresh,
    HistorySourcePluginRunOptions, HistorySourcePluginSource,
};

fn main() -> Result<()> {
    let started = Instant::now();
    let cli = Cli::parse();
    let action = cli.command.name();
    let sends_analytics = cli.command.sends_analytics();
    let json_output = cli.command.json_output();
    let allow_background_upgrade = cli.command.allows_background_upgrade();
    let mut analytics_properties = command_analytics_properties(&cli.command);
    let data_root = cli
        .data_root
        .clone()
        .map(Ok)
        .unwrap_or_else(default_data_root)
        .context("resolve ctx data root")?;
    let config = AppConfig::load(&data_root)?;
    if matches!(&cli.command, CommandRoot::Setup(_)) && sends_analytics {
        analytics::send_cli_event(
            &data_root,
            &config,
            AnalyticsEvent {
                action: "setup_started",
                json_output,
                success: true,
                duration: StdDuration::ZERO,
                properties: analytics_properties.clone(),
            },
        );
    }

    let result = match cli.command {
        CommandRoot::Setup(args) => run_setup(args, data_root.clone(), &mut analytics_properties),
        CommandRoot::Status(args) => run_status(args, data_root.clone(), &mut analytics_properties),
        CommandRoot::Sources(args) => {
            run_sources(args, data_root.clone(), &mut analytics_properties)
        }
        CommandRoot::Import(args) => run_import(args, data_root.clone(), &mut analytics_properties),
        CommandRoot::Show(args) => run_show(args, data_root.clone(), &mut analytics_properties),
        CommandRoot::Locate(args) => run_locate(args, data_root.clone(), &mut analytics_properties),
        CommandRoot::Search(args) => run_search(args, data_root.clone(), &mut analytics_properties),
        CommandRoot::Sql(args) => run_sql(args, data_root.clone()),
        CommandRoot::Docs(args) => docs::run(args),
        CommandRoot::Skill(args) => skill::run(args, &mut analytics_properties),
        CommandRoot::Mcp(args) => mcp::run(args, data_root.clone()),
        CommandRoot::Upgrade(args) => upgrade::run(args, data_root.clone(), config.clone()),
        CommandRoot::Doctor(args) => run_doctor(args, data_root.clone(), &mut analytics_properties),
    };
    if sends_analytics {
        analytics::send_cli_event(
            &data_root,
            &config,
            AnalyticsEvent {
                action,
                json_output,
                success: result.is_ok(),
                duration: started.elapsed(),
                properties: analytics_properties,
            },
        );
    }
    if result.is_ok() && allow_background_upgrade {
        upgrade::maybe_spawn_auto_upgrade(&data_root, &config, json_output);
    }
    result
}

#[path = "main/wal.rs"]
mod cli_main_wal;
pub(crate) use cli_main_wal::*;

#[path = "main/import_01.rs"]
mod cli_main_import_01;
pub(crate) use cli_main_import_01::*;

#[path = "main/import_02.rs"]
mod cli_main_import_02;
pub(crate) use cli_main_import_02::*;

#[path = "main/import_03.rs"]
mod cli_main_import_03;
pub(crate) use cli_main_import_03::*;

#[path = "main/search_01.rs"]
mod cli_main_search_01;
pub(crate) use cli_main_search_01::*;

#[path = "main/search_02.rs"]
mod cli_main_search_02;
pub(crate) use cli_main_search_02::*;

#[path = "main/event.rs"]
mod cli_main_event;
pub(crate) use cli_main_event::*;

#[path = "main/history_source_plugin.rs"]
mod cli_main_history_source_plugin;
pub(crate) use cli_main_history_source_plugin::*;

#[path = "main/claude.rs"]
mod cli_main_claude;
pub(crate) use cli_main_claude::*;

#[path = "main/setup.rs"]
mod cli_main_setup;
pub(crate) use cli_main_setup::*;

#[path = "main/catalog.rs"]
mod cli_main_catalog;
pub(crate) use cli_main_catalog::*;

#[path = "main/json.rs"]
mod cli_main_json;
pub(crate) use cli_main_json::*;

#[path = "main/doctor.rs"]
mod cli_main_doctor;
pub(crate) use cli_main_doctor::*;

#[path = "main/show.rs"]
mod cli_main_show;
pub(crate) use cli_main_show::*;

#[path = "main/session.rs"]
mod cli_main_session;
pub(crate) use cli_main_session::*;

#[path = "main/locate.rs"]
mod cli_main_locate;
pub(crate) use cli_main_locate::*;

#[path = "main/provider_source.rs"]
mod cli_main_provider_source;
pub(crate) use cli_main_provider_source::*;

#[path = "main/sql.rs"]
mod cli_main_sql;
pub(crate) use cli_main_sql::*;

#[path = "main/source_identity.rs"]
mod cli_main_source_identity;
pub(crate) use cli_main_source_identity::*;

#[path = "main/transcript.rs"]
mod cli_main_transcript;
pub(crate) use cli_main_transcript::*;

#[path = "main/refresh.rs"]
mod cli_main_refresh;
pub(crate) use cli_main_refresh::*;

#[path = "main/custom_history.rs"]
mod cli_main_custom_history;
pub(crate) use cli_main_custom_history::*;

#[path = "main/mcp.rs"]
mod cli_main_mcp;
pub(crate) use cli_main_mcp::*;

#[path = "main/resume.rs"]
mod cli_main_resume;
pub(crate) use cli_main_resume::*;

#[path = "main/source_stats.rs"]
mod cli_main_source_stats;
pub(crate) use cli_main_source_stats::*;

#[path = "main/source_progress.rs"]
mod cli_main_source_progress;
pub(crate) use cli_main_source_progress::*;

#[path = "main/progress.rs"]
mod cli_main_progress;
pub(crate) use cli_main_progress::*;

#[path = "main/aggregate.rs"]
mod cli_main_aggregate;
pub(crate) use cli_main_aggregate::*;

#[path = "main/render.rs"]
mod cli_main_render;
pub(crate) use cli_main_render::*;

#[path = "main/eta.rs"]
mod cli_main_eta;
pub(crate) use cli_main_eta::*;

#[path = "main/format_seconds.rs"]
mod cli_main_format_seconds;
pub(crate) use cli_main_format_seconds::*;

#[path = "main/format_bytes.rs"]
mod cli_main_format_bytes;
pub(crate) use cli_main_format_bytes::*;

#[path = "main/store.rs"]
mod cli_main_store;
pub(crate) use cli_main_store::*;

#[path = "main/analytics.rs"]
mod cli_main_analytics;
pub(crate) use cli_main_analytics::*;

#[path = "main/status.rs"]
mod cli_main_status;
pub(crate) use cli_main_status::*;

#[path = "main/provider.rs"]
mod cli_main_provider;
pub(crate) use cli_main_provider::*;

#[path = "main/path.rs"]
mod cli_main_path;
pub(crate) use cli_main_path::*;

#[path = "main/codex_01.rs"]
mod cli_main_codex_01;
pub(crate) use cli_main_codex_01::*;

#[path = "main/codex_02.rs"]
mod cli_main_codex_02;
pub(crate) use cli_main_codex_02::*;

#[path = "main/codex_03.rs"]
mod cli_main_codex_03;
pub(crate) use cli_main_codex_03::*;

#[path = "main/error.rs"]
mod cli_main_error;
pub(crate) use cli_main_error::*;

#[path = "main/sqlite.rs"]
mod cli_main_sqlite;
pub(crate) use cli_main_sqlite::*;

#[path = "main/normalize_uuid.rs"]
mod cli_main_normalize_uuid;
pub(crate) use cli_main_normalize_uuid::*;

#[path = "main/cursor.rs"]
mod cli_main_cursor;
pub(crate) use cli_main_cursor::*;

#[path = "main/shell.rs"]
mod cli_main_shell;
pub(crate) use cli_main_shell::*;

#[path = "main/record.rs"]
mod cli_main_record;
pub(crate) use cli_main_record::*;

#[path = "main/raw_sql.rs"]
mod cli_main_raw_sql;
pub(crate) use cli_main_raw_sql::*;

#[path = "main/truncate.rs"]
mod cli_main_truncate;
pub(crate) use cli_main_truncate::*;

#[path = "main/pad.rs"]
mod cli_main_pad;
pub(crate) use cli_main_pad::*;

#[path = "main/csv.rs"]
mod cli_main_csv;
pub(crate) use cli_main_csv::*;

#[path = "main/short.rs"]
mod cli_main_short;
pub(crate) use cli_main_short::*;

#[path = "main/source_import.rs"]
mod cli_main_source_import;
pub(crate) use cli_main_source_import::*;

#[path = "main/time.rs"]
mod cli_main_time;
pub(crate) use cli_main_time::*;

#[path = "main/mark.rs"]
mod cli_main_mark;
pub(crate) use cli_main_mark::*;

#[path = "main/identity.rs"]
mod cli_main_identity;
pub(crate) use cli_main_identity::*;

#[cfg(test)]
#[path = "main_tests/tests.rs"]
mod tests;
