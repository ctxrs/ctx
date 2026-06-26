use std::{
    env, fs,
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
    thread,
    time::{Duration as StdDuration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context, Result};
use chrono::{Duration, Utc};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde_json::{json, Value};
use uuid::Uuid;

mod analytics;
mod config;
mod identity;
mod net;

use analytics::{AnalyticsEvent, AnalyticsProperties};
use config::{AppConfig, CONFIG_FILE};
use ctx_history_capture::{
    catalog_codex_session_tree, discover_provider_sources, discover_provider_sources_for_provider,
    import_antigravity_cli_history, import_claude_projects_jsonl_tree, import_codex_history_jsonl,
    import_codex_session_jsonl, import_codex_session_jsonl_tail, import_codex_session_paths,
    import_codex_session_tree, import_copilot_cli_session_events, import_cursor_native_history,
    import_factory_ai_droid_sessions, import_gemini_cli_history, import_opencode_sqlite,
    import_pi_session_jsonl, provider_source_for_path, provider_source_spec, stable_capture_uuid,
    AntigravityCliImportOptions, CatalogSummary, ClaudeProjectsImportOptions, CodexEventImportMode,
    CodexHistoryImportOptions, CodexSessionCatalogOptions, CodexSessionImportOptions,
    CodexSessionImportProgress, CodexSessionImportProgressCallback, CodexToolOutputMode,
    CopilotCliImportOptions, CursorNativeImportOptions, FactoryAiDroidImportOptions,
    GeminiCliImportOptions, OpenCodeSqliteImportOptions, PiSessionImportOptions,
    ProviderImportSummary, ProviderImportSupport, ProviderSource, ProviderSourceStatus,
};
use ctx_history_core::{
    database_path, default_data_root, CaptureProvider, ContextCitation, ContextCitationType, Event,
    EventRole, EventType, HistoryRecord, ProviderRawRetention, RedactionState, Session,
};
use ctx_history_store::{
    CatalogSession, CatalogSourceIndexUpdate, SourceImportFile, SourceImportFileIndexUpdate, Store,
};

const WAL_TRUNCATE_MIN_BYTES: u64 = 64 * 1024 * 1024;
const LARGE_IMPORT_SOURCE_FILES_WARNING: usize = 10_000;
const LARGE_IMPORT_SOURCE_BYTES_WARNING: u64 = 1024 * 1024 * 1024;
const MAX_SEARCH_LIMIT: usize = 200;

#[derive(Debug, Parser)]
#[command(name = "ctx", version, about = "Search local agent history")]
struct Cli {
    #[arg(long, env = "CTX_DATA_ROOT", global = true)]
    data_root: Option<PathBuf>,
    #[command(subcommand)]
    command: CommandRoot,
}

#[derive(Debug, Subcommand)]
enum CommandRoot {
    #[command(about = "Create local ctx storage and index discovered history")]
    Setup(SetupArgs),
    #[command(about = "Show local ctx index status")]
    Status(JsonArgs),
    #[command(about = "List configured and discovered agent history sources")]
    Sources(JsonArgs),
    #[command(about = "Index provider history into local search")]
    Import(ImportArgs),
    #[command(about = "List indexed agent history items")]
    List(ListArgs),
    #[command(about = "Show an indexed session transcript or event")]
    Show(ShowArgs),
    #[command(about = "Locate provider/source metadata for an indexed session or event")]
    Locate(LocateArgs),
    #[command(about = "Export an indexed session transcript")]
    Export(ExportArgs),
    #[command(about = "Search indexed agent history")]
    Search(SearchArgs),
    #[command(about = "Check local ctx health")]
    Doctor(JsonArgs),
    #[command(about = "Validate local ctx storage")]
    Validate(JsonArgs),
}

#[derive(Debug, Args)]
struct SetupArgs {
    #[arg(long, alias = "no-import")]
    catalog_only: bool,
    #[arg(long)]
    json: bool,
    #[arg(long, value_enum, default_value_t = ProgressArg::Auto)]
    progress: ProgressArg,
}

#[derive(Debug, Args, Clone)]
struct JsonArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ImportArgs {
    #[arg(long, value_enum)]
    provider: Option<ProviderArg>,
    #[arg(long)]
    path: Option<PathBuf>,
    #[arg(long, conflicts_with_all = ["provider", "path"])]
    all: bool,
    #[arg(long)]
    resume: bool,
    #[arg(long)]
    json: bool,
    #[arg(long, value_enum, default_value_t = ProgressArg::Auto)]
    progress: ProgressArg,
}

#[derive(Debug, Args)]
struct ListArgs {
    #[arg(long, default_value_t = 20)]
    limit: usize,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ShowArgs {
    #[command(subcommand)]
    target: ShowTarget,
}

#[derive(Debug, Subcommand)]
enum ShowTarget {
    #[command(about = "Show a session transcript")]
    Session(ShowSessionArgs),
    #[command(about = "Show one event or a surrounding event window")]
    Event(ShowEventArgs),
}

#[derive(Debug, Args)]
struct ShowSessionArgs {
    id: Option<Uuid>,
    #[arg(long, value_enum)]
    provider: Option<ProviderArg>,
    #[arg(long = "provider-session")]
    provider_session: Option<String>,
    #[arg(long, value_enum, default_value_t = TranscriptMode::Full)]
    mode: TranscriptMode,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ShowEventArgs {
    id: Uuid,
    #[arg(long, default_value_t = 0)]
    before: usize,
    #[arg(long, default_value_t = 0)]
    after: usize,
    #[arg(long)]
    window: Option<usize>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct LocateArgs {
    #[command(subcommand)]
    target: LocateTarget,
}

#[derive(Debug, Subcommand)]
enum LocateTarget {
    #[command(about = "Locate provider/source metadata for a session")]
    Session(LocateSessionArgs),
    #[command(about = "Locate provider/source metadata for an event")]
    Event(LocateEventArgs),
}

#[derive(Debug, Args)]
struct LocateSessionArgs {
    id: Option<Uuid>,
    #[arg(long, value_enum)]
    provider: Option<ProviderArg>,
    #[arg(long = "provider-session")]
    provider_session: Option<String>,
    #[arg(long, value_enum, default_value_t = LocateFormat::Text)]
    format: LocateFormat,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct LocateEventArgs {
    id: Uuid,
    #[arg(long, value_enum, default_value_t = LocateFormat::Text)]
    format: LocateFormat,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ExportArgs {
    #[command(subcommand)]
    target: ExportTarget,
}

#[derive(Debug, Subcommand)]
enum ExportTarget {
    #[command(about = "Export a session transcript")]
    Session(ExportSessionArgs),
}

#[derive(Debug, Args)]
struct ExportSessionArgs {
    id: Option<Uuid>,
    #[arg(long, value_enum)]
    provider: Option<ProviderArg>,
    #[arg(long = "provider-session")]
    provider_session: Option<String>,
    #[arg(long, value_enum, default_value_t = TranscriptMode::Full)]
    mode: TranscriptMode,
    #[arg(long, value_enum, default_value_t = OutputFormat::Markdown)]
    format: OutputFormat,
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct SearchArgs {
    query: Option<String>,
    #[arg(
        long,
        default_value_t = 20,
        value_parser = parse_search_limit,
        help = "Maximum results to return, from 1 to 200"
    )]
    limit: usize,
    #[arg(long)]
    provider: Option<ProviderArg>,
    #[arg(long)]
    repo: Option<String>,
    #[arg(long)]
    since: Option<String>,
    #[arg(long)]
    primary_only: bool,
    #[arg(long)]
    include_subagents: bool,
    #[arg(long)]
    event_type: Option<String>,
    #[arg(long)]
    file: Option<PathBuf>,
    #[arg(
        long,
        value_enum,
        default_value_t = RefreshArg::Auto,
        help = "Pre-search refresh behavior: auto, off, or strict",
        long_help = "Pre-search refresh behavior. auto best-effort refreshes discovered native provider sources and serves the existing index if refresh fails; off searches the existing index only; strict fails if the refresh cannot run or import successfully."
    )]
    refresh: RefreshArg,
    #[arg(long)]
    json: bool,
}

impl CommandRoot {
    fn name(&self) -> &'static str {
        match self {
            Self::Setup(_) => "setup",
            Self::Status(_) => "status",
            Self::Sources(_) => "sources",
            Self::Import(_) => "import",
            Self::List(_) => "list",
            Self::Show(_) => "show",
            Self::Locate(_) => "locate",
            Self::Export(_) => "export",
            Self::Search(_) => "search",
            Self::Doctor(_) => "doctor",
            Self::Validate(_) => "validate",
        }
    }

    fn json_output(&self) -> bool {
        match self {
            Self::Setup(args) => args.json,
            Self::Status(args) => args.json,
            Self::Sources(args) => args.json,
            Self::Import(args) => args.json,
            Self::List(args) => args.json,
            Self::Show(args) => args.json_output(),
            Self::Locate(args) => args.json_output(),
            Self::Export(args) => args.json_output(),
            Self::Search(args) => args.json,
            Self::Doctor(args) => args.json,
            Self::Validate(args) => args.json,
        }
    }
}

impl ShowArgs {
    fn json_output(&self) -> bool {
        match &self.target {
            ShowTarget::Session(args) => args.json || args.format == OutputFormat::Json,
            ShowTarget::Event(args) => args.json || args.format == OutputFormat::Json,
        }
    }
}

impl LocateArgs {
    fn json_output(&self) -> bool {
        match &self.target {
            LocateTarget::Session(args) => args.json || args.format == LocateFormat::Json,
            LocateTarget::Event(args) => args.json || args.format == LocateFormat::Json,
        }
    }
}

impl ExportArgs {
    fn json_output(&self) -> bool {
        matches!(
            &self.target,
            ExportTarget::Session(args) if args.out.is_none() && args.format == OutputFormat::Json
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum TranscriptMode {
    Full,
    Lite,
    Log,
}

impl TranscriptMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Lite => "lite",
            Self::Log => "log",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum RefreshArg {
    Auto,
    Off,
    Strict,
}

impl RefreshArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Off => "off",
            Self::Strict => "strict",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    Text,
    Markdown,
    Json,
    Jsonl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum LocateFormat {
    Text,
    Json,
}

impl LocateFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
        }
    }
}

impl OutputFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Markdown => "markdown",
            Self::Json => "json",
            Self::Jsonl => "jsonl",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ProviderArg {
    Codex,
    Pi,
    #[value(alias = "claude-code")]
    Claude,
    #[value(name = "opencode", alias = "open-code")]
    OpenCode,
    #[value(alias = "antigravity-cli")]
    Antigravity,
    #[value(alias = "gemini-cli")]
    Gemini,
    Cursor,
    #[value(alias = "copilot")]
    CopilotCli,
    #[value(alias = "factoryai-droid", alias = "factory-droid")]
    FactoryAiDroid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ProgressArg {
    Auto,
    Plain,
    Json,
    None,
}

impl ProviderArg {
    fn capture_provider(self) -> CaptureProvider {
        match self {
            Self::Codex => CaptureProvider::Codex,
            Self::Pi => CaptureProvider::Pi,
            Self::Claude => CaptureProvider::Claude,
            Self::OpenCode => CaptureProvider::OpenCode,
            Self::Antigravity => CaptureProvider::Antigravity,
            Self::Gemini => CaptureProvider::Gemini,
            Self::Cursor => CaptureProvider::Cursor,
            Self::CopilotCli => CaptureProvider::CopilotCli,
            Self::FactoryAiDroid => CaptureProvider::FactoryAiDroid,
        }
    }
}

type SourceInfo = ProviderSource;

#[derive(Debug, Clone, Default)]
struct ImportTotals {
    source_files: usize,
    source_bytes: u64,
    imported_sources: usize,
    failed_sources: usize,
    imported_sessions: usize,
    imported_events: usize,
    imported_edges: usize,
    skipped: usize,
    failed: usize,
}

#[derive(Debug)]
struct ImportReport {
    resume: bool,
    totals: ImportTotals,
    sources: Vec<Value>,
}

impl ImportReport {
    fn empty(resume: bool) -> Self {
        Self {
            resume,
            totals: ImportTotals::default(),
            sources: Vec::new(),
        }
    }

    fn resume_mode(&self) -> &'static str {
        resume_mode_name(self.resume)
    }
}

#[derive(Debug, Clone, Copy)]
struct ImportRunOptions {
    progress: ProgressArg,
    json: bool,
    print_human: bool,
    allow_empty_sources: bool,
    operation: &'static str,
}

fn resume_mode_name(resume: bool) -> &'static str {
    if resume {
        "idempotent_rescan"
    } else {
        "normal_scan"
    }
}

impl ImportTotals {
    fn add(&mut self, summary: &ProviderImportSummary, stats: &SourceStats) {
        self.source_files += stats.files;
        self.source_bytes = self.source_bytes.saturating_add(stats.bytes);
        self.imported_sources += 1;
        self.imported_sessions += summary.imported_sessions;
        self.imported_events += summary.imported_events;
        self.imported_edges += summary.imported_edges;
        self.skipped += summary.skipped;
        self.failed += summary.failed;
    }

    fn add_source_failure(&mut self, stats: &SourceStats) {
        self.source_files += stats.files;
        self.source_bytes = self.source_bytes.saturating_add(stats.bytes);
        self.failed_sources += 1;
    }
}

#[derive(Debug, Default)]
struct CatalogTotals {
    sources: usize,
    source_files: usize,
    source_bytes: u64,
    cataloged_sessions: usize,
    cached_sessions: usize,
    parsed_sessions: usize,
    skipped_sessions: usize,
    failed_sessions: usize,
}

impl CatalogTotals {
    fn add(&mut self, summary: &CatalogSummary) {
        self.sources += 1;
        self.source_files += summary.source_files;
        self.source_bytes = self.source_bytes.saturating_add(summary.source_bytes);
        self.cataloged_sessions += summary.cataloged_sessions;
        self.cached_sessions += summary.cached_sessions;
        self.parsed_sessions += summary.parsed_sessions;
        self.skipped_sessions += summary.skipped_sessions;
        self.failed_sessions += summary.failed_sessions;
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct SourceStats {
    files: usize,
    bytes: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct SourceProgressSnapshot {
    completed_bytes: u64,
    total_bytes: u64,
}

#[derive(Debug, Clone)]
struct SearchRefreshReport {
    mode: RefreshArg,
    status: &'static str,
    source_count: usize,
    totals: ImportTotals,
    error: Option<String>,
}

impl SearchRefreshReport {
    fn skipped(mode: RefreshArg, status: &'static str) -> Self {
        Self {
            mode,
            status,
            source_count: 0,
            totals: ImportTotals::default(),
            error: None,
        }
    }

    fn completed(mode: RefreshArg, source_count: usize, totals: ImportTotals) -> Self {
        Self {
            mode,
            status: "completed",
            source_count,
            totals,
            error: None,
        }
    }

    fn failed(mode: RefreshArg, source_count: usize, error: String) -> Self {
        Self {
            mode,
            status: "failed",
            source_count,
            totals: ImportTotals::default(),
            error: Some(error),
        }
    }

    fn to_json(&self) -> Value {
        compact_json(json!({
            "mode": self.mode.as_str(),
            "status": self.status,
            "source_count": self.source_count,
            "totals": import_totals_json(&self.totals),
            "error": self.error,
        }))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProgressRenderMode {
    None,
    Plain { interactive: bool },
    Json,
}

#[derive(Debug)]
struct ProgressState {
    started: Instant,
    last_emit: Option<Instant>,
    last_line_len: usize,
}

#[derive(Clone)]
struct ProgressReporter {
    mode: ProgressRenderMode,
    operation: &'static str,
    total_bytes: u64,
    state: Arc<Mutex<ProgressState>>,
}

impl ProgressReporter {
    fn new(arg: ProgressArg, json_output: bool, operation: &'static str, total_bytes: u64) -> Self {
        let stderr_is_terminal = std::io::stderr().is_terminal();
        let mode = match arg {
            ProgressArg::None => ProgressRenderMode::None,
            ProgressArg::Json => ProgressRenderMode::Json,
            ProgressArg::Plain => ProgressRenderMode::Plain {
                interactive: stderr_is_terminal,
            },
            ProgressArg::Auto if json_output || !stderr_is_terminal => ProgressRenderMode::None,
            ProgressArg::Auto => ProgressRenderMode::Plain { interactive: true },
        };
        Self {
            mode,
            operation,
            total_bytes,
            state: Arc::new(Mutex::new(ProgressState {
                started: Instant::now(),
                last_emit: None,
                last_line_len: 0,
            })),
        }
    }

    fn is_enabled(&self) -> bool {
        self.mode != ProgressRenderMode::None
    }

    fn message(&self, phase: &'static str, message: impl Into<String>) {
        if !self.is_enabled() {
            return;
        }
        let message = message.into();
        self.emit(ProgressLine {
            phase,
            message,
            completed_bytes: 0,
            total_bytes: self.total_bytes,
            completed_files: None,
            total_files: None,
            imported_events: None,
            done: false,
            force: true,
        });
    }

    fn done(&self, phase: &'static str, message: impl Into<String>, completed_bytes: u64) {
        if !self.is_enabled() {
            return;
        }
        self.emit(ProgressLine {
            phase,
            message: message.into(),
            completed_bytes,
            total_bytes: self.total_bytes.max(completed_bytes),
            completed_files: None,
            total_files: None,
            imported_events: None,
            done: true,
            force: true,
        });
    }

    fn finish_line(&self) {
        let mut state = self.state.lock().expect("progress state poisoned");
        if matches!(self.mode, ProgressRenderMode::Plain { interactive: true })
            && state.last_line_len > 0
        {
            eprintln!();
            state.last_line_len = 0;
        }
    }

    fn warning(&self, message: impl AsRef<str>) {
        if matches!(self.mode, ProgressRenderMode::None) {
            return;
        }
        self.finish_line();
        match self.mode {
            ProgressRenderMode::Json => {
                eprintln!(
                    "{}",
                    json!({
                        "type": "ctx_progress",
                        "operation": self.operation,
                        "level": "warning",
                        "message": message.as_ref(),
                    })
                );
            }
            ProgressRenderMode::Plain { .. } => eprintln!("warning: {}", message.as_ref()),
            ProgressRenderMode::None => {}
        }
    }

    fn codex_import_callback(
        &self,
        source: &SourceInfo,
        source_offset_bytes: u64,
    ) -> Option<CodexSessionImportProgressCallback> {
        if !self.is_enabled() || source.provider != CaptureProvider::Codex {
            return None;
        }
        let reporter = self.clone();
        let provider = source.provider.as_str().to_owned();
        Some(Arc::new(move |progress: CodexSessionImportProgress| {
            let completed_bytes = source_offset_bytes.saturating_add(progress.completed_bytes);
            reporter.emit(ProgressLine {
                phase: "indexing",
                message: provider.clone(),
                completed_bytes,
                total_bytes: reporter.total_bytes.max(completed_bytes),
                completed_files: Some(progress.completed_files),
                total_files: Some(progress.total_files),
                imported_events: Some(progress.imported_events),
                done: progress.done,
                force: progress.done,
            });
        }))
    }

    fn parallel_codex_import_callback(
        &self,
        source: &SourceInfo,
        source_index: usize,
        source_states: Arc<Mutex<Vec<SourceProgressSnapshot>>>,
    ) -> Option<CodexSessionImportProgressCallback> {
        if !self.is_enabled() || source.provider != CaptureProvider::Codex {
            return None;
        }
        let reporter = self.clone();
        let provider = source.provider.as_str().to_owned();
        Some(Arc::new(move |progress: CodexSessionImportProgress| {
            let (completed_bytes, total_bytes) = {
                let mut states = source_states
                    .lock()
                    .expect("parallel progress state poisoned");
                if let Some(state) = states.get_mut(source_index) {
                    state.total_bytes = state.total_bytes.max(progress.total_bytes);
                    state.completed_bytes = progress
                        .completed_bytes
                        .min(state.total_bytes.max(progress.completed_bytes));
                }
                aggregate_source_progress(&states)
            };
            reporter.emit(ProgressLine {
                phase: "indexing",
                message: provider.clone(),
                completed_bytes,
                total_bytes: reporter.total_bytes.max(total_bytes).max(completed_bytes),
                completed_files: Some(progress.completed_files),
                total_files: Some(progress.total_files),
                imported_events: Some(progress.imported_events),
                done: progress.done,
                force: progress.done,
            });
        }))
    }

    fn parallel_source_done(
        &self,
        source: &SourceInfo,
        source_index: usize,
        source_states: &Arc<Mutex<Vec<SourceProgressSnapshot>>>,
        stats: SourceStats,
        summary: &ProviderImportSummary,
    ) {
        if !self.is_enabled() {
            return;
        }
        let (completed_bytes, total_bytes) = {
            let mut states = source_states
                .lock()
                .expect("parallel progress state poisoned");
            if let Some(state) = states.get_mut(source_index) {
                state.total_bytes = state.total_bytes.max(stats.bytes);
                state.completed_bytes = state.total_bytes;
            }
            aggregate_source_progress(&states)
        };
        self.emit(ProgressLine {
            phase: "indexing",
            message: format!("imported {}", source.provider.as_str()),
            completed_bytes,
            total_bytes: self.total_bytes.max(total_bytes).max(completed_bytes),
            completed_files: Some(stats.files),
            total_files: Some(stats.files),
            imported_events: Some(summary.imported_events),
            done: true,
            force: true,
        });
    }

    fn parallel_source_failed(
        &self,
        source: &SourceInfo,
        source_index: usize,
        source_states: &Arc<Mutex<Vec<SourceProgressSnapshot>>>,
        stats: SourceStats,
        error: &str,
    ) {
        if !self.is_enabled() {
            return;
        }
        let (completed_bytes, total_bytes) = {
            let mut states = source_states
                .lock()
                .expect("parallel progress state poisoned");
            if let Some(state) = states.get_mut(source_index) {
                state.total_bytes = state.total_bytes.max(stats.bytes);
                state.completed_bytes = state.total_bytes;
            }
            aggregate_source_progress(&states)
        };
        self.emit(ProgressLine {
            phase: "indexing",
            message: format!(
                "skipped {}: {}",
                source.provider.as_str(),
                source_error_reason(source, error)
            ),
            completed_bytes,
            total_bytes: self.total_bytes.max(total_bytes).max(completed_bytes),
            completed_files: Some(stats.files),
            total_files: Some(stats.files),
            imported_events: Some(0),
            done: true,
            force: true,
        });
    }

    fn emit(&self, line: ProgressLine) {
        let mut state = self.state.lock().expect("progress state poisoned");
        let now = Instant::now();
        if !line.force
            && state
                .last_emit
                .is_some_and(|last| now.duration_since(last) < StdDuration::from_millis(900))
        {
            return;
        }
        state.last_emit = Some(now);
        let elapsed = now.duration_since(state.started);
        match self.mode {
            ProgressRenderMode::None => {}
            ProgressRenderMode::Json => {
                let value = json!({
                    "type": "ctx_progress",
                    "operation": self.operation,
                    "phase": line.phase,
                    "message": line.message,
                    "completed_bytes": line.completed_bytes,
                    "total_bytes": line.total_bytes,
                    "percent": progress_percent(line.completed_bytes, line.total_bytes),
                    "elapsed_seconds": elapsed.as_secs_f64(),
                    "eta_seconds": eta_seconds(line.completed_bytes, line.total_bytes, elapsed),
                    "completed_files": line.completed_files,
                    "total_files": line.total_files,
                    "imported_events": line.imported_events,
                    "done": line.done,
                });
                eprintln!("{value}");
            }
            ProgressRenderMode::Plain { interactive } => {
                let rendered = render_progress_line(&line, elapsed);
                if interactive {
                    let padding = state.last_line_len.saturating_sub(rendered.len());
                    eprint!("\r{}{}", rendered, " ".repeat(padding));
                    if line.done {
                        eprintln!();
                        state.last_line_len = 0;
                    } else {
                        state.last_line_len = rendered.len();
                        let _ = std::io::stderr().flush();
                    }
                } else {
                    eprintln!("{rendered}");
                }
            }
        }
    }
}

fn aggregate_source_progress(states: &[SourceProgressSnapshot]) -> (u64, u64) {
    states
        .iter()
        .fold((0u64, 0u64), |(completed, total), state| {
            let source_total = state.total_bytes.max(state.completed_bytes);
            (
                completed.saturating_add(state.completed_bytes.min(source_total)),
                total.saturating_add(source_total),
            )
        })
}

struct ProgressLine {
    phase: &'static str,
    message: String,
    completed_bytes: u64,
    total_bytes: u64,
    completed_files: Option<usize>,
    total_files: Option<usize>,
    imported_events: Option<usize>,
    done: bool,
    force: bool,
}

fn render_progress_line(line: &ProgressLine, elapsed: StdDuration) -> String {
    let percent = progress_percent(line.completed_bytes, line.total_bytes);
    let bar = progress_bar(percent, 20);
    let eta = eta_seconds(line.completed_bytes, line.total_bytes, elapsed)
        .map(format_seconds)
        .unwrap_or_else(|| "estimating".to_owned());
    let files = match (line.completed_files, line.total_files) {
        (Some(done), Some(total)) if total > 0 => format!(" {done}/{total} files"),
        _ => String::new(),
    };
    let events = line
        .imported_events
        .map(|events| format!(" {events} events"))
        .unwrap_or_default();
    let remaining = if line.done {
        "done".to_owned()
    } else {
        format!("{eta} left")
    };
    format!(
        "{:<10} [{}] {:>5.1}% {}/{}{}{} {} - {}",
        line.phase,
        bar,
        percent,
        format_bytes(line.completed_bytes),
        format_bytes(line.total_bytes),
        files,
        events,
        remaining,
        line.message
    )
}

fn progress_percent(completed: u64, total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    ((completed as f64 / total as f64) * 100.0).clamp(0.0, 100.0)
}

fn eta_seconds(completed: u64, total: u64, elapsed: StdDuration) -> Option<f64> {
    if completed == 0 || total <= completed {
        return None;
    }
    let rate = completed as f64 / elapsed.as_secs_f64().max(0.001);
    if rate <= 0.0 {
        return None;
    }
    Some((total - completed) as f64 / rate)
}

fn progress_bar(percent: f64, width: usize) -> String {
    let filled = ((percent / 100.0) * width as f64).round() as usize;
    format!(
        "{}{}",
        "#".repeat(filled.min(width)),
        "-".repeat(width.saturating_sub(filled))
    )
}

fn format_seconds(seconds: f64) -> String {
    let seconds = seconds.max(0.0).round() as u64;
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        let minutes = seconds / 60;
        let rem = seconds % 60;
        format!("{minutes}m{rem:02}s")
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes}B")
    } else {
        format!("{value:.1}{}", UNITS[unit])
    }
}

struct ListItemDto;
struct ShowDto;
struct SearchDto;

fn main() -> Result<()> {
    let started = Instant::now();
    let cli = Cli::parse();
    let action = cli.command.name();
    let json_output = cli.command.json_output();
    let mut analytics_properties = command_analytics_properties(&cli.command);
    let data_root = cli
        .data_root
        .clone()
        .map(Ok)
        .unwrap_or_else(default_data_root)
        .context("resolve ctx data root")?;
    let config = AppConfig::load(&data_root)?;

    let result = match cli.command {
        CommandRoot::Setup(args) => run_setup(args, data_root.clone(), &mut analytics_properties),
        CommandRoot::Status(args) => run_status(args, data_root.clone(), &mut analytics_properties),
        CommandRoot::Sources(args) => run_sources(args, &mut analytics_properties),
        CommandRoot::Import(args) => run_import(args, data_root.clone(), &mut analytics_properties),
        CommandRoot::List(args) => run_list(args, data_root.clone(), &mut analytics_properties),
        CommandRoot::Show(args) => run_show(args, data_root.clone(), &mut analytics_properties),
        CommandRoot::Locate(args) => run_locate(args, data_root.clone(), &mut analytics_properties),
        CommandRoot::Export(args) => run_export(args, data_root.clone(), &mut analytics_properties),
        CommandRoot::Search(args) => run_search(args, data_root.clone(), &mut analytics_properties),
        CommandRoot::Doctor(args) => run_doctor(args, data_root.clone(), &mut analytics_properties),
        CommandRoot::Validate(args) => {
            run_validate(args, data_root.clone(), &mut analytics_properties)
        }
    };
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
    result
}

fn command_analytics_properties(command: &CommandRoot) -> AnalyticsProperties {
    let mut properties = analytics::empty_properties();
    match command {
        CommandRoot::Setup(args) => {
            analytics::insert_bool(&mut properties, "catalog_only", args.catalog_only);
            analytics::insert_str(
                &mut properties,
                "progress_mode",
                progress_mode_name(args.progress),
            );
        }
        CommandRoot::Status(_)
        | CommandRoot::Sources(_)
        | CommandRoot::Doctor(_)
        | CommandRoot::Validate(_) => {}
        CommandRoot::Import(args) => {
            analytics::insert_bool(&mut properties, "resume", args.resume);
            analytics::insert_bool(&mut properties, "all_sources", args.all);
            analytics::insert_str(
                &mut properties,
                "source_mode",
                if args.path.is_some() {
                    "explicit_path"
                } else if args.all {
                    "all_discovered"
                } else if args.provider.is_some() {
                    "discovered_provider"
                } else {
                    "auto_discovered"
                },
            );
            if let Some(provider) = args.provider {
                analytics::insert_str(
                    &mut properties,
                    "provider_filter",
                    provider.capture_provider().as_str(),
                );
            }
            analytics::insert_str(
                &mut properties,
                "progress_mode",
                progress_mode_name(args.progress),
            );
        }
        CommandRoot::List(args) => {
            analytics::insert_count_bucket(&mut properties, "limit_bucket", args.limit as u64);
        }
        CommandRoot::Show(args) => match &args.target {
            ShowTarget::Session(args) => {
                analytics::insert_str(&mut properties, "target_kind", "session");
                analytics::insert_str(&mut properties, "transcript_mode", args.mode.as_str());
                analytics::insert_str(&mut properties, "output_format", args.format.as_str());
                analytics::insert_bool(
                    &mut properties,
                    "provider_lookup",
                    args.provider.is_some() || args.provider_session.is_some(),
                );
            }
            ShowTarget::Event(args) => {
                analytics::insert_str(&mut properties, "target_kind", "event");
                analytics::insert_str(&mut properties, "output_format", args.format.as_str());
                analytics::insert_count_bucket(
                    &mut properties,
                    "window_bucket",
                    args.window.unwrap_or(args.before.max(args.after)) as u64,
                );
            }
        },
        CommandRoot::Locate(args) => match &args.target {
            LocateTarget::Session(args) => {
                analytics::insert_str(&mut properties, "target_kind", "session");
                analytics::insert_str(&mut properties, "output_format", args.format.as_str());
                analytics::insert_bool(
                    &mut properties,
                    "provider_lookup",
                    args.provider.is_some() || args.provider_session.is_some(),
                );
            }
            LocateTarget::Event(args) => {
                analytics::insert_str(&mut properties, "target_kind", "event");
                analytics::insert_str(&mut properties, "output_format", args.format.as_str());
            }
        },
        CommandRoot::Export(args) => match &args.target {
            ExportTarget::Session(args) => {
                analytics::insert_str(&mut properties, "target_kind", "session");
                analytics::insert_str(&mut properties, "transcript_mode", args.mode.as_str());
                analytics::insert_str(&mut properties, "output_format", args.format.as_str());
                analytics::insert_bool(&mut properties, "writes_out_file", args.out.is_some());
                analytics::insert_bool(
                    &mut properties,
                    "provider_lookup",
                    args.provider.is_some() || args.provider_session.is_some(),
                );
            }
        },
        CommandRoot::Search(args) => {
            analytics::insert_bool(&mut properties, "has_query", args.query.is_some());
            analytics::insert_bool(
                &mut properties,
                "has_provider_filter",
                args.provider.is_some(),
            );
            analytics::insert_bool(&mut properties, "has_repo_filter", args.repo.is_some());
            analytics::insert_bool(&mut properties, "has_since_filter", args.since.is_some());
            analytics::insert_bool(
                &mut properties,
                "has_event_type_filter",
                args.event_type.is_some(),
            );
            analytics::insert_bool(&mut properties, "has_file_filter", args.file.is_some());
            analytics::insert_bool(&mut properties, "primary_only", args.primary_only);
            analytics::insert_bool(&mut properties, "include_subagents", args.include_subagents);
            analytics::insert_count_bucket(&mut properties, "limit_bucket", args.limit as u64);
            if let Some(provider) = args.provider {
                analytics::insert_str(
                    &mut properties,
                    "provider_filter",
                    provider.capture_provider().as_str(),
                );
            }
        }
    }
    properties
}

fn progress_mode_name(progress: ProgressArg) -> &'static str {
    match progress {
        ProgressArg::Auto => "auto",
        ProgressArg::Plain => "plain",
        ProgressArg::Json => "json",
        ProgressArg::None => "none",
    }
}

fn run_setup(
    args: SetupArgs,
    data_root: PathBuf,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    fs::create_dir_all(&data_root)?;
    let db_path = database_path(data_root.clone());
    let store = Store::open(&db_path)?;
    let config_path = data_root.join(CONFIG_FILE);
    config::write_default_config(&data_root)?;
    let sources = discovered_sources();
    let progress = ProgressReporter::new(args.progress, args.json, "setup", 0);
    progress.message("cataloging", "cataloging discovered Codex sessions");
    let (catalog, catalog_sources) = catalog_available_sources(&store, &sources)?;
    progress.done(
        "cataloging",
        format!("cataloged {} Codex sessions", catalog.cataloged_sessions),
        catalog.source_bytes,
    );
    let catalog_counts = store.catalog_session_counts()?;
    analytics::insert_count_bucket(
        analytics_properties,
        "providers_detected_bucket",
        sources.len() as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "cataloged_sessions_bucket",
        catalog.cataloged_sessions as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "pending_sessions_bucket",
        catalog_counts.pending as u64,
    );
    analytics::insert_bytes_bucket(
        analytics_properties,
        "catalog_source_bytes_bucket",
        catalog.source_bytes,
    );
    let import_report = if args.catalog_only {
        None
    } else {
        drop(store);
        let import_args = ImportArgs {
            provider: None,
            path: None,
            all: true,
            resume: false,
            json: args.json,
            progress: args.progress,
        };
        Some(run_import_internal(
            &import_args,
            data_root.clone(),
            analytics_properties,
            ImportRunOptions {
                progress: args.progress,
                json: args.json,
                print_human: !args.json,
                allow_empty_sources: true,
                operation: "setup",
            },
        )?)
    };
    let setup_store = Store::open(&db_path)?;
    let catalog_counts = setup_store.catalog_session_counts()?;
    let indexed_items = indexed_history_item_count(&setup_store)?;

    if args.json {
        print_json(json!({
            "schema_version": 1,
            "data_root": data_root,
            "database_path": db_path,
            "config_path": config_path,
            "mode": if args.catalog_only { "catalog_only" } else { "ready" },
            "indexed_items": indexed_items,
            "sources": sources_json(&sources),
            "catalog": {
                "sources": catalog.sources,
                "source_files": catalog.source_files,
                "source_bytes": catalog.source_bytes,
                "cataloged_sessions": catalog.cataloged_sessions,
                "cached_sessions": catalog.cached_sessions,
                "parsed_sessions": catalog.parsed_sessions,
                "indexed_sessions": catalog_counts.indexed,
                "pending_sessions": catalog_counts.pending,
                "skipped_sessions": catalog.skipped_sessions,
                "failed_sessions": catalog.failed_sessions,
                "failed_index_sessions": catalog_counts.failed,
                "stale_sessions": catalog_counts.stale,
            },
            "catalog_sources": catalog_sources,
            "import": setup_import_json(import_report.as_ref()),
            "network_required": false,
            "repo_writes": false,
        }))?;
    } else {
        progress.finish_line();
        print_setup_status_line(
            import_report.as_ref(),
            args.catalog_only,
            catalog_counts.pending,
            indexed_items,
        );
        println!("data_root: {}", data_root.display());
        println!("database_path: {}", db_path.display());
        println!("config_path: {}", config_path.display());
        println!("indexed_items: {indexed_items}");
        println!("cataloged_sessions: {}", catalog.cataloged_sessions);
        println!("cached_catalog_sessions: {}", catalog.cached_sessions);
        println!("parsed_catalog_sessions: {}", catalog.parsed_sessions);
        println!("indexed_catalog_sessions: {}", catalog_counts.indexed);
        println!("pending_catalog_sessions: {}", catalog_counts.pending);
        println!("failed_catalog_sessions: {}", catalog_counts.failed);
        println!("stale_catalog_sessions: {}", catalog_counts.stale);
        println!("catalog_source_files: {}", catalog.source_files);
        println!("catalog_source_bytes: {}", catalog.source_bytes);
        if let Some(report) = &import_report {
            println!("imported_sources: {}", report.totals.imported_sources);
            println!("failed_sources: {}", report.totals.failed_sources);
            println!("imported_sessions: {}", report.totals.imported_sessions);
            println!("imported_events: {}", report.totals.imported_events);
            println!("imported_edges: {}", report.totals.imported_edges);
        }
        println!("next_steps:");
        if args.catalog_only {
            println!("  ctx import --all");
            println!("  ctx sources");
        } else if setup_has_indexed_content(indexed_items) {
            println!("  ctx search \"what failed before\"");
            println!("  ctx sources");
            if setup_has_failed_sources(import_report.as_ref()) {
                println!("  ctx import --provider <provider>");
            }
        } else {
            println!("  ctx sources");
            println!("  ctx import --all");
        }
    }
    Ok(())
}

fn setup_import_json(report: Option<&ImportReport>) -> Value {
    match report {
        Some(report) => json!({
            "ran": true,
            "resume": report.resume,
            "resume_mode": report.resume_mode(),
            "totals": import_totals_json(&report.totals),
            "sources": report.sources.clone(),
        }),
        None => json!({
            "ran": false,
            "reason": "catalog_only",
        }),
    }
}

fn print_setup_status_line(
    report: Option<&ImportReport>,
    catalog_only: bool,
    pending_catalog_sessions: usize,
    indexed_items: usize,
) {
    if catalog_only {
        if pending_catalog_sessions > 0 {
            println!("ctx catalog is ready; import is still pending");
        } else {
            println!("ctx catalog is ready");
        }
        return;
    }
    let Some(report) = report else {
        println!("ctx is initialized; no local history was indexed");
        return;
    };
    if setup_has_indexed_content(indexed_items) && report.totals.failed_sources > 0 {
        println!("ctx indexed available local agent history; some sources were skipped");
    } else if setup_has_indexed_content(indexed_items) {
        println!("ctx local agent history search is ready");
    } else {
        println!("ctx is initialized; no local history was indexed");
    }
}

fn setup_has_indexed_content(indexed_items: usize) -> bool {
    indexed_items > 0
}

fn indexed_history_item_count(store: &Store) -> Result<usize> {
    let sessions = store.list_sessions()?;
    let mut count = sessions.len();
    for session in sessions {
        count = count.saturating_add(store.events_for_session(session.id)?.len());
    }
    Ok(count)
}

fn setup_has_failed_sources(report: Option<&ImportReport>) -> bool {
    report.is_some_and(|report| report.totals.failed_sources > 0)
}

fn run_status(
    args: JsonArgs,
    data_root: PathBuf,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    let db_path = database_path(data_root.clone());
    let initialized = db_path.exists();
    let config_path = data_root.join(CONFIG_FILE);
    let (records, sources, catalog_counts) = if initialized {
        let store = Store::open(&db_path)?;
        (
            indexed_history_item_count(&store)?,
            store.list_capture_sources()?.len(),
            store.catalog_session_counts()?,
        )
    } else {
        (0, 0, Default::default())
    };
    analytics::insert_bool(analytics_properties, "initialized", initialized);
    analytics::insert_count_bucket(analytics_properties, "indexed_items_bucket", records as u64);
    analytics::insert_count_bucket(
        analytics_properties,
        "indexed_sources_bucket",
        sources as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "cataloged_sessions_bucket",
        catalog_counts.total as u64,
    );

    if args.json {
        print_json(json!({
            "schema_version": 1,
            "initialized": initialized,
            "data_root": data_root,
            "database_path": db_path,
            "config_path": config_path,
            "indexed_items": records,
            "indexed_sources": sources,
            "cataloged_sessions": catalog_counts.total,
            "indexed_catalog_sessions": catalog_counts.indexed,
            "pending_catalog_sessions": catalog_counts.pending,
            "failed_catalog_sessions": catalog_counts.failed,
            "stale_catalog_sessions": catalog_counts.stale,
            "local_only": true,
        }))?;
    } else {
        println!("data_root: {}", data_root.display());
        println!("database_path: {}", db_path.display());
        println!("config_path: {}", config_path.display());
        println!("initialized: {initialized}");
        println!("indexed_items: {records}");
        println!("indexed_sources: {sources}");
        println!("cataloged_sessions: {}", catalog_counts.total);
        println!("indexed_catalog_sessions: {}", catalog_counts.indexed);
        println!("pending_catalog_sessions: {}", catalog_counts.pending);
        println!("failed_catalog_sessions: {}", catalog_counts.failed);
        println!("stale_catalog_sessions: {}", catalog_counts.stale);
        println!("local_only: true");
    }
    Ok(())
}

fn run_sources(args: JsonArgs, analytics_properties: &mut AnalyticsProperties) -> Result<()> {
    let sources = discovered_sources();
    let existing = sources.iter().filter(|source| source.exists).count();
    let importable = sources
        .iter()
        .filter(|source| {
            source.exists
                && matches!(source.import_support, ProviderImportSupport::Native)
                && source.status == ProviderSourceStatus::Available
        })
        .count();
    analytics::insert_count_bucket(
        analytics_properties,
        "providers_detected_bucket",
        sources.len() as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "providers_existing_bucket",
        existing as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "providers_importable_bucket",
        importable as u64,
    );
    if args.json {
        print_json(json!({
            "schema_version": 1,
            "sources": sources_json(&sources),
        }))?;
    } else {
        for source in sources {
            println!(
                "{} {} {} ({})",
                source.provider.as_str(),
                source.path.display(),
                source.status.as_str(),
                source.source_format
            );
        }
    }
    Ok(())
}

fn catalog_available_sources(
    store: &Store,
    sources: &[SourceInfo],
) -> Result<(CatalogTotals, Vec<Value>)> {
    let mut totals = CatalogTotals::default();
    let mut catalog_sources = Vec::new();
    for source in sources {
        if source.provider != CaptureProvider::Codex
            || source.source_format != "codex_session_jsonl_tree"
            || !source.exists
            || source.status != ProviderSourceStatus::Available
        {
            continue;
        }
        let summary = catalog_codex_session_tree(
            &source.path,
            store,
            CodexSessionCatalogOptions {
                source_root: Some(source.path.clone()),
                allow_partial_failures: true,
                ..CodexSessionCatalogOptions::default()
            },
        )
        .with_context(|| format!("catalog Codex sessions from {}", source.path.display()))?;
        totals.add(&summary);
        catalog_sources.push(json!({
            "provider": source.provider.as_str(),
            "path": source.path,
            "source_format": source.source_format,
            "source_files": summary.source_files,
            "source_bytes": summary.source_bytes,
            "cataloged_sessions": summary.cataloged_sessions,
            "cached_sessions": summary.cached_sessions,
            "parsed_sessions": summary.parsed_sessions,
            "skipped_sessions": summary.skipped_sessions,
            "failed_sessions": summary.failed_sessions,
        }));
    }
    Ok((totals, catalog_sources))
}

fn run_import(
    args: ImportArgs,
    data_root: PathBuf,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    let json = args.json;
    let progress = args.progress;
    let report = run_import_internal(
        &args,
        data_root,
        analytics_properties,
        ImportRunOptions {
            progress,
            json,
            print_human: !json,
            allow_empty_sources: false,
            operation: "import",
        },
    )?;
    print_import_report(&report, json)
}

fn run_import_internal(
    args: &ImportArgs,
    data_root: PathBuf,
    analytics_properties: &mut AnalyticsProperties,
    options: ImportRunOptions,
) -> Result<ImportReport> {
    fs::create_dir_all(&data_root)?;
    config::write_default_config(&data_root)?;
    let db_path = database_path(data_root);
    let mut store = Store::open(&db_path)?;
    let mut totals = ImportTotals::default();
    let mut imported_sources = Vec::new();

    let requests = import_requests(args)?;
    if requests.is_empty() {
        if options.allow_empty_sources {
            return Ok(ImportReport::empty(args.resume));
        }
        return Err(anyhow!(
            "no importable provider history sources found; use --path or run `ctx sources`"
        ));
    }

    let mut planned_sources = Vec::new();
    let mut planned_total_bytes = 0u64;
    for source in requests {
        let stats = source_stats(&source.path)
            .with_context(|| format!("scan import source {}", source.path.display()))?;
        planned_total_bytes = planned_total_bytes.saturating_add(stats.bytes);
        planned_sources.push((source, stats));
    }
    analytics::insert_count_bucket(
        analytics_properties,
        "sources_seen_bucket",
        planned_sources.len() as u64,
    );
    analytics::insert_bytes_bucket(
        analytics_properties,
        "source_bytes_bucket",
        planned_total_bytes,
    );

    let progress = ProgressReporter::new(
        options.progress,
        options.json,
        options.operation,
        planned_total_bytes,
    );
    let allow_source_failures = args.all && args.path.is_none();
    progress.message(
        "discovering",
        format!(
            "found {} import source(s), {}",
            planned_sources.len(),
            format_bytes(planned_total_bytes)
        ),
    );
    if let Some(warning) = low_disk_space_warning(&db_path, planned_total_bytes) {
        progress.warning(warning);
    }
    if let Some(warning) = large_import_warning(&planned_sources, planned_total_bytes) {
        progress.warning(warning);
    }

    if should_parallelize_import(&planned_sources) {
        let final_refresh_required = store.event_search_projection_needs_backfill()?
            || planned_sources
                .iter()
                .any(|(source, _)| !source_uses_incremental_event_search(source));
        drop(store);

        if options.print_human {
            progress.finish_line();
            println!("sources:");
            for (source, stats) in &planned_sources {
                println!(
                    "  {} {} ({} files, {})",
                    source.provider.as_str(),
                    source.path.display(),
                    stats.files,
                    format_bytes(stats.bytes)
                );
            }
        }

        let source_states = Arc::new(Mutex::new(
            planned_sources
                .iter()
                .map(|(_, stats)| SourceProgressSnapshot {
                    completed_bytes: 0,
                    total_bytes: stats.bytes,
                })
                .collect::<Vec<_>>(),
        ));
        let handles = planned_sources
            .into_iter()
            .enumerate()
            .map(|(index, (source, stats))| {
                let db_path = db_path.clone();
                let progress_callback = progress.parallel_codex_import_callback(
                    &source,
                    index,
                    Arc::clone(&source_states),
                );
                let full_rescan = args.resume;
                let join_source = source.clone();
                let join_stats = stats;
                let handle = thread::spawn(move || -> ImportSourceRun {
                    let result = (|| -> Result<ProviderImportSummary> {
                        let mut store = Store::open(&db_path)?;
                        import_one_source_without_search_refresh(
                            &mut store,
                            &source,
                            progress_callback,
                            full_rescan,
                        )
                        .with_context(|| {
                            format!(
                                "import {} source {}",
                                source.provider.as_str(),
                                source.path.display()
                            )
                        })
                    })();
                    match result {
                        Ok(summary) => ImportSourceRun::Imported(ImportSourceOutcome {
                            index,
                            source,
                            stats,
                            summary,
                        }),
                        Err(err) => {
                            let error = error_summary(&err);
                            ImportSourceRun::Failed(ImportSourceFailure {
                                index,
                                source,
                                stats,
                                error,
                            })
                        }
                    }
                });
                (index, join_source, join_stats, handle)
            })
            .collect::<Vec<_>>();

        let mut runs = Vec::with_capacity(handles.len());
        let mut first_error = None;
        for (index, source, stats, handle) in handles {
            match handle.join() {
                Ok(ImportSourceRun::Imported(outcome)) => {
                    runs.push(ImportSourceRun::Imported(outcome))
                }
                Ok(ImportSourceRun::Failed(failure)) => {
                    if !allow_source_failures || import_error_is_systemic(&failure.error) {
                        first_error.get_or_insert_with(|| {
                            anyhow!(
                                "import {} source {}: {}",
                                failure.source.provider.as_str(),
                                failure.source.path.display(),
                                failure.error
                            )
                        });
                    }
                    runs.push(ImportSourceRun::Failed(failure));
                }
                Err(_) => {
                    let failure = ImportSourceFailure {
                        index,
                        source,
                        stats,
                        error: "provider import worker panicked".to_owned(),
                    };
                    if !allow_source_failures {
                        first_error.get_or_insert_with(|| anyhow!("{}", failure.error));
                    }
                    runs.push(ImportSourceRun::Failed(failure));
                }
            }
        }
        if let Some(err) = first_error {
            return Err(err);
        }

        runs.sort_by_key(ImportSourceRun::index);
        for run in runs {
            match run {
                ImportSourceRun::Imported(outcome) => {
                    totals.add(&outcome.summary, &outcome.stats);
                    progress.parallel_source_done(
                        &outcome.source,
                        outcome.index,
                        &source_states,
                        outcome.stats,
                        &outcome.summary,
                    );
                    if options.print_human {
                        progress.finish_line();
                        print_source_imported(&outcome.source, &outcome.summary);
                    }
                    imported_sources.push(source_import_json(
                        &outcome.source,
                        &outcome.stats,
                        &outcome.summary,
                    ));
                }
                ImportSourceRun::Failed(failure) => {
                    totals.add_source_failure(&failure.stats);
                    progress.parallel_source_failed(
                        &failure.source,
                        failure.index,
                        &source_states,
                        failure.stats,
                        &failure.error,
                    );
                    if options.print_human {
                        progress.finish_line();
                        print_source_failed(&failure);
                    }
                    imported_sources.push(source_failure_json(&failure));
                }
            }
        }

        if final_refresh_required {
            progress.message("finalizing", "refreshing search index");
            let store = Store::open(&db_path)?;
            store.refresh_search_index()?;
        }
    } else {
        let mut completed_source_bytes = 0u64;
        for (source, stats) in planned_sources {
            if options.print_human {
                progress.finish_line();
                println!(
                    "importing {} {} ({} files, {})",
                    source.provider.as_str(),
                    source.path.display(),
                    stats.files,
                    format_bytes(stats.bytes)
                );
            }
            let source_progress = progress.codex_import_callback(&source, completed_source_bytes);
            completed_source_bytes = completed_source_bytes.saturating_add(stats.bytes);
            match import_one_source(&mut store, &source, source_progress, args.resume) {
                Ok(summary) => {
                    totals.add(&summary, &stats);
                    progress.done(
                        "indexing",
                        format!("imported {}", source.provider.as_str()),
                        completed_source_bytes,
                    );
                    if options.print_human {
                        progress.finish_line();
                        print_source_imported(&source, &summary);
                    }
                    imported_sources.push(source_import_json(&source, &stats, &summary));
                }
                Err(err) => {
                    let error = error_summary(&err);
                    if allow_source_failures && !import_error_is_systemic(&error) {
                        let failure = ImportSourceFailure {
                            index: imported_sources.len(),
                            source,
                            stats,
                            error,
                        };
                        totals.add_source_failure(&failure.stats);
                        progress.done(
                            "indexing",
                            format!(
                                "skipped {}: {}",
                                failure.source.provider.as_str(),
                                source_error_reason(&failure.source, &failure.error)
                            ),
                            completed_source_bytes,
                        );
                        if options.print_human {
                            progress.finish_line();
                            print_source_failed(&failure);
                        }
                        imported_sources.push(source_failure_json(&failure));
                    } else {
                        return Err(err);
                    }
                }
            }
        }
    }

    if totals.imported_sessions > 0 || totals.imported_events > 0 || totals.imported_edges > 0 {
        progress.message("finalizing", "optimizing search index");
        Store::open(&db_path)?.optimize_search_index()?;
    }

    progress.message("finalizing", "checkpointing search database");
    Store::open(&db_path)?.checkpoint_wal_truncate_if_larger_than(WAL_TRUNCATE_MIN_BYTES)?;

    if options.print_human {
        progress.finish_line();
    }
    progress.done(
        "finalizing",
        format!("indexed {} source file(s)", totals.source_files),
        totals.source_bytes,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "source_files_bucket",
        totals.source_files as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "failed_sources_bucket",
        totals.failed_sources as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "sessions_imported_bucket",
        totals.imported_sessions as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "events_imported_bucket",
        totals.imported_events as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "edges_imported_bucket",
        totals.imported_edges as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "skipped_bucket",
        totals.skipped as u64,
    );
    analytics::insert_count_bucket(analytics_properties, "failed_bucket", totals.failed as u64);
    if totals.imported_sources == 0 && totals.failed_sources > 0 {
        return Err(anyhow!("all import sources failed"));
    }
    Ok(ImportReport {
        resume: args.resume,
        totals,
        sources: imported_sources,
    })
}

fn print_import_report(report: &ImportReport, json_output: bool) -> Result<()> {
    if json_output {
        print_json(import_report_json(report))
    } else {
        print_import_report_human(report);
        Ok(())
    }
}

fn import_report_json(report: &ImportReport) -> Value {
    json!({
        "schema_version": 1,
        "resume": report.resume,
        "resume_mode": report.resume_mode(),
        "totals": import_totals_json(&report.totals),
        "sources": report.sources.clone(),
    })
}

fn import_totals_json(totals: &ImportTotals) -> Value {
    json!({
        "source_files": totals.source_files,
        "source_bytes": totals.source_bytes,
        "imported_sources": totals.imported_sources,
        "failed_sources": totals.failed_sources,
        "imported_sessions": totals.imported_sessions,
        "imported_events": totals.imported_events,
        "imported_edges": totals.imported_edges,
        "skipped": totals.skipped,
        "failed": totals.failed,
    })
}

fn print_import_report_human(report: &ImportReport) {
    println!("source_files: {}", report.totals.source_files);
    println!("source_bytes: {}", report.totals.source_bytes);
    println!("imported_sources: {}", report.totals.imported_sources);
    println!("failed_sources: {}", report.totals.failed_sources);
    println!("imported_sessions: {}", report.totals.imported_sessions);
    println!("imported_events: {}", report.totals.imported_events);
    println!("imported_edges: {}", report.totals.imported_edges);
    println!("skipped: {}", report.totals.skipped);
    println!("failed: {}", report.totals.failed);
    println!("resume: {}", report.resume);
    println!("resume_mode: {}", report.resume_mode());
}

#[derive(Debug)]
struct ImportSourceOutcome {
    index: usize,
    source: SourceInfo,
    stats: SourceStats,
    summary: ProviderImportSummary,
}

#[derive(Debug)]
struct ImportSourceFailure {
    index: usize,
    source: SourceInfo,
    stats: SourceStats,
    error: String,
}

#[derive(Debug)]
enum ImportSourceRun {
    Imported(ImportSourceOutcome),
    Failed(ImportSourceFailure),
}

impl ImportSourceRun {
    fn index(&self) -> usize {
        match self {
            Self::Imported(outcome) => outcome.index,
            Self::Failed(failure) => failure.index,
        }
    }
}

fn should_parallelize_import(planned_sources: &[(SourceInfo, SourceStats)]) -> bool {
    let _ = planned_sources;
    false
}

fn large_import_warning(
    planned_sources: &[(SourceInfo, SourceStats)],
    planned_total_bytes: u64,
) -> Option<String> {
    let planned_total_files = planned_sources
        .iter()
        .map(|(_, stats)| stats.files)
        .sum::<usize>();
    if planned_total_files < LARGE_IMPORT_SOURCE_FILES_WARNING
        && planned_total_bytes < LARGE_IMPORT_SOURCE_BYTES_WARNING
    {
        return None;
    }
    Some(format!(
        "large import: {} source file(s), {}; initial indexing may use sustained CPU and disk",
        planned_total_files,
        format_bytes(planned_total_bytes)
    ))
}

fn source_import_json(
    source: &SourceInfo,
    stats: &SourceStats,
    summary: &ProviderImportSummary,
) -> Value {
    json!({
        "status": "imported",
        "provider": source.provider.as_str(),
        "path": source.path,
        "source_format": source.source_format,
        "source_files": stats.files,
        "source_bytes": stats.bytes,
        "imported_sessions": summary.imported_sessions,
        "imported_events": summary.imported_events,
        "imported_edges": summary.imported_edges,
        "skipped": summary.skipped,
        "failed": summary.failed,
        "failures": provider_failures_json(summary),
    })
}

fn provider_failures_json(summary: &ProviderImportSummary) -> Vec<Value> {
    summary
        .failures
        .iter()
        .take(5)
        .map(|failure| {
            json!({
                "line": failure.line,
                "error": failure.error,
            })
        })
        .collect()
}

fn source_failure_json(failure: &ImportSourceFailure) -> Value {
    json!({
        "status": "failed",
        "provider": failure.source.provider.as_str(),
        "path": failure.source.path,
        "source_format": failure.source.source_format,
        "source_files": failure.stats.files,
        "source_bytes": failure.stats.bytes,
        "error": source_error_reason(&failure.source, &failure.error),
    })
}

fn print_source_imported(source: &SourceInfo, summary: &ProviderImportSummary) {
    println!(
        "imported {}: sessions={} events={} edges={} skipped={} failed={}",
        source.provider.as_str(),
        summary.imported_sessions,
        summary.imported_events,
        summary.imported_edges,
        summary.skipped,
        summary.failed
    );
}

fn print_source_failed(failure: &ImportSourceFailure) {
    println!(
        "skipped {}: {}",
        failure.source.provider.as_str(),
        source_error_reason(&failure.source, &failure.error)
    );
    println!("  path: {}", failure.source.path.display());
}

fn source_error_reason(source: &SourceInfo, error: &str) -> String {
    let error = one_line_error(error);
    let prefix = format!(
        "import {} source {}: ",
        source.provider.as_str(),
        source.path.display()
    );
    error.strip_prefix(&prefix).unwrap_or(&error).to_owned()
}

fn one_line_error(error: &str) -> String {
    error
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("unknown error")
        .to_owned()
}

fn error_summary(error: &anyhow::Error) -> String {
    let top = error.to_string();
    let root = error
        .chain()
        .last()
        .map(ToString::to_string)
        .unwrap_or_else(|| top.clone());
    if is_sqlite_busy_text(&top) || is_sqlite_busy_text(&root) {
        return "ctx index is busy because another ctx import or search refresh is writing to the local database; retry in a moment, or use `ctx search --refresh off` to search the existing index".to_owned();
    }
    if root == top || top.contains(&root) {
        top
    } else {
        format!("{top}: {root}")
    }
}

fn is_sqlite_busy_text(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("database is locked") || lower.contains("database table is locked")
}

fn import_error_is_systemic(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("database or disk is full")
        || lower.contains("ctx index is busy")
        || lower.contains("database is locked")
        || lower.contains("readonly database")
        || lower.contains("disk i/o error")
        || lower.contains("out of memory")
}

fn low_disk_space_warning(db_path: &Path, planned_total_bytes: u64) -> Option<String> {
    let parent = db_path.parent().unwrap_or_else(|| Path::new("."));
    let available = available_space_bytes(parent)?;
    let recommended = (planned_total_bytes / 4).clamp(1 << 30, 20 * (1 << 30));
    if available < recommended {
        Some(format!(
            "low disk space: {} available near {}, {} recommended before indexing {}",
            format_bytes(available),
            parent.display(),
            format_bytes(recommended),
            format_bytes(planned_total_bytes)
        ))
    } else {
        None
    }
}

#[cfg(unix)]
fn available_space_bytes(path: &Path) -> Option<u64> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};

    fn statvfs_field_to_u64<T>(value: T) -> Option<u64>
    where
        T: TryInto<u64>,
    {
        value.try_into().ok()
    }

    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    let rc = unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    let stat = unsafe { stat.assume_init() };
    let available_blocks = statvfs_field_to_u64(stat.f_bavail)?;
    let fragment_size = statvfs_field_to_u64(stat.f_frsize)?;
    Some(available_blocks.saturating_mul(fragment_size))
}

#[cfg(not(unix))]
fn available_space_bytes(_path: &Path) -> Option<u64> {
    None
}

fn run_list(
    args: ListArgs,
    data_root: PathBuf,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    let store = Store::open(database_path(data_root))?;
    let sessions = store
        .list_sessions()?
        .into_iter()
        .take(args.limit)
        .collect::<Vec<_>>();
    analytics::insert_count_bucket(
        analytics_properties,
        "items_returned_bucket",
        sessions.len() as u64,
    );
    if args.json {
        let items = sessions
            .iter()
            .map(ListItemDto::session)
            .collect::<Vec<_>>();
        print_json(json!({
            "schema_version": 1,
            "items": items,
        }))?;
    } else {
        for session in sessions {
            println!(
                "{} session {}",
                session.id,
                session
                    .external_session_id
                    .unwrap_or_else(|| session.provider.to_string())
            );
        }
    }
    Ok(())
}

fn run_show(
    args: ShowArgs,
    data_root: PathBuf,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    let store = Store::open(database_path(data_root))?;
    match args.target {
        ShowTarget::Session(args) => {
            let session = resolve_session(
                &store,
                args.id,
                args.provider.map(ProviderArg::capture_provider),
                args.provider_session.as_deref(),
            )?;
            let events = store.events_for_session(session.id)?;
            analytics::insert_count_bucket(
                analytics_properties,
                "events_returned_bucket",
                events.len() as u64,
            );
            let format = effective_format(args.format, args.json);
            write_rendered_session(&store, &session, &events, args.mode, format, None)?;
        }
        ShowTarget::Event(args) => {
            let event = store.get_event(args.id)?;
            let events = event_window(&store, &event, args.before, args.after, args.window)?;
            analytics::insert_count_bucket(
                analytics_properties,
                "events_returned_bucket",
                events.len() as u64,
            );
            let format = effective_format(args.format, args.json);
            write_rendered_events(&store, &event, &events, format, None)?;
        }
    }
    Ok(())
}

fn event_preview(event: &Event) -> String {
    let preview = ctx_history_search::event_preview_text(event);
    if preview.trim().is_empty() {
        format!("{} event", event.event_type.as_str())
    } else {
        ctx_history_search::redacted_snippet(&preview, 120)
    }
}

fn run_locate(
    args: LocateArgs,
    data_root: PathBuf,
    _analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    let store = Store::open(database_path(data_root))?;
    match args.target {
        LocateTarget::Session(args) => {
            let session = resolve_session(
                &store,
                args.id,
                args.provider.map(ProviderArg::capture_provider),
                args.provider_session.as_deref(),
            )?;
            let value = locate_session_json(&store, &session);
            if locate_json_output(args.format, args.json) {
                print_json(value)?;
            } else {
                print_locate_session_text(&value)?;
            }
        }
        LocateTarget::Event(args) => {
            let event = store.get_event(args.id)?;
            let value = locate_event_json(&store, &event);
            if locate_json_output(args.format, args.json) {
                print_json(value)?;
            } else {
                print_locate_event_text(&value)?;
            }
        }
    }
    Ok(())
}

fn run_export(
    args: ExportArgs,
    data_root: PathBuf,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    let store = Store::open(database_path(data_root))?;
    match args.target {
        ExportTarget::Session(args) => {
            let session = resolve_session(
                &store,
                args.id,
                args.provider.map(ProviderArg::capture_provider),
                args.provider_session.as_deref(),
            )?;
            let events = store.events_for_session(session.id)?;
            analytics::insert_count_bucket(
                analytics_properties,
                "events_returned_bucket",
                events.len() as u64,
            );
            write_rendered_session(&store, &session, &events, args.mode, args.format, args.out)?;
        }
    }
    Ok(())
}

fn effective_format(format: OutputFormat, json: bool) -> OutputFormat {
    if json {
        OutputFormat::Json
    } else {
        format
    }
}

fn locate_json_output(format: LocateFormat, json: bool) -> bool {
    json || format == LocateFormat::Json
}

fn resolve_session(
    store: &Store,
    id: Option<Uuid>,
    provider: Option<CaptureProvider>,
    provider_session: Option<&str>,
) -> Result<Session> {
    if let Some(id) = id {
        return store.get_session(id).with_context(|| {
            format!("session {id} was not found; use `ctx search --json` to get ctx_session_id")
        });
    }
    let provider = provider.ok_or_else(|| {
        anyhow!(
            "session lookup requires either a ctx session id or --provider with --provider-session"
        )
    })?;
    let provider_session = provider_session
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow!("session lookup requires --provider-session when no ctx session id is provided")
        })?;
    let matches = store
        .list_sessions()?
        .into_iter()
        .filter(|session| {
            session.provider == provider
                && session.external_session_id.as_deref() == Some(provider_session)
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [session] => Ok(session.clone()),
        [] => Err(anyhow!(
            "no {provider} session with provider_session_id {provider_session:?} is indexed"
        )),
        _ => Err(anyhow!(
            "multiple {provider} sessions with provider_session_id {provider_session:?} are indexed; use ctx_session_id"
        )),
    }
}

