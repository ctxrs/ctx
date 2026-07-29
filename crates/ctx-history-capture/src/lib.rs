pub mod provider_sources;
pub use common::io::{
    inventory_provider_jsonl_paths, inventory_provider_regular_paths, provider_regular_file_len,
    ProviderJsonlInventory, ProviderJsonlInventoryLimits, PROVIDER_JSONL_INVENTORY_MAX_DEPTH,
    PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES, PROVIDER_JSONL_INVENTORY_MAX_ELIGIBLE_PATHS,
    PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES, PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES,
};
pub use provider_sources::{
    discover_lingma_inventory_with_authority, discover_provider_sources,
    discover_provider_sources_for_provider, discover_provider_sources_for_provider_report,
    discover_provider_sources_for_provider_with_context,
    discover_provider_sources_for_provider_with_projects, discover_provider_sources_report,
    discover_provider_sources_with_context, discover_provider_sources_with_projects,
    discover_warp_sources_with_authority, observe_ordinary_file, provider_source_for_path,
    provider_source_spec, provider_source_specs, resolve_lingma_discovery_authority,
    resolve_warp_discovery_authority, DiscoveredLingmaDatabase, DiscoveredWarpSource,
    DiscoveryContext, DiscoveryIssue, DiscoveryIssueKind, DiscoveryPlatform, DiscoveryPlatformDirs,
    DiscoveryReport, LingmaDatabaseCatalogLineage, LingmaDiscoveredInventory,
    LingmaDiscoveryUnavailable, LingmaInventorySelector, LingmaVscodeClient, LingmaVscodeProfile,
    OrdinaryFileObservation, ProviderCatalogSupport, ProviderDefaultLocation,
    ProviderImportSupport, ProviderSource, ProviderSourceKind, ProviderSourceSpec,
    ProviderSourceStatus, WarpDiscoveryUnavailable, WarpInstalledPlatform, WarpInstalledSurfaceKey,
    WarpReleaseChannel, WarpTerminalSurface, DISCOVERY_ENV_ALLOWLIST,
};

pub(crate) const MAX_PROVIDER_JSONL_LINE_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_PROVIDER_SQLITE_VALUE_BYTES: usize = MAX_PROVIDER_JSONL_LINE_BYTES;
pub(crate) const MAX_OPENCLAW_SESSION_INDEX_BYTES: usize = 1024 * 1024;
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
pub(crate) const MIMOCODE_SQLITE_SOURCE_FORMAT: &str = "mimocode_sqlite";
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
pub(crate) const CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT: &str = "cursor_agent_transcript_jsonl_tree";
/// v0.25 labeled each Cursor transcript leaf instead of its authoritative
/// provider-owned tree. Read this only while migrating persisted routes.
pub(crate) const LEGACY_CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT: &str =
    "cursor_agent_transcript_jsonl";
pub(crate) const WINDSURF_CASCADE_HOOK_TRANSCRIPT_SOURCE_FORMAT: &str =
    "windsurf_cascade_hook_transcript_jsonl";
pub(crate) const QODER_SOURCE_FORMAT: &str = "qoder_transcript_jsonl";
pub(crate) const ZED_THREADS_SQLITE_SOURCE_FORMAT: &str = "zed_threads_sqlite";
pub(crate) const FACTORY_DROID_SOURCE_FORMAT: &str = "factory_ai_droid_sessions_jsonl";
pub(crate) const COPILOT_CLI_SOURCE_FORMAT: &str = "copilot_cli_session_events_jsonl";
pub(crate) const QWEN_CODE_SOURCE_FORMAT: &str = "qwen_code_chat_jsonl";
pub(crate) const KIMI_CODE_CLI_SOURCE_FORMAT: &str = "kimi_code_cli_wire_jsonl";
pub(crate) const ROVODEV_SOURCE_FORMAT: &str = "rovodev_session_json_tree";
pub(crate) const FORGECODE_SQLITE_SOURCE_FORMAT: &str = "forgecode_sqlite";
pub(crate) const DEEPAGENTS_SQLITE_SOURCE_FORMAT: &str = "deepagents_sessions_sqlite";
pub(crate) const MISTRAL_VIBE_SOURCE_FORMAT: &str = "mistral_vibe_session_jsonl";
pub(crate) const MUX_SOURCE_FORMAT: &str = "mux_session_jsonl";
pub(crate) const PROVIDER_MAX_TEXT_CHARS: usize = 16_000;
pub(crate) const PROVIDER_MAX_PREVIEW_CHARS: usize = 4_000;

pub mod complete_content;

pub(crate) mod native_source;
mod pro_output;
pub(crate) mod released_jsonl_cursor;
pub use pro_output::{
    ImportProfile, OutputAssociations, OutputCommandContext, OutputNativeCoordinate,
    OutputNativeCursor, OutputObservationKind, OutputOutcome, OutputOutcomeMetadata,
    OutputRepositoryContext, OutputSourceIdentity, OutputSourceLocator,
    ProOutputMaterializationPage, ProOutputObservation, ProOutputPageResult, ProOutputProgress,
    ProOutputSink, ProOutputSinkError, ProOutputSourceDisposition,
};

