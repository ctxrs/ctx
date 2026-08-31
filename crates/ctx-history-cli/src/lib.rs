//! Transport-neutral command contracts for local agent-history operations.
//!
//! Clap parsing, final analytics delivery, and product-specific host composition
//! remain outside this crate. This boundary owns only plain command values and
//! the ports future command bodies use to request configuration, terminal I/O,
//! and observations.

mod analytics;
mod cli;
mod config;
mod dispatch;
mod history_source_plugins;
mod import_application;
mod import_presentation;
mod import_report;
mod list_events;
mod local_usage;
mod output;
mod ports;
mod presentation_limit;
mod progress;
mod provider_args;
mod provider_sources;
mod request;
mod search_filters;
mod semantic;
mod source_index;
mod sources;
mod transcript;
mod ui;

// Publication-authority fixtures exercise the lower query contract.  They are
// deliberately test-only: final command composition must not depend on them.
#[cfg(test)]
mod test_query_authority;

pub use cli::{
    ContentScopeArg, LocateArgs, LocateEventArgs, LocateSessionArgs, LocateTarget, SearchArgs,
    SearchBackendArg, ShowArgs, ShowEventArgs, ShowSessionArgs, ShowTarget,
};
pub use ctx_history_capture::{ProviderRootDefinition, ProviderRootKind};
pub use ctx_history_core::parse_capture_provider_name;
pub use import_application::{run_import_application, ImportApplicationPort};
pub use import_report::{
    import_completion_error, import_error_scope, import_failure_type, import_report_failure_type,
    import_report_json, import_report_outcome, render_import_report_human, resume_mode_name,
    ImportFailureScope, ImportFailureType,
};
pub use list_events::{
    decode_cursor, event_query_error_value, event_range_page_value,
    mcp_event_query_core_record_bytes, render_event, run as run_list_events,
    selection as list_events_selection,
    selection_from_request as list_events_selection_from_request,
    validated_limit as validated_event_limit, EventContentProjection, EventContentProjectionArg,
    EventQueryDirection, EventQueryError, EventQueryFormat, EventQueryScope, EventQueryWireRequest,
    ListEventsArgs, DEFAULT_EVENT_QUERY_LIMIT,
};
pub use provider_args::{
    cli_supported_provider, compact_provider_error, mcp_provider_names, native_provider_cli_specs,
    parse_native_provider, parse_native_provider_name, parse_provider, parse_provider_name,
    provider_cli_name, provider_cli_spec, provider_cli_specs, provider_is_importable, ProviderArg,
    ProviderCliSpec,
};
pub use provider_sources::{
    discovered_plugin_sources_json, discovered_sources_for_provider_report,
    discovered_sources_for_provider_report_with_data_root,
    discovered_sources_for_provider_report_with_data_root_and_provider_roots,
    discovered_sources_report, discovered_sources_report_with_data_root,
    discovered_sources_report_with_data_root_and_provider_roots, discovery_report_issues_json,
    discovery_report_issues_json_with_provider_roots, enrich_sources_json_with_selection,
    filter_cli_supported_report, filter_cli_supported_sources, history_source_plugin_refresh_json,
    history_source_plugin_report, import_support_json, manual_path_guidance,
    plugin_manifest_failures_json, plugin_sources_json, provider_selection_guidance, sources_json,
    CliSourceDiscoveryPort, SourceInfo, DEFAULT_VISIBLE_SOURCE_PROVIDERS, MAX_DISCOVERY_ISSUES,
    MAX_DISCOVERY_ISSUE_MESSAGE_BYTES,
};
pub use source_index::{
    copied_lineage_summary, generation_query_authority_error_json, mcp_search_with_compact,
    mcp_show_event_application, mcp_show_session_application, normalize_mcp_search_request,
    run_locate, run_search, run_show, validate_explicit_semantic_scope, McpSearchError,
    McpSearchExecutionFailure, ShowApplicationError, SourceSearchRequest,
};
pub use sources::{run_sources, SourcesDiscoveryObservation, SourcesExecutionObservation};

pub use config::{HistoryCliConfig, HistoryConfigPort, HistoryConfigSnapshotPort};
pub use history_source_plugins::{
    discover_history_source_plugins_with_diagnostics, prepare_source_backed_history_source,
    HistorySourcePluginManifestFailure, HistorySourcePluginRefresh, HistorySourcePluginSource,
    PreparedHistorySourcePluginRefresh,
};
pub use output::JsonOutputFormat;
pub use ports::{
    OutputStream, SearchExecutionObservation, SearchFailurePhase, SearchRefreshStatus, TerminalPort,
};
pub use progress::{
    format_bytes, format_count, presentation_snapshot, provider_display_name, ProgressReporter,
    ProgressWriterError,
};
pub use request::{
    HistoryProvider, ImportFormat, ImportRequest, ListEventsContentProjection, ListEventsDirection,
    ListEventsRequest, ListEventsScope, ListRequest, LocateRequest, OutputFormat, ProgressMode,
    RefreshMode, SearchBackend, SearchContentScope, SearchRequest, SetupRequest, ShowRequest,
    SourceIndexRequest, SourcesRequest, TranscriptMode,
};
pub use search_filters::parse_since_filter;
pub use transcript::{shell_quote_arg, write_output, TranscriptOutput};

/// Marks a failure whose command-specific output has already been written.
/// The final `ctx` dispatch maps this marker to its normal failure exit once,
/// without rendering a second diagnostic.
#[derive(Debug, thiserror::Error)]
#[error("CLI error was already rendered")]
pub struct RenderedCliError;
