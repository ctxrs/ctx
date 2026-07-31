use ctx_history_core::CaptureProvider;

use crate::{
    output::{OutputFormat, SqlFormat},
    progress::ProgressArg,
    transcript::TranscriptMode,
};

use super::{BytesBucket, CountBucket, DurationBucket, ProgressMode, TextLengthBucket};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportSourceMode {
    ExplicitFormat,
    HistorySourcePlugin,
    ExplicitPath,
    AllDiscovered,
    DiscoveredProvider,
    AutoDiscovered,
}

impl ImportSourceMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ExplicitFormat => "explicit_format",
            Self::HistorySourcePlugin => "history_source_plugin",
            Self::ExplicitPath => "explicit_path",
            Self::AllDiscovered => "all_discovered",
            Self::DiscoveredProvider => "discovered_provider",
            Self::AutoDiscovered => "auto_discovered",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportOutcome {
    Success,
    Failure,
    CompletedWithRejections,
    CompletedWithSourceFailures,
    CompletedWithRejectionsAndSourceFailures,
}

impl ImportOutcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::CompletedWithRejections => "completed_with_rejections",
            Self::CompletedWithSourceFailures => "completed_with_source_failures",
            Self::CompletedWithRejectionsAndSourceFailures => {
                "completed_with_rejections_and_source_failures"
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportFailureScope {
    None,
    Record,
    Source,
    RecordAndSource,
    Invocation,
}

impl ImportFailureScope {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Record => "record",
            Self::Source => "source",
            Self::RecordAndSource => "record_and_source",
            Self::Invocation => "invocation",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportFailureType {
    None,
    RecordRejection,
    SourceFailure,
    RecordRejectionAndSourceFailure,
    InvalidRequest,
    Store,
    Io,
    Other,
}

impl ImportFailureType {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::RecordRejection => "record_rejection",
            Self::SourceFailure => "source_failure",
            Self::RecordRejectionAndSourceFailure => "record_rejection_and_source_failure",
            Self::InvalidRequest => "invalid_request",
            Self::Store => "store",
            Self::Io => "io",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct StoreTelemetry {
    pub(crate) indexed_sessions: Option<CountBucket>,
    pub(crate) indexed_events: Option<CountBucket>,
    pub(crate) indexed_items: Option<CountBucket>,
    pub(crate) db_size: Option<BytesBucket>,
}

#[derive(Debug)]
pub(crate) struct ImportTelemetry {
    pub(crate) resume: bool,
    pub(crate) all_sources: bool,
    pub(crate) no_daemon: bool,
    pub(crate) source_mode: ImportSourceMode,
    pub(crate) provider_filter: Option<CaptureProvider>,
    pub(crate) reset_cursor: bool,
    pub(crate) progress_mode: ProgressMode,
    pub(crate) sources_seen: Option<CountBucket>,
    pub(crate) source_bytes: Option<BytesBucket>,
    pub(crate) source_files: Option<CountBucket>,
    pub(crate) failed_sources: Option<CountBucket>,
    pub(crate) sessions_imported: Option<CountBucket>,
    pub(crate) events_imported: Option<CountBucket>,
    pub(crate) edges_imported: Option<CountBucket>,
    pub(crate) skipped: Option<CountBucket>,
    pub(crate) rejected_records: Option<CountBucket>,
    pub(crate) outcome: Option<ImportOutcome>,
    pub(crate) failure_scope: Option<ImportFailureScope>,
    pub(crate) failure_type: Option<ImportFailureType>,
}

impl ImportTelemetry {
    pub(crate) fn from_args(args: &crate::ImportArgs) -> Self {
        Self {
            resume: args.resume,
            all_sources: args.all,
            no_daemon: args.no_daemon,
            source_mode: if args.input_format.is_some() {
                ImportSourceMode::ExplicitFormat
            } else if args.history_source.is_some() || !args.history_source_manifest.is_empty() {
                ImportSourceMode::HistorySourcePlugin
            } else if args.path.is_some() {
                ImportSourceMode::ExplicitPath
            } else if args.all {
                ImportSourceMode::AllDiscovered
            } else if args.provider.is_some() {
                ImportSourceMode::DiscoveredProvider
            } else {
                ImportSourceMode::AutoDiscovered
            },
            provider_filter: args.provider.map(|provider| provider.capture_provider()),
            reset_cursor: args.reset_cursor,
            progress_mode: ProgressMode::from_arg(args.progress),
            sources_seen: None,
            source_bytes: None,
            source_files: None,
            failed_sources: None,
            sessions_imported: None,
            events_imported: None,
            edges_imported: None,
            skipped: None,
            rejected_records: None,
            outcome: None,
            failure_scope: None,
            failure_type: None,
        }
    }

    pub(crate) fn for_setup(progress: ProgressArg, no_daemon: bool) -> Self {
        let mut telemetry = Self::from_args(&crate::ImportArgs {
            provider: None,
            path: None,
            history_source: None,
            history_source_manifest: Vec::new(),
            reset_cursor: false,
            input_format: None,
            all: true,
            resume: false,
            partial: false,
            no_daemon,
            format: crate::output::JsonOutputFormat::Text,
            progress,
        });
        telemetry.source_mode = ImportSourceMode::AllDiscovered;
        telemetry
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SetupMode {
    Ready,
    Background,
}

impl SetupMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Background => "background",
        }
    }
}

#[derive(Debug)]
pub(crate) struct SetupTelemetry {
    pub(crate) catalog_only: bool,
    pub(crate) no_daemon: bool,
    pub(crate) wait: bool,
    pub(crate) progress_mode: ProgressMode,
    pub(crate) mode: Option<SetupMode>,
    pub(crate) providers_detected: Option<CountBucket>,
    pub(crate) cataloged_sessions: Option<CountBucket>,
    pub(crate) inventory_sources: Option<CountBucket>,
    pub(crate) inventory_source_files: Option<CountBucket>,
    pub(crate) pending_sessions: Option<CountBucket>,
    pub(crate) catalog_source_bytes: Option<BytesBucket>,
    pub(crate) inventory_source_bytes: Option<BytesBucket>,
    pub(crate) has_indexed_content: Option<bool>,
    pub(crate) store: StoreTelemetry,
    pub(crate) import: ImportTelemetry,
}

#[derive(Debug, Default)]
pub(crate) struct StatusTelemetry {
    pub(crate) initialized: Option<bool>,
    pub(crate) indexed_items: Option<CountBucket>,
    pub(crate) indexed_sessions: Option<CountBucket>,
    pub(crate) indexed_events: Option<CountBucket>,
    pub(crate) indexed_sources: Option<CountBucket>,
    pub(crate) inventory_units: Option<CountBucket>,
    pub(crate) pending_inventory_units: Option<CountBucket>,
    pub(crate) failed_inventory_units: Option<CountBucket>,
    pub(crate) stale_inventory_units: Option<CountBucket>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IndexOperation {
    Status,
    Watch,
    Wait,
}

impl IndexOperation {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Watch => "watch",
            Self::Wait => "wait",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IndexState {
    Ready,
    Empty,
    Pending,
    Missing,
    Disabled,
    Failed,
    Blocked,
    Unknown,
}

impl IndexState {
    pub(crate) fn from_safe_summary(value: &str) -> Self {
        match value {
            "ready" => Self::Ready,
            "empty" => Self::Empty,
            "pending" | "partial" | "running" | "stale" => Self::Pending,
            "missing" => Self::Missing,
            "disabled" => Self::Disabled,
            "failed" | "stale_lock" | "unavailable" => Self::Failed,
            "blocked" | "skipped" => Self::Blocked,
            _ => Self::Unknown,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Empty => "empty",
            Self::Pending => "pending",
            Self::Missing => "missing",
            Self::Disabled => "disabled",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaitOutcome {
    Ready,
    Blocked,
    Timeout,
}

impl WaitOutcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Blocked => "blocked",
            Self::Timeout => "timeout",
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct IndexTelemetry {
    pub(crate) operation: Option<IndexOperation>,
    pub(crate) wait_lexical: Option<bool>,
    pub(crate) wait_semantic: Option<bool>,
    pub(crate) wait_outcome: Option<WaitOutcome>,
    pub(crate) initialized: Option<bool>,
    pub(crate) lexical_state: Option<IndexState>,
    pub(crate) semantic_state: Option<IndexState>,
    pub(crate) indexed_items: Option<CountBucket>,
    pub(crate) inventory_units: Option<CountBucket>,
    pub(crate) pending_inventory_units: Option<CountBucket>,
    pub(crate) failed_inventory_units: Option<CountBucket>,
    pub(crate) stale_inventory_units: Option<CountBucket>,
}

#[derive(Debug)]
pub(crate) struct SourcesTelemetry {
    pub(crate) all: bool,
    pub(crate) show_missing: bool,
    pub(crate) provider_filter: Option<CaptureProvider>,
    pub(crate) providers_detected: Option<CountBucket>,
    pub(crate) providers_existing: Option<CountBucket>,
    pub(crate) providers_importable: Option<CountBucket>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TargetKind {
    Session,
    Event,
}

impl TargetKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Event => "event",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RenderFormat {
    Text,
    Json,
    Jsonl,
    Markdown,
    Csv,
    Raw,
}

impl RenderFormat {
    pub(crate) fn from_output_format(value: OutputFormat) -> Self {
        match value {
            OutputFormat::Text => Self::Text,
            OutputFormat::Json => Self::Json,
            OutputFormat::Jsonl => Self::Jsonl,
            OutputFormat::Markdown => Self::Markdown,
        }
    }

    pub(crate) fn from_sql_format(value: SqlFormat) -> Self {
        match value {
            SqlFormat::Table => Self::Text,
            SqlFormat::Json => Self::Json,
            SqlFormat::Csv => Self::Csv,
            SqlFormat::Raw => Self::Raw,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
            Self::Jsonl => "jsonl",
            Self::Markdown => "markdown",
            Self::Csv => "csv",
            Self::Raw => "raw",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TranscriptModeKind {
    Lite,
    Full,
    Log,
}

impl TranscriptModeKind {
    pub(crate) fn from_mode(value: TranscriptMode) -> Self {
        match value {
            TranscriptMode::Lite => Self::Lite,
            TranscriptMode::Full => Self::Full,
            TranscriptMode::Log => Self::Log,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Lite => "lite",
            Self::Full => "full",
            Self::Log => "log",
        }
    }
}

#[derive(Debug)]
pub(crate) struct ShowTelemetry {
    pub(crate) target_kind: TargetKind,
    pub(crate) transcript_mode: Option<TranscriptModeKind>,
    pub(crate) output_format: RenderFormat,
    pub(crate) writes_out_file: bool,
    pub(crate) provider_lookup: bool,
    pub(crate) window: Option<CountBucket>,
    pub(crate) events_returned: Option<CountBucket>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefreshStatus {
    Disabled,
    Skipped,
    NoSources,
    DaemonBackground,
    Completed,
    Failed,
    Background,
    Unknown,
}

impl RefreshStatus {
    pub(crate) fn from_safe_summary(value: &str) -> Self {
        match value {
            "disabled" | "off" => Self::Disabled,
            "skipped" | "not_needed" => Self::Skipped,
            "no_sources" => Self::NoSources,
            "daemon_background" => Self::DaemonBackground,
            "completed" | "success" => Self::Completed,
            "failed" => Self::Failed,
            "background" | "started" => Self::Background,
            _ => Self::Unknown,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Skipped => "skipped",
            Self::NoSources => "no_sources",
            Self::DaemonBackground => "daemon_background",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Background => "background",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug)]
pub(crate) struct SearchTelemetry {
    pub(crate) has_query: bool,
    pub(crate) has_provider_filter: bool,
    pub(crate) has_workspace_filter: bool,
    pub(crate) has_since_filter: bool,
    pub(crate) has_event_type_filter: bool,
    pub(crate) has_file_filter: bool,
    pub(crate) has_session_filter: bool,
    pub(crate) event_results: bool,
    pub(crate) primary_only: bool,
    pub(crate) include_subagents: bool,
    pub(crate) include_current_session: bool,
    pub(crate) limit: CountBucket,
    pub(crate) provider_filter: Option<CaptureProvider>,
    pub(crate) had_existing_store: Option<bool>,
    pub(crate) indexed_content_before_known: Option<bool>,
    pub(crate) had_indexed_content_before: Option<bool>,
    pub(crate) refresh_duration: Option<DurationBucket>,
    pub(crate) refresh_mode: Option<crate::RefreshArg>,
    pub(crate) refresh_status: Option<RefreshStatus>,
    pub(crate) refresh_source_count: Option<CountBucket>,
    pub(crate) store_created: Option<bool>,
    pub(crate) has_indexed_content_after: Option<bool>,
    pub(crate) query_length: Option<TextLengthBucket>,
    pub(crate) query_term_count: Option<CountBucket>,
    pub(crate) query_duration: Option<DurationBucket>,
    pub(crate) backend_requested: Option<crate::SearchBackendArg>,
    pub(crate) backend_effective: Option<crate::SearchBackendArg>,
    pub(crate) result_count: Option<CountBucket>,
    pub(crate) citation_count: Option<CountBucket>,
    pub(crate) zero_result: Option<bool>,
    pub(crate) render_duration: Option<DurationBucket>,
    pub(crate) store: StoreTelemetry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SqlInputKind {
    Inline,
    Stdin,
    File,
    Missing,
}

impl SqlInputKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::Stdin => "stdin",
            Self::File => "file",
            Self::Missing => "missing",
        }
    }
}

#[derive(Debug)]
pub(crate) struct SqlTelemetry {
    pub(crate) input: SqlInputKind,
    pub(crate) output_format: RenderFormat,
    pub(crate) returned_rows: Option<CountBucket>,
    pub(crate) returned_columns: Option<CountBucket>,
    pub(crate) rows_truncated: Option<bool>,
    pub(crate) values_truncated: Option<bool>,
    pub(crate) query_duration: Option<DurationBucket>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DocsOperation {
    List,
    Search,
    Show,
    ManPrint,
    ManGenerate,
}

impl DocsOperation {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Search => "search",
            Self::Show => "show",
            Self::ManPrint => "man_print",
            Self::ManGenerate => "man_generate",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DocTopicId {
    GettingStarted,
    FirstTenMinutes,
    CliReference,
    Docs,
    Search,
    Sql,
    Mcp,
    McpIntegrations,
    Upgrade,
    UnmanagedInstalls,
    AgentUsage,
    AgentSkillInstall,
    SlashCommandIntegrations,
    Sdks,
    JsonContracts,
    Storage,
    Providers,
    CustomHistoryImportFormat,
    HistorySourcePlugins,
    ProviderSupport,
    ProviderImportPolicy,
    Troubleshooting,
    Limitations,
}

impl DocTopicId {
    pub(crate) fn from_known_id(value: &str) -> Option<Self> {
        Some(match value {
            "getting-started" => Self::GettingStarted,
            "first-10-minutes" => Self::FirstTenMinutes,
            "cli-reference" => Self::CliReference,
            "docs" => Self::Docs,
            "search" => Self::Search,
            "sql" => Self::Sql,
            "mcp" => Self::Mcp,
            "mcp-integrations" => Self::McpIntegrations,
            "upgrade" => Self::Upgrade,
            "unmanaged-installs" => Self::UnmanagedInstalls,
            "agent-usage" => Self::AgentUsage,
            "agent-skill-install" => Self::AgentSkillInstall,
            "slash-command-integrations" => Self::SlashCommandIntegrations,
            "sdks" => Self::Sdks,
            "json-contracts" => Self::JsonContracts,
            "storage" => Self::Storage,
            "providers" => Self::Providers,
            "custom-history-import-format" => Self::CustomHistoryImportFormat,
            "history-source-plugins" => Self::HistorySourcePlugins,
            "provider-support" => Self::ProviderSupport,
            "provider-import-policy" => Self::ProviderImportPolicy,
            "troubleshooting" => Self::Troubleshooting,
            "limitations" => Self::Limitations,
            _ => return None,
        })
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::GettingStarted => "getting-started",
            Self::FirstTenMinutes => "first-10-minutes",
            Self::CliReference => "cli-reference",
            Self::Docs => "docs",
            Self::Search => "search",
            Self::Sql => "sql",
            Self::Mcp => "mcp",
            Self::McpIntegrations => "mcp-integrations",
            Self::Upgrade => "upgrade",
            Self::UnmanagedInstalls => "unmanaged-installs",
            Self::AgentUsage => "agent-usage",
            Self::AgentSkillInstall => "agent-skill-install",
            Self::SlashCommandIntegrations => "slash-command-integrations",
            Self::Sdks => "sdks",
            Self::JsonContracts => "json-contracts",
            Self::Storage => "storage",
            Self::Providers => "providers",
            Self::CustomHistoryImportFormat => "custom-history-import-format",
            Self::HistorySourcePlugins => "history-source-plugins",
            Self::ProviderSupport => "provider-support",
            Self::ProviderImportPolicy => "provider-import-policy",
            Self::Troubleshooting => "troubleshooting",
            Self::Limitations => "limitations",
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct DocsTelemetry {
    pub(crate) operation: Option<DocsOperation>,
    pub(crate) implicit_list: bool,
    pub(crate) query_length: Option<TextLengthBucket>,
    pub(crate) query_term_count: Option<CountBucket>,
    pub(crate) result_count: Option<CountBucket>,
    pub(crate) zero_result: Option<bool>,
    pub(crate) topic: Option<DocTopicId>,
    pub(crate) writes_output: bool,
}