fn event_window(
    store: &Store,
    event: &Event,
    before: usize,
    after: usize,
    window: Option<usize>,
) -> Result<Vec<Event>> {
    let Some(session_id) = event.session_id else {
        return Ok(vec![event.clone()]);
    };
    let events = store.events_for_session(session_id)?;
    let Some(index) = events.iter().position(|candidate| candidate.id == event.id) else {
        return Ok(vec![event.clone()]);
    };
    let (before, after) = window
        .map(|window| (window, window))
        .unwrap_or((before, after));
    let start = index.saturating_sub(before);
    let end = (index + after + 1).min(events.len());
    Ok(events[start..end].to_vec())
}

fn write_rendered_session(
    store: &Store,
    session: &Session,
    events: &[Event],
    mode: TranscriptMode,
    format: OutputFormat,
    out: Option<PathBuf>,
) -> Result<()> {
    let body = match format {
        OutputFormat::Text => render_session_text(store, session, events, mode),
        OutputFormat::Markdown => render_session_markdown(store, session, events, mode),
        OutputFormat::Json => serde_json::to_string_pretty(&session_transcript_json(
            store, session, events, mode, format,
        ))?,
        OutputFormat::Jsonl => render_session_jsonl(store, session, events, mode)?,
    };
    write_output(body, out)
}

