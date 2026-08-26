use std::time::Duration;

use ctx_history_core::CaptureProvider;

use super::{
    BytesBucket, CountBucket, DurationBucket, ProgressMode, SearchHealthFacts, TextLengthBucket,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportSourceMode {
    ExplicitFormat,
    HistorySourcePlugin,
    ExplicitPath,
    AllDiscovered,
    DiscoveredProvider,
    AutoDiscovered,
}

impl ImportSourceMode {
    pub fn as_str(self) -> &'static str {
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
pub enum ImportOutcome {
    Success,
    Failure,
    CompletedWithRejections,
    CompletedWithSourceFailures,
    CompletedWithRejectionsAndSourceFailures,
}

impl ImportOutcome {
    pub fn as_str(self) -> &'static str {
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
pub enum ImportFailureScope {
    None,
    Record,
    Source,
    RecordAndSource,
    Invocation,
}

impl ImportFailureScope {
    pub fn as_str(self) -> &'static str {
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
pub enum ImportFailureType {
    None,
    RecordRejection,
    SourceFailure,
    RecordRejectionAndSourceFailure,
    InvalidRequest,
    Io,
    Other,
}

impl ImportFailureType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::RecordRejection => "record_rejection",
            Self::SourceFailure => "source_failure",
            Self::RecordRejectionAndSourceFailure => "record_rejection_and_source_failure",
            Self::InvalidRequest => "invalid_request",
            Self::Io => "io",
            Self::Other => "other",
        }
    }
}

#[derive(Debug)]
pub struct ImportTelemetry {
    pub resume: bool,
    pub all_sources: bool,
    pub no_daemon: bool,
    pub source_mode: ImportSourceMode,
    pub provider_filter: Option<CaptureProvider>,
    pub reset_cursor: bool,
    pub progress_mode: ProgressMode,
    pub sources_seen: Option<CountBucket>,
    pub source_bytes: Option<BytesBucket>,
    pub source_files: Option<CountBucket>,
    pub failed_sources: Option<CountBucket>,
    pub sessions_imported: Option<CountBucket>,
    pub events_imported: Option<CountBucket>,
    pub edges_imported: Option<CountBucket>,
    pub skipped: Option<CountBucket>,
    pub rejected_records: Option<CountBucket>,
    pub outcome: Option<ImportOutcome>,
    pub failure_scope: Option<ImportFailureScope>,
    pub failure_type: Option<ImportFailureType>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupMode {
    Ready,
    Background,
}

impl SetupMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Background => "background",
        }
    }
}

#[derive(Debug)]
pub struct SetupTelemetry {
    pub catalog_only: bool,
    pub no_daemon: bool,
    pub wait: bool,
    pub progress_mode: ProgressMode,
    pub mode: Option<SetupMode>,
    pub providers_detected: Option<CountBucket>,
    pub cataloged_sessions: Option<CountBucket>,
    pub inventory_sources: Option<CountBucket>,
    pub inventory_source_files: Option<CountBucket>,
    pub pending_sessions: Option<CountBucket>,
    pub catalog_source_bytes: Option<BytesBucket>,
    pub inventory_source_bytes: Option<BytesBucket>,
    pub has_indexed_content: Option<bool>,
    pub import: ImportTelemetry,
}

#[derive(Debug, Default)]
pub struct StatusTelemetry {
    pub initialized: Option<bool>,
    pub indexed_items: Option<CountBucket>,
    pub indexed_sessions: Option<CountBucket>,
    pub indexed_events: Option<CountBucket>,
    pub indexed_sources: Option<CountBucket>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexOperation {
    Status,
    Mode,
    Watch,
    Wait,
}

impl IndexOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Mode => "mode",
            Self::Watch => "watch",
            Self::Wait => "wait",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexState {
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
    pub fn from_safe_summary(value: &str) -> Self {
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

    pub fn as_str(self) -> &'static str {
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
pub enum WaitOutcome {
    Ready,
    Blocked,
    Timeout,
}

impl WaitOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Blocked => "blocked",
            Self::Timeout => "timeout",
        }
    }
}

#[derive(Debug, Default)]
pub struct IndexTelemetry {
    pub operation: Option<IndexOperation>,
    pub wait_lexical: Option<bool>,
    pub wait_semantic: Option<bool>,
    pub wait_outcome: Option<WaitOutcome>,
    pub initialized: Option<bool>,
    pub lexical_state: Option<IndexState>,
    pub semantic_state: Option<IndexState>,
    pub indexed_items: Option<CountBucket>,
}

#[derive(Debug)]
pub struct SourcesTelemetry {
    pub all: bool,
    pub show_missing: bool,
    pub provider_filter: Option<CaptureProvider>,
    pub providers_detected: Option<CountBucket>,
    pub providers_existing: Option<CountBucket>,
    pub providers_importable: Option<CountBucket>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Session,
    Event,
    Events,
}

impl TargetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Event => "event",
            Self::Events => "events",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderFormat {
    Text,
    Json,
    Jsonl,
    Markdown,
}

