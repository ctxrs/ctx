pub mod provider_sources;
pub use provider_sources::{
    discover_provider_sources, discover_provider_sources_for_provider, provider_source_for_path,
    provider_source_spec, provider_source_specs, ProviderCatalogSupport, ProviderDefaultLocation,
    ProviderImportSupport, ProviderSource, ProviderSourceKind, ProviderSourceSpec,
    ProviderSourceStatus,
};

pub const CAPTURE_SCHEMA_VERSION: u32 = 1;
pub(crate) const MAX_PROVIDER_JSONL_LINE_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_PROVIDER_SQLITE_VALUE_BYTES: usize = MAX_PROVIDER_JSONL_LINE_BYTES;
pub(crate) const MAX_OPENCLAW_SESSION_INDEX_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_OPENCLAW_SESSION_INDEX_PATHS: usize = 256;
pub(crate) const MAX_OPENCLAW_SESSION_INDEX_VISITED_PATHS: usize = 4096;
pub(crate) const CODEX_SESSION_SOURCE_FORMAT: &str = "codex_session_jsonl";
pub(crate) const CLAUDE_PROJECTS_SOURCE_FORMAT: &str = "claude_projects_jsonl_tree";
pub(crate) const CLINE_TASK_JSON_SOURCE_FORMAT: &str = "cline_task_directory_json";
pub(crate) const ROO_TASK_JSON_SOURCE_FORMAT: &str = "roo_task_directory_json";
pub(crate) const CODEBUDDY_SOURCE_FORMAT: &str = "codebuddy_history_json";
pub(crate) const AUGGIE_SESSION_JSON_SOURCE_FORMAT: &str = "auggie_session_json";
pub(crate) const JUNIE_SESSION_EVENTS_SOURCE_FORMAT: &str = "junie_session_events_jsonl_tree";
pub(crate) const FIREBENDER_SQLITE_SOURCE_FORMAT: &str = "firebender_chat_history_sqlite";
pub(crate) const OPENCODE_SQLITE_SOURCE_FORMAT: &str = "opencode_sqlite";
pub(crate) const KILO_SQLITE_SOURCE_FORMAT: &str = "kilo_sqlite";
pub(crate) const KIRO_SQLITE_SOURCE_FORMAT: &str = "kiro_cli_sqlite";
pub(crate) const CRUSH_SQLITE_SOURCE_FORMAT: &str = "crush_sqlite";
pub(crate) const GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT: &str = "goose_sessions_sqlite";
pub(crate) const OPENCLAW_SOURCE_FORMAT: &str = "openclaw_session_jsonl_tree";
pub(crate) const HERMES_SQLITE_SOURCE_FORMAT: &str = "hermes_state_sqlite";
pub(crate) const NANOCLAW_SOURCE_FORMAT: &str = "nanoclaw_project";
pub(crate) const ASTRBOT_SQLITE_SOURCE_FORMAT: &str = "astrbot_data_v4_sqlite";
pub(crate) const SHELLEY_SQLITE_SOURCE_FORMAT: &str = "shelley_sqlite";
pub(crate) const CONTINUE_CLI_SOURCE_FORMAT: &str = "continue_cli_sessions_json";
pub(crate) const OPENHANDS_FILE_EVENTS_SOURCE_FORMAT: &str = "openhands_file_events";
pub(crate) const WARP_SQLITE_SOURCE_FORMAT: &str = "warp_sqlite";
pub(crate) const LINGMA_SQLITE_SOURCE_FORMAT: &str = "lingma_sqlite";
pub(crate) const ANTIGRAVITY_CLI_SOURCE_FORMAT: &str = "antigravity_cli_transcript_jsonl_tree";
pub(crate) const GEMINI_CLI_SOURCE_FORMAT: &str = "gemini_cli_chat_recording_jsonl";
pub(crate) const TABNINE_CLI_SOURCE_FORMAT: &str = "tabnine_cli_chat_recording_jsonl";
pub(crate) const CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT: &str = "cursor_agent_transcript_jsonl";
pub(crate) const WINDSURF_CASCADE_HOOK_TRANSCRIPT_SOURCE_FORMAT: &str =
    "windsurf_cascade_hook_transcript_jsonl";