fn write_rendered_events(
    store: &Store,
    selected: &Event,
    events: &[Event],
    format: OutputFormat,
    out: Option<PathBuf>,
) -> Result<()> {
    let body = match format {
        OutputFormat::Text => render_events_text(store, selected, events),
        OutputFormat::Markdown => render_events_markdown(store, selected, events),
        OutputFormat::Json => {
            serde_json::to_string_pretty(&event_window_json(store, selected, events, format))?
        }
        OutputFormat::Jsonl => render_events_jsonl(store, events)?,
    };
    write_output(body, out)
}

fn write_output(body: String, out: Option<PathBuf>) -> Result<()> {
    if let Some(out) = out {
        if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        fs::write(&out, body).with_context(|| format!("write {}", out.display()))?;
    } else {
        print!("{body}");
        if !body.ends_with('\n') {
            println!();
        }
    }
    Ok(())
}

fn selected_transcript_events(events: &[Event], mode: TranscriptMode) -> Vec<&Event> {
    match mode {
        TranscriptMode::Log => events.iter().collect(),
        TranscriptMode::Full => events.iter().filter(|event| is_message(event)).collect(),
        TranscriptMode::Lite => lite_transcript_events(events),
    }
}

fn lite_transcript_events(events: &[Event]) -> Vec<&Event> {
    let mut selected = Vec::new();
    let mut pending_assistant: Option<&Event> = None;
    for event in events {
        if is_user_message(event) {
            if let Some(assistant) = pending_assistant.take() {
                selected.push(assistant);
            }
            selected.push(event);
        } else if is_assistant_message(event) {
            pending_assistant = Some(event);
        }
    }
    if let Some(assistant) = pending_assistant {
        selected.push(assistant);
    }
    selected
}