impl RenderFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
            Self::Jsonl => "jsonl",
            Self::Markdown => "markdown",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptModeKind {
    Lite,
    Full,
    Log,
}

impl TranscriptModeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lite => "lite",
            Self::Full => "full",
            Self::Log => "log",
        }
    }
}

#[derive(Debug)]
pub struct ShowTelemetry {
    pub target_kind: TargetKind,
    pub transcript_mode: Option<TranscriptModeKind>,
    pub output_format: RenderFormat,
    pub writes_out_file: bool,
    pub provider_lookup: bool,
    pub window: Option<CountBucket>,
    pub events_returned: Option<CountBucket>,
}

#[derive(Debug)]
pub struct LocateTelemetry {
    pub target_kind: TargetKind,
    pub output_format: RenderFormat,
    pub provider_lookup: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshStatus {
    Disabled,
    ExistingGeneration,
    Skipped,
    NoSources,
    DaemonBackground,
    DaemonUnavailable,
    Completed,
    Failed,
    Background,
    Unknown,
}

impl RefreshStatus {
    pub fn from_safe_summary(value: &str) -> Self {
        match value {
            "disabled" | "off" => Self::Disabled,
            "existing_generation" => Self::ExistingGeneration,
            "skipped" | "not_needed" => Self::Skipped,
            "no_sources" => Self::NoSources,
            "daemon_background" => Self::DaemonBackground,
            "daemon_unavailable" => Self::DaemonUnavailable,
            "completed" | "success" => Self::Completed,
            "failed" => Self::Failed,
            "background" | "started" => Self::Background,
            _ => Self::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::ExistingGeneration => "existing_generation",
            Self::Skipped => "skipped",
            Self::NoSources => "no_sources",
            Self::DaemonBackground => "daemon_background",
            Self::DaemonUnavailable => "daemon_unavailable",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Background => "background",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug)]
pub struct SearchTelemetry {
    pub has_query: bool,
    pub has_provider_filter: bool,
    pub has_workspace_filter: bool,
    pub has_since_filter: bool,
    pub has_event_type_filter: bool,
    pub has_file_filter: bool,
    pub has_session_filter: bool,
    pub event_results: bool,
    pub primary_only: bool,
    pub include_current_session: bool,
    pub limit: CountBucket,
    pub provider_filter: Option<CaptureProvider>,
    pub refresh_duration: Option<DurationBucket>,
    pub refresh_mode: Option<RefreshMode>,
    pub refresh_status: Option<RefreshStatus>,
    pub refresh_source_count: Option<CountBucket>,
    pub has_indexed_content_after: Option<bool>,
    pub query_length: Option<TextLengthBucket>,
    pub query_term_count: Option<CountBucket>,
    pub query_duration: Option<DurationBucket>,
    pub backend_requested: Option<SearchBackend>,
    pub backend_effective: Option<SearchBackend>,
    pub result_count: Option<CountBucket>,
    pub citation_count: Option<CountBucket>,
    pub zero_result: Option<bool>,
    pub render_duration: Option<DurationBucket>,
    pub output_duration: Option<Duration>,
    pub output_served: Option<bool>,
    pub health: Option<SearchHealthFacts>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshMode {
    Background,
    Off,
    Wait,
}

impl RefreshMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Off => "off",
            Self::Wait => "wait",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchBackend {
    Hybrid,
    Lexical,
    Semantic,
}

impl SearchBackend {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hybrid => "hybrid",
            Self::Lexical => "lexical",
            Self::Semantic => "semantic",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocsOperation {
    List,
    Search,
    Show,
    ManPrint,
    ManGenerate,
}

impl DocsOperation {
    pub fn as_str(self) -> &'static str {
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
pub enum DocTopicId {
    GettingStarted,
    FirstTenMinutes,
    CliReference,
    Docs,
    Search,
    EventQueries,
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
    pub fn from_known_id(value: &str) -> Option<Self> {
        Some(match value {
            "getting-started" => Self::GettingStarted,
            "first-10-minutes" => Self::FirstTenMinutes,
            "cli-reference" => Self::CliReference,
            "docs" => Self::Docs,
            "search" => Self::Search,
            "event-queries" => Self::EventQueries,
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

    pub fn as_str(self) -> &'static str {
        match self {
            Self::GettingStarted => "getting-started",
            Self::FirstTenMinutes => "first-10-minutes",
            Self::CliReference => "cli-reference",
            Self::Docs => "docs",
            Self::Search => "search",
            Self::EventQueries => "event-queries",
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
pub struct DocsTelemetry {
    pub operation: Option<DocsOperation>,
    pub implicit_list: bool,
    pub query_length: Option<TextLengthBucket>,
    pub query_term_count: Option<CountBucket>,
    pub result_count: Option<CountBucket>,
    pub zero_result: Option<bool>,
    pub topic: Option<DocTopicId>,
    pub writes_output: bool,
}