pub(crate) const QODER_SOURCE_FORMAT: &str = "qoder_transcript_jsonl";
pub(crate) const ZED_THREADS_SQLITE_SOURCE_FORMAT: &str = "zed_threads_sqlite";
pub(crate) const FACTORY_DROID_SOURCE_FORMAT: &str = "factory_ai_droid_sessions_jsonl";
pub(crate) const COPILOT_CLI_SOURCE_FORMAT: &str = "copilot_cli_session_events_jsonl";
pub(crate) const QWEN_CODE_SOURCE_FORMAT: &str = "qwen_code_chat_jsonl";
pub(crate) const KIMI_CODE_CLI_SOURCE_FORMAT: &str = "kimi_code_cli_wire_jsonl";
pub(crate) const ROVODEV_SOURCE_FORMAT: &str = "rovodev_session_json";
pub(crate) const FORGECODE_SQLITE_SOURCE_FORMAT: &str = "forgecode_sqlite";
pub(crate) const DEEPAGENTS_SQLITE_SOURCE_FORMAT: &str = "deepagents_sessions_sqlite";
pub(crate) const MISTRAL_VIBE_SOURCE_FORMAT: &str = "mistral_vibe_session_jsonl";
pub(crate) const MUX_SOURCE_FORMAT: &str = "mux_session_jsonl";
pub(crate) const CODEX_MAX_TEXT_CHARS: usize = 16_000;
pub(crate) const CODEX_MAX_METADATA_TEXT_CHARS: usize = 4_000;
pub(crate) const CODEX_MAX_OUTPUT_PREVIEW_CHARS: usize = 4_000;
pub(crate) const PROVIDER_MAX_TEXT_CHARS: usize = 16_000;
pub(crate) const PROVIDER_MAX_PREVIEW_CHARS: usize = 4_000;
pub(crate) const CODEX_FAST_IMPORT_TRANSACTION_FILES: usize = 512;
pub(crate) const CODEX_FAST_IMPORT_PASSIVE_CHECKPOINT_MIN_BYTES: u64 = 2 * 1024 * 1024 * 1024;

mod error;
pub use error::{CaptureError, Result};

mod summaries;
pub use summaries::{
    CatalogSummary, ProviderImportFailure, ProviderImportSummary, SpoolCounts, SpoolImportFailure,
    SpoolImportSummary, SpoolRepairSummary,
};

mod options;
pub use options::{
    AntigravityCliImportOptions, AstrBotSqliteImportOptions, AuggieImportOptions,
    ClaudeProjectsImportOptions, ClineTaskJsonImportOptions, CodeBuddyImportOptions,
    CodexEventImportMode, CodexHistoryImportOptions, CodexSessionCatalogOptions,
    CodexSessionImportOptions, CodexSessionImportProgress, CodexSessionImportProgressCallback,
    CodexToolOutputMode, ContinueCliImportOptions, CopilotCliImportOptions,
    CrushSqliteImportOptions, CursorNativeImportOptions, CustomHistoryJsonlV1ImportOptions,
    DeepAgentsSqliteImportOptions, FactoryAiDroidImportOptions, FirebenderSqliteImportOptions,
    FixtureOptions, ForgeCodeSqliteImportOptions, GeminiCliImportOptions,
    GooseSessionsSqliteImportOptions, HermesSqliteImportOptions, JunieImportOptions,
    KiloSqliteImportOptions, KimiCodeCliImportOptions, KiroSqliteImportOptions,
    LingmaSqliteImportOptions, MistralVibeImportOptions, MuxImportOptions, NanoClawImportOptions,
    OpenClawImportOptions, OpenCodeSqliteImportOptions, OpenHandsImportOptions,
    PiSessionImportOptions, ProviderFixtureImportOptions, QoderImportOptions,
    QwenCodeImportOptions, RooTaskJsonImportOptions, RovoDevImportOptions,
    ShelleySqliteImportOptions, TabnineCliImportOptions, TraeImportOptions,
    WarpSqliteImportOptions, WindsurfCascadeHookImportOptions, ZedThreadsSqliteImportOptions,
};