fn is_message(event: &Event) -> bool {
    event.event_type == EventType::Message
        && matches!(
            event.role,
            Some(EventRole::User | EventRole::Assistant | EventRole::System)
        )
}

fn is_user_message(event: &Event) -> bool {
    event.event_type == EventType::Message && event.role == Some(EventRole::User)
}

fn is_assistant_message(event: &Event) -> bool {
    event.event_type == EventType::Message && event.role == Some(EventRole::Assistant)
}

fn event_content(event: &Event) -> String {
    if matches!(
        event.redaction_state,
        RedactionState::Raw | RedactionState::Withheld
    ) {
        return "raw event payload withheld".to_owned();
    }
    if let Some(value) = event.payload.get("body").and_then(event_value_text) {
        return ctx_history_search::redacted_snippet(&value, 16_000);
    }
    if let Some(value) = event_value_text(&event.payload) {
        return ctx_history_search::redacted_snippet(&value, 16_000);
    }
    let preview = ctx_history_search::event_preview_text(event);
    if preview.trim().is_empty() {
        format!("{} event", event.event_type.as_str())
    } else {
        ctx_history_search::redacted_snippet(&preview, 16_000)
    }
}

fn event_value_text(value: &Value) -> Option<String> {
    if let Some(value) = value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(value.to_owned());
    }
    let object = value.as_object()?;
    for key in [
        "text",
        "preview",
        "summary",
        "command",
        "output_preview",
        "output",
        "message",
    ] {
        if let Some(value) = object
            .get(key)
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(value.to_owned());
        }
    }
    let structured = ["tool", "name", "arguments_preview", "status"]
        .into_iter()
        .filter_map(|key| object.get(key).and_then(|value| value.as_str()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if structured.is_empty() {
        None
    } else {
        Some(structured.join(" "))
    }
}

fn render_session_text(
    store: &Store,
    session: &Session,
    events: &[Event],
    mode: TranscriptMode,
) -> String {
    let mut out = String::new();
    push_session_header(&mut out, store, session, mode, OutputFormat::Text);
    for event in selected_transcript_events(events, mode) {
        push_event_text_block(&mut out, event);
    }
    out
}

fn render_session_markdown(
    store: &Store,
    session: &Session,
    events: &[Event],
    mode: TranscriptMode,
) -> String {
    let mut out = String::new();
    let label = session
        .external_session_id
        .clone()
        .unwrap_or_else(|| session.id.to_string());
    out.push_str(&format!("# {} session {}\n\n", session.provider, label));
    push_session_metadata_markdown(&mut out, store, session, mode, OutputFormat::Markdown);
    for event in selected_transcript_events(events, mode) {
        let heading = event
            .role
            .map(|role| role.as_str())
            .unwrap_or(event.event_type.as_str());
        out.push_str(&format!(
            "\n## {} - {} - {}\n\n",
            heading,
            event.event_type.as_str(),
            event.occurred_at
        ));
        out.push_str(&format!("ctx_event_id: `{}`\n\n", event.id));
        out.push_str(&event_content(event));
        out.push('\n');
    }
    out
}

fn push_session_header(
    out: &mut String,
    store: &Store,
    session: &Session,
    mode: TranscriptMode,
    format: OutputFormat,
) {
    out.push_str(&format!("ctx_session_id: {}\n", session.id));
    out.push_str(&format!("provider: {}\n", session.provider));
    if let Some(provider_session_id) = &session.external_session_id {
        out.push_str(&format!("provider_session_id: {provider_session_id}\n"));
    }
    out.push_str(&format!("mode: {}\n", mode.as_str()));
    out.push_str(&format!("format: {}\n", format.as_str()));
    if let Some(source) = source_json_for(store, session.capture_source_id) {
        if let Some(path) = source.get("path").and_then(|value| value.as_str()) {
            out.push_str(&format!("source_path: {path}\n"));
        }
    }
    out.push('\n');
}

fn push_session_metadata_markdown(
    out: &mut String,
    store: &Store,
    session: &Session,
    mode: TranscriptMode,
    format: OutputFormat,
) {
    out.push_str(&format!("- ctx_session_id: `{}`\n", session.id));
    out.push_str(&format!("- provider: `{}`\n", session.provider));
    if let Some(provider_session_id) = &session.external_session_id {
        out.push_str(&format!("- provider_session_id: `{provider_session_id}`\n"));
    }
    out.push_str(&format!("- mode: `{}`\n", mode.as_str()));
    out.push_str(&format!("- format: `{}`\n", format.as_str()));
    if let Some(source) = source_json_for(store, session.capture_source_id) {
        if let Some(path) = source.get("path").and_then(|value| value.as_str()) {
            out.push_str(&format!("- source_path: `{path}`\n"));
        }
    }
}

fn push_event_text_block(out: &mut String, event: &Event) {
    let role = event.role.map(|role| role.as_str()).unwrap_or("-");
    out.push_str(&format!(
        "[{}] {} {} {}\n",
        event.occurred_at,
        role,
        event.event_type.as_str(),
        event.id
    ));
    out.push_str(&event_content(event));
    out.push_str("\n\n");
}

fn render_events_text(store: &Store, selected: &Event, events: &[Event]) -> String {
    let mut out = String::new();
    out.push_str(&format!("ctx_event_id: {}\n", selected.id));
    if let Some(session_id) = selected.session_id {
        out.push_str(&format!("ctx_session_id: {session_id}\n"));
        if let Ok(session) = store.get_session(session_id) {
            out.push_str(&format!("provider: {}\n", session.provider));
            if let Some(provider_session_id) = session.external_session_id {
                out.push_str(&format!("provider_session_id: {provider_session_id}\n"));
            }
        }
    }
    out.push('\n');
    for event in events {
        push_event_text_block(&mut out, event);
    }
    out
}

fn render_events_markdown(store: &Store, selected: &Event, events: &[Event]) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Event {}\n\n", selected.id));
    if let Some(session_id) = selected.session_id {
        out.push_str(&format!("- ctx_session_id: `{session_id}`\n"));
        if let Ok(session) = store.get_session(session_id) {
            out.push_str(&format!("- provider: `{}`\n", session.provider));
            if let Some(provider_session_id) = session.external_session_id {
                out.push_str(&format!("- provider_session_id: `{provider_session_id}`\n"));
            }
        }
    }
    for event in events {
        let role = event.role.map(|role| role.as_str()).unwrap_or("-");
        out.push_str(&format!(
            "\n## {} - {} - {}\n\n",
            role,
            event.event_type.as_str(),
            event.occurred_at
        ));
        out.push_str(&format!("ctx_event_id: `{}`\n\n", event.id));
        out.push_str(&event_content(event));
        out.push('\n');
    }
    out
}