mod error;
pub use error::{CaptureError, ProviderJsonlInventoryLimit, ProviderSourceFailureKind, Result};

mod summaries;
pub use summaries::{
    CatalogSummary, ProviderImportFailure, ProviderImportSummary, ProviderImportTerminalOutcome,
    ProviderImportWorkResult,
};

mod options;
pub use options::{
    AntigravityCliImportOptions, AuggieImportOptions, ClaudeProjectsImportOptions,
    ClineTaskJsonImportOptions, CodeBuddyImportOptions, ContinueCliImportOptions,
    CopilotCliImportOptions, CrushSqliteImportOptions, CursorNativeImportOptions,
    CustomHistoryJsonlV1ImportOptions, DeepAgentsSqliteImportOptions, FactoryAiDroidImportOptions,
    FirebenderSqliteImportOptions, ForgeCodeSqliteImportOptions, GeminiCliImportOptions,
    GooseSessionsSqliteImportOptions, HermesSqliteImportOptions, JunieImportOptions,
    KiloSqliteImportOptions, KimiCodeCliImportOptions, KiroSqliteImportOptions,
    LingmaSqliteImportOptions, MiMoCodeSqliteImportOptions, MistralVibeImportOptions,
    MuxImportOptions, NanoClawImportOptions, OpenClawImportOptions, OpenCodeSqliteImportOptions,
    OpenHandsImportOptions, PiSessionImportOptions, QoderImportOptions, QwenCodeImportOptions,
    RooTaskJsonImportOptions, RovoDevImportOptions, ShelleySqliteImportOptions,
    TabnineCliImportOptions, TraeImportOptions, WarpSqliteImportOptions,
    WindsurfCascadeHookImportOptions, ZedThreadsSqliteImportOptions,
};

pub(crate) mod common {
    pub(crate) mod identity;
    pub(crate) mod io;
    pub(crate) mod json;
    pub(crate) mod time;
}
pub use common::identity::{compute_payload_hash, stable_capture_uuid};
pub(crate) use common::identity::{default_machine_id, fnv1a64};

#[cfg(test)]
mod test_support_paths;

pub(crate) mod provider;
pub use provider::adapter::{CaptureWorkLimit, ProviderAdapterContext, ProviderImportOptions};
pub use provider::codex::{
    hydrate_codex_locator, ingest_codex_source_backed_v0, CodexHydratedRecordV0,
    CodexLocatorResolverV0, CodexSourceBackedCountersV0, CodexSourceBackedErrorV0,
    CodexSourceBackedIngestReceiptV0, CodexSourceBackedPhaseTimingsV0, CodexSourceBackedResultV0,
};
pub use provider::custom_history_jsonl::{
    custom_history_jsonl_v1_cursor_stream, decode_custom_history_jsonl_v1_cursor,
};
pub use provider::source_backed::{
    build_automatic_source_backed_registry, refresh_source_backed_generation,
    refresh_source_backed_generation_with_progress, register_astrbot_source_backed_route,
    register_codex_prompt_history_source_backed_route, register_crush_source_backed_route,
    register_cursor_source_backed_route, register_custom_history_source_backed_route,
    register_forgecode_explicit_source_backed_route, register_gemini_source_backed_route,
    register_goose_source_backed_route, register_hermes_explicit_source_backed_route,
    register_landed_source_backed_route, register_lingma_source_backed_route,
    register_nanoclaw_source_backed_route, register_shelley_source_backed_route,
    register_warp_source_backed_route, source_backed_route_constructor,
    source_backed_route_inventory, CrushProjectDatabaseV0, CrushProjectInventoryObservationV0,
    CrushProjectInventorySourceV0, SourceBackedAutomaticRegistryBuild,
    SourceBackedAutomaticRegistryIssue, SourceBackedAutomaticUnavailableReason,
    SourceBackedCertifiedRemoval,
    SourceBackedCoordinatorError, SourceBackedCoordinatorResult, SourceBackedGenerationSink,
    SourceBackedHydrationSupport, SourceBackedProviderRegistry, SourceBackedProviderRouteMetadata,
    SourceBackedRefreshExecutor, SourceBackedRefreshProgress, SourceBackedRefreshReceipt,
    SourceBackedResolverRegistry, SourceBackedRevalidationTarget, SourceBackedRoute,
    SourceBackedRouteConstructor, SourceBackedRouteDriver, SourceBackedRouteError,
    SourceBackedRouteErrorKind, SourceBackedRouteMetadata, SourceBackedRouteResult,
    SourceBackedRouteSelection, SourceBackedSelectorAuthority, LANDED_SOURCE_BACKED_ROUTES,
};