pub(crate) mod common {
    pub(crate) mod identity;
    pub(crate) mod io;
    pub(crate) mod json;
    pub(crate) mod time;
}
pub use common::identity::{compute_payload_hash, stable_capture_uuid};
pub(crate) use common::identity::{default_machine_id, fnv1a64, sanitize_filename_component};

mod fixture;
pub use fixture::{fixture_envelope, write_fixture};

pub(crate) mod provider;
pub use provider::adapter::{
    AntigravityCliJsonlAdapter, AstrBotSqliteAdapter, AuggieSessionJsonAdapter,
    ClaudeProjectsJsonlAdapter, ClineTaskJsonAdapter, CodeBuddyHistoryJsonAdapter,
    CodexHistoryJsonlAdapter, CodexSessionJsonlAdapter, ContinueCliSessionsAdapter,
    CopilotCliSessionEventsAdapter, CrushSqliteAdapter, CursorAgentTranscriptJsonlAdapter,
    DeepAgentsSqliteAdapter, FactoryAiDroidJsonlAdapter, FirebenderSqliteAdapter,
    ForgeCodeSqliteAdapter, GeminiCliJsonlAdapter, GooseSessionsSqliteAdapter, HermesSqliteAdapter,
    JunieSessionEventsAdapter, KiloSqliteAdapter, KimiCodeCliWireJsonlAdapter, KiroSqliteAdapter,
    LingmaSqliteAdapter, MistralVibeJsonlAdapter, MuxJsonlAdapter, NanoClawProjectAdapter,
    NormalizedProviderImportOptions, OpenClawJsonlAdapter, OpenCodeSqliteAdapter,
    OpenHandsFileEventsAdapter, PiSessionJsonlAdapter, ProviderAdapterContext,
    ProviderCaptureAdapter, ProviderEventDto, ProviderFileTouchedEnvelope,
    ProviderFixtureJsonlAdapter, ProviderFixtureLine, ProviderNormalizationResult,
    ProviderSessionDto, QoderJsonlAdapter, QwenCodeJsonlAdapter, RooTaskJsonAdapter,
    RovoDevSessionJsonAdapter, ShelleySqliteAdapter, TabnineCliJsonlAdapter,
    WindsurfCascadeHookTranscriptJsonlAdapter, ZedThreadsSqliteAdapter,
};
pub use provider::api::{
    import_antigravity_cli_history, import_astrbot_sqlite, import_auggie_history,
    import_claude_projects_jsonl_tree, import_cline_task_json_history, import_codebuddy_history,
    import_continue_cli_sessions, import_copilot_cli_session_events, import_crush_sqlite,
    import_cursor_native_history, import_custom_history_jsonl_v1,
    import_custom_history_jsonl_v1_reader, import_deepagents_sqlite,
    import_factory_ai_droid_sessions, import_firebender_sqlite, import_forgecode_sqlite,
    import_gemini_cli_history, import_goose_sessions_sqlite, import_hermes_sqlite,
    import_junie_history, import_kilo_sqlite, import_kimi_code_cli_history, import_kiro_sqlite,
    import_lingma_sqlite, import_mistral_vibe_history, import_mux_history, import_nanoclaw_project,
    import_openclaw_history, import_opencode_sqlite, import_openhands_file_events,
    import_pi_session_jsonl, import_provider_fixture_jsonl, import_qoder_history,
    import_qwen_code_history, import_roo_task_json_history, import_rovodev_history,
    import_shelley_sqlite, import_tabnine_cli_history, import_trae_history, import_warp_sqlite,
    import_windsurf_cascade_hook_transcripts, import_zed_threads_sqlite,
    validate_custom_history_jsonl_v1, validate_custom_history_jsonl_v1_reader,
};
pub use provider::codex::{
    catalog_codex_session_tree, import_codex_history_jsonl, import_codex_session_jsonl,
    import_codex_session_jsonl_tail, import_codex_session_paths, import_codex_session_tree,
};
pub use provider::custom_history_jsonl::custom_history_jsonl_v1_cursor_stream;
pub use provider::importer::import_normalized_provider_captures;

mod spool;
pub use spool::{
    archive_from_envelopes, import_spool, inbox_dir, read_jsonl, retry_failed_spool_files,
    spool_counts, SpoolWriter,
};

#[cfg(test)]
mod tests;