fn session_transcript_json(
    store: &Store,
    session: &Session,
    events: &[Event],
    mode: TranscriptMode,
    format: OutputFormat,
) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "target": "session",
        "item_type": "session_transcript",
        "ctx_session_id": session.id,
        "provider": session.provider,
        "provider_session_id": session.external_session_id,
        "mode": mode.as_str(),
        "format": format.as_str(),
        "session": ShowDto::session(store, session),
        "source": source_json_for(store, session.capture_source_id),
        "events": selected_transcript_events(events, mode)
            .into_iter()
            .map(|event| transcript_event_json(store, event))
            .collect::<Vec<_>>(),
    }))
}

fn event_window_json(
    store: &Store,
    selected: &Event,
    events: &[Event],
    format: OutputFormat,
) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "target": "event",
        "item_type": "event_window",
        "ctx_event_id": selected.id,
        "ctx_session_id": selected.session_id,
        "format": format.as_str(),
        "event": transcript_event_json(store, selected),
        "events": events
            .iter()
            .map(|event| transcript_event_json(store, event))
            .collect::<Vec<_>>(),
    }))
}

fn transcript_event_json(store: &Store, event: &Event) -> Value {
    let session = event.session_id.and_then(|id| store.get_session(id).ok());
    compact_json(json!({
        "ctx_event_id": event.id,
        "item_id": event.id,
        "item_type": "event",
        "ctx_session_id": event.session_id,
        "provider": session.as_ref().map(|session| session.provider),
        "provider_session_id": session
            .as_ref()
            .and_then(|session| session.external_session_id.clone()),
        "sequence": event.seq,
        "event_type": event.event_type,
        "role": event.role,
        "occurred_at": event.occurred_at,
        "source_id": event.capture_source_id,
        "source_path": source_path_for(store, event.capture_source_id),
        "source_exists": source_path_exists(source_path_for(store, event.capture_source_id).as_deref()),
        "source": source_json_for(store, event.capture_source_id),
        "cursor": event_cursor(event),
        "preview": event_preview(event),
        "text": event_content(event),
        "redaction_state": event.redaction_state,
    }))
}

fn render_session_jsonl(
    store: &Store,
    session: &Session,
    events: &[Event],
    mode: TranscriptMode,
) -> Result<String> {
    let mut lines = Vec::new();
    for event in selected_transcript_events(events, mode) {
        lines.push(serde_json::to_string(&compact_json(json!({
            "schema_version": 1,
            "item_type": "session_transcript_event",
            "mode": mode.as_str(),
            "ctx_session_id": session.id,
            "provider": session.provider,
            "provider_session_id": session.external_session_id,
            "event": transcript_event_json(store, event),
        })))?);
    }
    Ok(lines.join("\n") + "\n")
}

fn render_events_jsonl(store: &Store, events: &[Event]) -> Result<String> {
    let mut lines = Vec::new();
    for event in events {
        lines.push(serde_json::to_string(&transcript_event_json(store, event))?);
    }
    Ok(lines.join("\n") + "\n")
}

fn locate_session_json(store: &Store, session: &Session) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "target": "session",
        "item_type": "session_location",
        "ctx_session_id": session.id,
        "provider": session.provider,
        "provider_session_id": session.external_session_id,
        "parent_ctx_session_id": session.parent_session_id,
        "root_ctx_session_id": session.root_session_id,
        "agent_type": session.agent_type,
        "role": session.role_hint,
        "status": session.status,
        "started_at": session.started_at,
        "ended_at": session.ended_at,
        "source": source_json_for(store, session.capture_source_id),
        "resume": provider_resume_json(session.provider, session.external_session_id.as_deref()),
    }))
}

fn locate_event_json(store: &Store, event: &Event) -> Value {
    let session = event.session_id.and_then(|id| store.get_session(id).ok());
    compact_json(json!({
        "schema_version": 1,
        "target": "event",
        "item_type": "event_location",
        "ctx_event_id": event.id,
        "ctx_session_id": event.session_id,
        "provider": session.as_ref().map(|session| session.provider),
        "provider_session_id": session
            .as_ref()
            .and_then(|session| session.external_session_id.clone()),
        "sequence": event.seq,
        "event_type": event.event_type,
        "role": event.role,
        "occurred_at": event.occurred_at,
        "source": source_json_for(store, event.capture_source_id),
        "cursor": event_cursor(event),
        "resume": session
            .as_ref()
            .map(|session| provider_resume_json(session.provider, session.external_session_id.as_deref())),
    }))
}

fn source_json_for(store: &Store, source_id: Option<Uuid>) -> Option<Value> {
    let source = source_id.and_then(|source_id| store.get_capture_source(source_id).ok())?;
    let path = source.descriptor.raw_source_path.clone();
    Some(compact_json(json!({
        "source_id": source.id,
        "provider": source.descriptor.provider,
        "provider_session_id": source.descriptor.external_session_id,
        "path": path,
        "exists": source_path_exists(path.as_deref()),
        "cwd": source.descriptor.cwd,
        "started_at": source.started_at,
        "ended_at": source.ended_at,
        "source_format": source_format(&source.sync.metadata),
        "cursor": source_cursor(&source.sync.metadata),
    })))
}

fn source_format(metadata: &Value) -> Option<String> {
    for pointer in [
        "/source_format",
        "/format",
        "/provider/source_format",
        "/source/source_format",
    ] {
        if let Some(value) = metadata.pointer(pointer).and_then(|value| value.as_str()) {
            return Some(value.to_owned());
        }
    }
    None
}

fn source_cursor(metadata: &Value) -> Option<String> {
    metadata
        .pointer("/cursor/after/cursor")
        .and_then(|value| value.as_str())
        .or_else(|| metadata.pointer("/cursor").and_then(|value| value.as_str()))
        .map(str::to_owned)
}

fn provider_resume_json(provider: CaptureProvider, provider_session_id: Option<&str>) -> Value {
    let (command, argv) = match (provider, provider_session_id) {
        (CaptureProvider::Codex, Some(session_id)) => (
            Some(format!("codex resume {}", shell_quote_arg(session_id))),
            Some(vec![
                "codex".to_owned(),
                "resume".to_owned(),
                session_id.to_owned(),
            ]),
        ),
        _ => (None, None),
    };
    compact_json(json!({
        "available": command.is_some(),
        "command": command,
        "argv": argv,
    }))
}

fn shell_quote_arg(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '@'))
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn print_locate_session_text(value: &Value) -> Result<()> {
    println!(
        "ctx_session_id: {}",
        value["ctx_session_id"].as_str().unwrap_or("")
    );
    print_optional_json_str(value, "provider");
    print_optional_json_str(value, "provider_session_id");
    if let Some(source) = value.get("source") {
        print_optional_json_str(source, "path");
        print_optional_json_str(source, "source_format");
        if let Some(exists) = source.get("exists").and_then(|value| value.as_bool()) {
            println!("source_exists: {exists}");
        }
    }
    if let Some(command) = value
        .get("resume")
        .and_then(|resume| resume.get("command"))
        .and_then(|value| value.as_str())
    {
        println!("resume_command: {command}");
    }
    Ok(())
}

fn print_locate_event_text(value: &Value) -> Result<()> {
    println!(
        "ctx_event_id: {}",
        value["ctx_event_id"].as_str().unwrap_or("")
    );
    print_optional_json_str(value, "ctx_session_id");
    print_optional_json_str(value, "provider");
    print_optional_json_str(value, "provider_session_id");
    print_optional_json_str(value, "event_type");
    print_optional_json_str(value, "role");
    print_optional_json_str(value, "cursor");
    if let Some(source) = value.get("source") {
        print_optional_json_str(source, "path");
    }
    Ok(())
}

fn print_optional_json_str(value: &Value, key: &str) {
    if let Some(text) = value.get(key).and_then(|value| value.as_str()) {
        println!("{key}: {text}");
    }
}

impl ListItemDto {
    fn session(session: &Session) -> Value {
        compact_json(json!({
            "id": session.id,
            "item_id": session.id,
            "ctx_session_id": session.id,
            "item_type": "session",
            "provider": session.provider,
            "provider_session_id": session.external_session_id,
            "external_session_id": session.external_session_id,
            "agent_type": session.agent_type,
            "started_at": session.started_at,
            "ended_at": session.ended_at,
        }))
    }
}

impl ShowDto {
    fn session(store: &Store, session: &Session) -> Value {
        let source_path = source_path_for(store, session.capture_source_id);
        compact_json(json!({
            "id": session.id,
            "item_id": session.id,
            "item_type": "session",
            "provider": session.provider,
            "external_session_id": session.external_session_id,
            "agent_type": session.agent_type,
            "role": session.role_hint,
            "is_primary": session.is_primary,
            "status": session.status,
            "started_at": session.started_at,
            "ended_at": session.ended_at,
            "source_id": session.capture_source_id,
            "source_path": source_path,
            "source_exists": source_path_exists(source_path.as_deref()),
        }))
    }
}

impl SearchDto {
    fn packet(
        store: &Store,
        packet: &ctx_history_search::SearchPacket,
        refresh: &SearchRefreshReport,
    ) -> Value {
        compact_json(json!({
            "schema_version": packet.schema_version,
            "query": packet.query,
            "filters": packet.filters,
            "freshness": refresh.to_json(),
            "generated_at": packet.generated_at,
            "results": packet
                .results
                .iter()
                .map(|result| {
                    compact_json(json!({
                        "item_id": result.record_id,
                        "item_type": search_result_item_type(store, result),
                        "ctx_event_id": result.event_id,
                        "ctx_session_id": result.session_id,
                        "session_id": result.session_id,
                        "event_id": result.event_id,
                        "event_seq": result.event_seq,
                        "title": result.title,
                        "snippet": result.snippet,
                        "rank": result.rank,
                        "provider": result.provider,
                        "provider_session_id": result.provider_session_id,
                        "timestamp": result.timestamp,
                        "cwd": result.cwd,
                        "source_path": result.raw_source_path,
                        "source_exists": result.raw_source_exists,
                        "cursor": result.cursor,
                        "suggested_next_commands": search_next_commands(result),
                        "why_matched": result.why_matched,
                        "citations": public_citations(&result.citations),
                        "links": result.links,
                        "visibility": result.visibility,
                    }))
                })
                .collect::<Vec<_>>(),
            "pagination": packet.pagination,
            "truncation": packet.truncation,
        }))
    }
}

fn search_result_item_type(
    store: &Store,
    result: &ctx_history_search::SearchPacketResult,
) -> String {
    if result.event_id == Some(result.record_id) {
        return "event".to_owned();
    }
    if result.session_id == Some(result.record_id) {
        return "session".to_owned();
    }
    item_type_for_id(store, result.record_id)
}

fn search_next_commands(result: &ctx_history_search::SearchPacketResult) -> Vec<String> {
    let mut commands = Vec::new();
    if let Some(id) = result.event_id {
        commands.push(format!("ctx show event {id} --window 10"));
        commands.push(format!("ctx locate event {id}"));
    }
    if let Some(id) = result.session_id {
        commands.push(format!("ctx show session {id} --mode lite"));
        commands.push(format!("ctx locate session {id}"));
        commands.push(format!(
            "ctx export session {id} --mode full --format markdown --out /tmp/ctx-session-{id}.md"
        ));
    }
    commands
}

fn public_citations(citations: &[ContextCitation]) -> Vec<Value> {
    citations
        .iter()
        .map(|citation| {
            let ctx_event_id = if citation.citation_type == ContextCitationType::Event {
                Some(citation.id)
            } else {
                None
            };
            let ctx_session_id = if citation.citation_type == ContextCitationType::Session {
                Some(citation.id)
            } else {
                citation.session_id
            };
            compact_json(json!({
                "item_id": citation.id,
                "item_type": public_citation_item_type(citation.citation_type),
                "ctx_event_id": ctx_event_id,
                "ctx_session_id": ctx_session_id,
                "label": citation.label,
                "time": citation.time,
                "provider": citation.provider,
                "session_id": citation.session_id,
                "event_seq": citation.event_seq,
                "source_path": citation.raw_source_path,
                "source_exists": citation.raw_source_exists,
                "cursor": citation.cursor,
            }))
        })
        .collect()
}

fn public_citation_item_type(citation_type: ContextCitationType) -> &'static str {
    match citation_type {
        ContextCitationType::HistoryRecord => "indexed_item",
        ContextCitationType::Session => "session",
        ContextCitationType::Run => "run",
        ContextCitationType::Event => "event",
        ContextCitationType::VcsChange => "vcs_change",
        ContextCitationType::Artifact => "artifact",
        ContextCitationType::Summary => "summary",
        ContextCitationType::File => "file",
    }
}

fn public_record_item_type(record: &HistoryRecord) -> String {
    let item_type = record.kind.trim();
    match item_type {
        "" | "record" => "indexed_item".to_owned(),
        value => value.to_owned(),
    }
}

fn item_type_for_id(store: &Store, item_id: Uuid) -> String {
    if let Ok(record) = store.get_record(item_id) {
        return public_record_item_type(&record);
    }
    if store.get_event(item_id).is_ok() {
        return "event".to_owned();
    }
    if store.get_session(item_id).is_ok() {
        return "session".to_owned();
    }
    if store.get_run(item_id).is_ok() {
        return "run".to_owned();
    }
    "indexed_item".to_owned()
}

fn source_path_for(store: &Store, source_id: Option<Uuid>) -> Option<String> {
    source_id
        .and_then(|source_id| store.get_capture_source(source_id).ok())
        .and_then(|source| source.descriptor.raw_source_path)
}

fn source_path_exists(source_path: Option<&str>) -> Option<bool> {
    source_path.map(|path| Path::new(path).exists())
}

fn event_cursor(event: &Event) -> Option<String> {
    if let Some(cursor) = event.payload.get("cursor").and_then(|value| value.as_str()) {
        return Some(cursor.to_owned());
    }
    event
        .payload
        .get("body")
        .and_then(|body| body.get("cursor"))
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

fn compact_json(mut value: Value) -> Value {
    prune_null_json(&mut value);
    value
}

fn parse_search_limit(value: &str) -> std::result::Result<usize, String> {
    let limit = value
        .parse::<usize>()
        .map_err(|err| format!("invalid search limit: {err}"))?;
    if !(1..=MAX_SEARCH_LIMIT).contains(&limit) {
        return Err(format!(
            "search limit must be between 1 and {MAX_SEARCH_LIMIT}"
        ));
    }
    Ok(limit)
}

fn prune_null_json(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|_, nested| {
                prune_null_json(nested);
                !nested.is_null()
            });
        }
        Value::Array(items) => {
            for item in items {
                prune_null_json(item);
            }
        }
        _ => {}
    }
}

fn run_search(
    args: SearchArgs,
    data_root: PathBuf,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    let refresh = refresh_before_search(&args, &data_root)?;
    let store = Store::open(database_path(data_root))?;
    let query = args.query.unwrap_or_default();
    let options = ctx_history_search::PacketOptions {
        limit: args.limit,
        filters: search_filters(
            args.provider,
            args.repo.clone(),
            args.since.clone(),
            args.primary_only,
            args.include_subagents,
            args.event_type.clone(),
            args.file.clone(),
        )?,
        ..ctx_history_search::PacketOptions::default()
    };
    let packet = ctx_history_search::search_packet(&store, &query, &options)?;
    let result_count = packet.results.len();
    let indexed_items = indexed_history_item_count(&store)?;
    let citation_count = packet
        .results
        .iter()
        .map(|result| result.citations.len())
        .sum::<usize>();
    analytics::insert_count_bucket(
        analytics_properties,
        "result_count_bucket",
        result_count as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "citation_count_bucket",
        citation_count as u64,
    );
    if args.json {
        print_share_safe_value(SearchDto::packet(&store, &packet, &refresh))?;
    } else {
        if refresh.status == "failed" && args.refresh == RefreshArg::Auto {
            if let Some(error) = &refresh.error {
                eprintln!(
                    "warning: search refresh failed; serving existing index; use --refresh strict to fail instead: {error}"
                );
            }
        }
        if packet.results.is_empty() {
            println!("no results");
            if query.trim().is_empty() {
                println!("next: ctx list --limit 20");
            } else if indexed_items == 0 {
                println!("next: ctx import --all");
            } else {
                println!("next: ctx list --limit 20");
            }
        }
        for result in packet.results {
            println!("{}", result.title);
            if let Some(event_id) = result.event_id {
                println!("  ctx_event_id: {event_id}");
            }
            if let Some(session_id) = result.session_id {
                println!("  ctx_session_id: {session_id}");
            }
            if let Some(provider_session_id) = &result.provider_session_id {
                println!("  provider_session_id: {provider_session_id}");
            }
            println!("  {}", result.snippet);
            for command in search_next_commands(&result).into_iter().take(3) {
                println!("  next: {command}");
            }
            for citation in result.citations.iter().take(2) {
                println!(
                    "  citation: {} {}",
                    public_citation_item_type(citation.citation_type),
                    citation.id
                );
            }
        }
    }
    Ok(())
}

fn refresh_before_search(args: &SearchArgs, data_root: &Path) -> Result<SearchRefreshReport> {
    if args.refresh == RefreshArg::Off {
        return Ok(SearchRefreshReport::skipped(RefreshArg::Off, "skipped"));
    }
    let sources = search_refresh_sources(args.provider);
    if sources.is_empty() {
        if args.refresh == RefreshArg::Strict {
            return Err(anyhow!(
                "strict search refresh found no supported discovered native provider sources; use --refresh off to search the existing index"
            ));
        }
        return Ok(SearchRefreshReport::skipped(args.refresh, "no_sources"));
    }
    let source_count = sources.len();
    match refresh_sources_for_search(data_root, sources, args.refresh, args.json) {
        Ok(totals) => Ok(SearchRefreshReport::completed(
            args.refresh,
            source_count,
            totals,
        )),
        Err(err) if args.refresh == RefreshArg::Auto => Ok(SearchRefreshReport::failed(
            RefreshArg::Auto,
            source_count,
            error_summary(&err),
        )),
        Err(err) => Err(err.context("search refresh failed")),
    }
}

fn search_refresh_sources(provider: Option<ProviderArg>) -> Vec<SourceInfo> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    let mut sources = if let Some(provider) = provider {
        discover_provider_sources_for_provider(&home, provider.capture_provider())
    } else {
        discovered_sources()
    };
    sources
        .drain(..)
        .filter(|source| {
            source.exists
                && matches!(source.import_support, ProviderImportSupport::Native)
                && source.status == ProviderSourceStatus::Available
                && source.source_format != "codex_history_jsonl"
        })
        .collect()
}

fn refresh_sources_for_search(
    data_root: &Path,
    sources: Vec<SourceInfo>,
    refresh: RefreshArg,
    json_output: bool,
) -> Result<ImportTotals> {
    fs::create_dir_all(data_root)?;
    config::write_default_config(data_root)?;
    let db_path = database_path(data_root.to_path_buf());
    let planned_sources = sources
        .into_iter()
        .map(|source| (source, SourceStats::default()))
        .collect::<Vec<_>>();
    if planned_sources.is_empty() {
        return Ok(ImportTotals::default());
    }

    let progress_arg = match refresh {
        RefreshArg::Strict if json_output => ProgressArg::Json,
        RefreshArg::Strict => ProgressArg::Auto,
        RefreshArg::Auto | RefreshArg::Off => ProgressArg::None,
    };
    let progress = ProgressReporter::new(progress_arg, json_output, "search-refresh", 0);
    let mut totals = ImportTotals::default();
    if should_parallelize_import(&planned_sources) {
        let source_states = Arc::new(Mutex::new(
            planned_sources
                .iter()
                .map(|(_, stats)| SourceProgressSnapshot {
                    completed_bytes: 0,
                    total_bytes: stats.bytes,
                })
                .collect::<Vec<_>>(),
        ));
        let handles = planned_sources
            .into_iter()
            .enumerate()
            .map(|(index, (source, stats))| {
                let db_path = db_path.clone();
                let progress_callback = progress.parallel_codex_import_callback(
                    &source,
                    index,
                    Arc::clone(&source_states),
                );
                thread::spawn(move || -> Result<ImportSourceOutcome> {
                    let mut store = Store::open(&db_path)?;
                    let summary = import_one_source_without_search_refresh(
                        &mut store,
                        &source,
                        progress_callback,
                        false,
                    )?;
                    Ok(ImportSourceOutcome {
                        index,
                        source,
                        stats,
                        summary,
                    })
                })
            })
            .collect::<Vec<_>>();

        let mut outcomes = Vec::with_capacity(handles.len());
        for handle in handles {
            let outcome = handle
                .join()
                .map_err(|_| anyhow!("provider import worker panicked"))??;
            outcomes.push(outcome);
        }
        outcomes.sort_by_key(|outcome| outcome.index);
        for outcome in outcomes {
            totals.add(&outcome.summary, &outcome.stats);
        }
    } else {
        let mut store = Store::open(&db_path)?;
        let mut completed_source_bytes = 0u64;
        for (source, stats) in planned_sources {
            progress.message(
                "refreshing",
                format!("importing {}", source.provider.as_str()),
            );
            let source_progress = progress.codex_import_callback(&source, completed_source_bytes);
            completed_source_bytes = completed_source_bytes.saturating_add(stats.bytes);
            let summary = import_one_source_without_search_refresh(
                &mut store,
                &source,
                source_progress,
                false,
            )?;
            totals.add(&summary, &stats);
            progress.done(
                "refreshing",
                format!("refreshed {}", source.provider.as_str()),
                completed_source_bytes,
            );
        }
    }

    Store::open(&db_path)?.checkpoint_wal_truncate_if_larger_than(WAL_TRUNCATE_MIN_BYTES)?;
    Ok(totals)
}

fn run_doctor(
    args: JsonArgs,
    data_root: PathBuf,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    let store = Store::open(database_path(data_root.clone()))?;
    let mut findings = store.validate()?;
    if !data_root.exists() {
        findings.push(format!("data root does not exist: {}", data_root.display()));
    }
    analytics::insert_count_bucket(
        analytics_properties,
        "finding_count_bucket",
        findings.len() as u64,
    );
    if args.json {
        print_json(json!({
            "schema_version": 1,
            "ok": findings.is_empty(),
            "findings": findings,
        }))?;
    } else if findings.is_empty() {
        println!("ok");
    } else {
        for finding in findings {
            println!("{finding}");
        }
    }
    Ok(())
}

fn run_validate(
    args: JsonArgs,
    data_root: PathBuf,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    let store = Store::open(database_path(data_root))?;
    let findings = store.validate()?;
    analytics::insert_count_bucket(
        analytics_properties,
        "finding_count_bucket",
        findings.len() as u64,
    );
    if args.json {
        print_json(json!({
            "schema_version": 1,
            "valid": findings.is_empty(),
            "findings": findings,
        }))?;
    } else if findings.is_empty() {
        println!("valid");
    } else {
        for finding in findings {
            println!("{finding}");
        }
    }
    Ok(())
}

fn import_requests(args: &ImportArgs) -> Result<Vec<SourceInfo>> {
    if let Some(path) = &args.path {
        let provider = args
            .provider
            .unwrap_or(ProviderArg::Codex)
            .capture_provider();
        let source = explicit_path_source(provider, path.clone());
        validate_source_import_supported(&source)?;
        return Ok(vec![source]);
    }
    if args.all || args.provider.is_none() {
        return Ok(discovered_sources()
            .into_iter()
            .filter(|source| {
                source.exists
                    && matches!(source.import_support, ProviderImportSupport::Native)
                    && source.status == ProviderSourceStatus::Available
            })
            .collect());
    }
    let provider = args.provider.expect("checked provider").capture_provider();
    let sources = discovered_sources()
        .into_iter()
        .filter(|source| {
            source.provider == provider
                && source.exists
                && source.status == ProviderSourceStatus::Available
        })
        .collect::<Vec<_>>();
    if sources.is_empty() {
        let spec = provider_source_spec(provider);
        if let Some(reason) = spec.and_then(|spec| spec.unsupported_reason) {
            return Err(anyhow!(
                "{} native import is unsupported: {reason}",
                provider.as_str()
            ));
        }
        return Err(anyhow!(
            "no native {} history found; use `ctx sources` to inspect discovered provider paths",
            provider.as_str()
        ));
    }
    for source in &sources {
        validate_source_import_supported(source)?;
    }
    Ok(sources)
}

fn validate_source_import_supported(source: &SourceInfo) -> Result<()> {
    match source.import_support {
        ProviderImportSupport::Native => Ok(()),
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

fn import_one_source(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    full_rescan: bool,
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
    )
}

fn import_one_source_without_search_refresh(
    store: &mut Store,
    source: &SourceInfo,
    progress: Option<CodexSessionImportProgressCallback>,
    full_rescan: bool,
) -> Result<ProviderImportSummary> {
    import_one_source_inner(store, source, progress, false, full_rescan)
}

fn import_one_source_inner(
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

fn import_manifested_source(
    store: &mut Store,
    source: &SourceInfo,
    record_id: Uuid,
    tool_output_mode: CodexToolOutputMode,
    event_mode: CodexEventImportMode,
    include_notices: bool,
    progress: Option<CodexSessionImportProgressCallback>,
) -> Result<ProviderImportSummary> {
    let source_root = source.path.display().to_string();
    let files = collect_source_import_files(source)
        .with_context(|| format!("catalog import files from {}", source.path.display()))?;
    if files.is_empty() {
        return Err(anyhow!(
            "no importable {} history files found under {}",
            source.provider.as_str(),
            source.path.display()
        ));
    }
    let current_paths = files
        .iter()
        .map(|file| file.source_path.clone())
        .collect::<Vec<_>>();
    let observed_at_ms = Utc::now().timestamp_millis();
    store.begin_immediate_batch()?;
    let persist = (|| -> Result<()> {
        store.upsert_source_import_files(&files)?;
        store.mark_source_import_missing_paths_stale(
            source.provider,
            &source_root,
            &current_paths,
            observed_at_ms,
        )?;
        Ok(())
    })();
    match persist {
        Ok(()) => store.commit_batch()?,
        Err(err) => {
            let _ = store.rollback_batch();
            return Err(err);
        }
    }

    let pending = store.list_pending_source_import_files(source.provider, &source_root)?;
    if pending.is_empty() {
        return Ok(ProviderImportSummary::default());
    }

    let mut summary = ProviderImportSummary::default();
    for pending_file in pending {
        let path = PathBuf::from(&pending_file.source_path);
        let mut pending_source = explicit_path_source(source.provider, path);
        pending_source.source_format = source.source_format;
        let imported =
            import_one_source_inner(store, &pending_source, progress.clone(), false, true);
        match imported {
            Ok(file_summary) => {
                store.mark_source_import_file_indexed(
                    source.provider,
                    SourceImportFileIndexUpdate {
                        source_root: &source_root,
                        source_path: &pending_file.source_path,
                        file_size_bytes: pending_file.file_size_bytes,
                        file_modified_at_ms: pending_file.file_modified_at_ms,
                        indexed_at_ms: Utc::now().timestamp_millis(),
                    },
                )?;
                merge_provider_import_summary(&mut summary, file_summary);
            }
            Err(err) => {
                store.mark_source_import_file_failed(
                    source.provider,
                    &source_root,
                    &pending_file.source_path,
                    &err.to_string(),
                    Utc::now().timestamp_millis(),
                )?;
                return Err(err);
            }
        }
    }

    let _ = record_id;
    let _ = tool_output_mode;
    let _ = event_mode;
    let _ = include_notices;
    Ok(summary)
}

fn source_uses_import_file_manifest(source: &SourceInfo) -> bool {
    source.source_format != "codex_session_jsonl_tree"
}

fn merge_provider_import_summary(
    summary: &mut ProviderImportSummary,
    other: ProviderImportSummary,
) {
    summary.imported += other.imported;
    summary.skipped += other.skipped;
    summary.failed += other.failed;
    summary.redacted += other.redacted;
    summary.imported_sessions += other.imported_sessions;
    summary.skipped_sessions += other.skipped_sessions;
    summary.imported_events += other.imported_events;
    summary.skipped_events += other.skipped_events;
    summary.imported_edges += other.imported_edges;
    summary.skipped_edges += other.skipped_edges;
    summary.failures.extend(other.failures);
}

fn collect_source_import_files(source: &SourceInfo) -> Result<Vec<SourceImportFile>> {
    let paths = collect_source_import_paths(source)?;
    let source_root = source.path.display().to_string();
    let observed_at_ms = Utc::now().timestamp_millis();
    let mut files = Vec::with_capacity(paths.len());
    for path in paths {
        let metadata = fs::metadata(&path)
            .with_context(|| format!("stat import source file {}", path.display()))?;
        files.push(SourceImportFile {
            provider: source.provider,
            source_format: source.source_format.to_owned(),
            source_root: source_root.clone(),
            source_path: path.display().to_string(),
            file_size_bytes: metadata.len(),
            file_modified_at_ms: system_time_ms(metadata.modified().unwrap_or(UNIX_EPOCH)),
            observed_at_ms,
            metadata: json!({}),
        });
    }
    Ok(files)
}

fn collect_source_import_paths(source: &SourceInfo) -> Result<Vec<PathBuf>> {
    let metadata = fs::symlink_metadata(&source.path)?;
    if metadata.file_type().is_symlink() {
        return Err(anyhow!(
            "symlinked provider transcript roots are rejected: {}",
            source.path.display()
        ));
    }
    if metadata.file_type().is_file() {
        return Ok(if source_import_file_matches(source, &source.path) {
            vec![source.path.clone()]
        } else {
            Vec::new()
        });
    }
    if !metadata.file_type().is_dir() {
        return Ok(Vec::new());
    }

    let mut paths = Vec::new();
    let mut stack = vec![source.path.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() && source_import_file_matches(source, &path) {
                paths.push(path);
            }
        }
    }
    paths.sort();
    Ok(paths)
}

fn source_import_file_matches(source: &SourceInfo, path: &Path) -> bool {
    match source.provider {
        CaptureProvider::OpenCode => path == source.path,
        CaptureProvider::CopilotCli => {
            path.file_name().and_then(|name| name.to_str()) == Some("events.jsonl")
        }
        CaptureProvider::Antigravity => matches!(
            path.file_name().and_then(|name| name.to_str()),
            Some("transcript_full.jsonl" | "transcript.jsonl")
        ),
        CaptureProvider::Gemini => {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "chats")
        }
        CaptureProvider::Cursor => {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "agent-transcripts")
        }
        _ => path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"),
    }
}

fn system_time_ms(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn import_incremental_codex_session_tree(
    store: &mut Store,
    source: &SourceInfo,
    record_id: Uuid,
    tool_output_mode: CodexToolOutputMode,
    event_mode: CodexEventImportMode,
    include_notices: bool,
    progress: Option<CodexSessionImportProgressCallback>,
) -> Result<ProviderImportSummary> {
    let source_root = source.path.display().to_string();
    catalog_codex_session_tree(
        &source.path,
        store,
        CodexSessionCatalogOptions {
            source_root: Some(source.path.clone()),
            allow_partial_failures: true,
            ..CodexSessionCatalogOptions::default()
        },
    )
    .with_context(|| format!("catalog Codex sessions from {}", source.path.display()))?;

    let pending = store.list_pending_catalog_sessions(CaptureProvider::Codex, &source_root)?;
    if pending.is_empty() {
        return Ok(ProviderImportSummary::default());
    }

    let mut summary = ProviderImportSummary::default();
    let mut full_import_sessions = Vec::new();
    for session in &pending {
        let state = store.catalog_source_index_state(
            CaptureProvider::Codex,
            &source_root,
            &session.source_path,
        )?;
        let tail_start = state
            .and_then(|state| state.indexed_file_size_bytes)
            .filter(|indexed_size| *indexed_size > 0 && *indexed_size < session.file_size_bytes);
        if let Some(start_offset) = tail_start {
            let tail_summary = match import_codex_session_jsonl_tail(
                PathBuf::from(&session.source_path),
                start_offset,
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
            {
                Ok(summary) => summary,
                Err(err) => {
                    mark_catalog_sessions_failed(
                        store,
                        std::slice::from_ref(session),
                        &err.to_string(),
                    )?;
                    return Err(err);
                }
            };
            mark_catalog_sessions_indexed(store, std::slice::from_ref(session), &tail_summary)?;
            merge_provider_import_summary(&mut summary, tail_summary);
        } else {
            full_import_sessions.push(session.clone());
        }
    }

    if !full_import_sessions.is_empty() {
        let paths = full_import_sessions
            .iter()
            .map(|session| PathBuf::from(&session.source_path))
            .collect::<Vec<_>>();
        let full_summary = match import_codex_session_paths(
            paths,
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
        {
            Ok(summary) => summary,
            Err(err) => {
                mark_catalog_sessions_failed(store, &full_import_sessions, &err.to_string())?;
                return Err(err);
            }
        };
        mark_catalog_sessions_indexed(store, &full_import_sessions, &full_summary)?;
        merge_provider_import_summary(&mut summary, full_summary);
    }
    Ok(summary)
}

fn mark_catalog_sessions_indexed(
    store: &Store,
    sessions: &[CatalogSession],
    summary: &ProviderImportSummary,
) -> Result<()> {
    let indexed_at_ms = Utc::now().timestamp_millis();
    let event_count = if sessions.len() == 1 {
        summary
            .imported_events
            .saturating_add(summary.skipped_events) as u64
    } else {
        0
    };
    for session in sessions {
        store.mark_catalog_source_indexed(
            session.provider,
            CatalogSourceIndexUpdate {
                source_root: &session.source_root,
                source_path: &session.source_path,
                file_size_bytes: session.file_size_bytes,
                file_modified_at_ms: session.file_modified_at_ms,
                event_count,
                indexed_at_ms,
            },
        )?;
    }
    Ok(())
}

fn mark_catalog_sessions_failed(
    store: &Store,
    sessions: &[CatalogSession],
    error: &str,
) -> Result<()> {
    let indexed_at_ms = Utc::now().timestamp_millis();
    for session in sessions {
        store.mark_catalog_source_failed(
            session.provider,
            &session.source_root,
            &session.source_path,
            error,
            indexed_at_ms,
        )?;
    }
    Ok(())
}

fn source_uses_incremental_event_search(source: &SourceInfo) -> bool {
    matches!(
        source.provider,
        CaptureProvider::Codex
            | CaptureProvider::Claude
            | CaptureProvider::Pi
            | CaptureProvider::Cursor
            | CaptureProvider::OpenCode
            | CaptureProvider::Antigravity
            | CaptureProvider::Gemini
            | CaptureProvider::CopilotCli
            | CaptureProvider::FactoryAiDroid
    )
}

fn codex_tool_output_mode() -> Result<CodexToolOutputMode> {
    if let Some(raw) = env::var_os("CTX_CODEX_TOOL_OUTPUT_MODE") {
        let raw = raw.to_string_lossy();
        return match raw.as_ref() {
            "full" => Ok(CodexToolOutputMode::Full),
            "metadata" => Ok(CodexToolOutputMode::Metadata),
            "failures" | "failure" | "errors" | "error" => Ok(CodexToolOutputMode::Failures),
            "skip" => Ok(CodexToolOutputMode::Skip),
            other => Err(anyhow!(
                "unsupported CTX_CODEX_TOOL_OUTPUT_MODE={other:?}; expected full, metadata, failures, or skip"
            )),
        };
    }
    if env::var_os("CTX_EXPERIMENTAL_SKIP_TOOL_OUTPUTS").is_some() {
        return Ok(CodexToolOutputMode::Skip);
    }
    Ok(CodexToolOutputMode::Skip)
}

fn codex_event_import_mode() -> Result<CodexEventImportMode> {
    if let Some(raw) = env::var_os("CTX_CODEX_EVENT_MODE") {
        let raw = raw.to_string_lossy();
        return match raw.as_ref() {
            "search" | "message" | "messages" => Ok(CodexEventImportMode::Search),
            "rich" | "full" => Ok(CodexEventImportMode::Rich),
            other => Err(anyhow!(
                "unsupported CTX_CODEX_EVENT_MODE={other:?}; expected search or rich"
            )),
        };
    }
    Ok(CodexEventImportMode::Search)
}

fn codex_include_notices() -> bool {
    env::var_os("CTX_CODEX_INCLUDE_NOTICES").is_some()
}

fn source_stats(path: &Path) -> Result<SourceStats> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_file() {
        return Ok(SourceStats {
            files: 1,
            bytes: metadata.len(),
        });
    }
    if !metadata.file_type().is_dir() {
        return Ok(SourceStats::default());
    }

    let mut stats = SourceStats::default();
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                let metadata = entry.metadata()?;
                stats.files += 1;
                stats.bytes = stats.bytes.saturating_add(metadata.len());
            }
        }
    }
    Ok(stats)
}

fn import_record_for_source(source: &SourceInfo) -> HistoryRecord {
    let key = format!(
        "agent-history:{}:{}",
        source.provider.as_str(),
        source.path.display()
    );
    let mut record = HistoryRecord::new(
        format!("{} agent history", source.provider.as_str()),
        format!(
            "Indexed local agent history from {} ({})",
            source.path.display(),
            source.source_format
        ),
        vec!["agent-history".into(), source.provider.as_str().into()],
        "agent_history",
        source.path.parent().map(|path| path.display().to_string()),
    );
    record.id = stable_capture_uuid(&key, "record");
    record
}

fn discovered_sources() -> Vec<SourceInfo> {
    home_dir()
        .as_deref()
        .map(discover_provider_sources)
        .unwrap_or_default()
}

fn explicit_path_source(provider: CaptureProvider, path: PathBuf) -> SourceInfo {
    source_for_path(provider, path)
}

fn source_for_path(provider: CaptureProvider, path: PathBuf) -> SourceInfo {
    provider_source_for_path(provider, path)
}

fn sources_json(sources: &[SourceInfo]) -> Vec<Value> {
    sources
        .iter()
        .map(|source| {
            json!({
                "provider": source.provider.as_str(),
                "path": source.path,
                "exists": source.exists,
                "source_format": source.source_format,
                "status": source.status.as_str(),
                "import_support": import_support_json(source.import_support),
                "native_import": matches!(source.import_support, ProviderImportSupport::Native),
                "importable": source.status == ProviderSourceStatus::Available
                    && matches!(source.import_support, ProviderImportSupport::Native),
                "raw_retention": raw_retention_json(source.raw_retention),
                "unsupported_reason": source.unsupported_reason,
            })
        })
        .collect()
}

fn import_support_json(support: ProviderImportSupport) -> &'static str {
    match support {
        ProviderImportSupport::Native => "native",
        ProviderImportSupport::Unsupported => "unsupported",
    }
}

fn raw_retention_json(retention: ProviderRawRetention) -> &'static str {
    match retention {
        ProviderRawRetention::None => "none",
        ProviderRawRetention::PathReference => "path_reference",
        ProviderRawRetention::MetadataOnly => "metadata_only",
        ProviderRawRetention::LocalBlob => "local_blob",
        ProviderRawRetention::Withheld => "withheld",
    }
}

fn search_filters(
    provider: Option<ProviderArg>,
    repo: Option<String>,
    since: Option<String>,
    primary_only: bool,
    include_subagents: bool,
    event_type: Option<String>,
    file: Option<PathBuf>,
) -> Result<ctx_history_search::SearchFilters> {
    Ok(ctx_history_search::SearchFilters {
        provider: provider.map(ProviderArg::capture_provider),
        repo,
        since: since.as_deref().map(parse_since_filter).transpose()?,
        primary_only,
        include_subagents: include_subagents || !primary_only,
        event_type: event_type
            .as_deref()
            .map(EventType::from_str)
            .transpose()
            .map_err(|err| anyhow!("{err}"))?,
        file: file.map(|path| path.display().to_string()),
    })
}

fn parse_since_filter(value: &str) -> Result<chrono::DateTime<Utc>> {
    let trimmed = value.trim();
    if let Some(days) = trimmed.strip_suffix('d') {
        let days: i64 = days
            .parse()
            .with_context(|| format!("invalid --since day window: {value}"))?;
        return Ok(Utc::now() - Duration::days(days));
    }
    Ok(chrono::DateTime::parse_from_rfc3339(trimmed)
        .with_context(|| format!("invalid --since value: {value}"))?
        .with_timezone(&Utc))
}

fn print_json(value: Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn print_share_safe_value(mut value: Value) -> Result<()> {
    mark_share_safe(&mut value);
    print_json(value)
}

fn mark_share_safe(value: &mut Value) {
    if let Value::Object(map) = value {
        map.entry("share_safe").or_insert(Value::Bool(false));
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
