use std::{
    env, fs,
    fs::File,
    io::{BufRead, BufReader, IsTerminal, Write},
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
    thread,
    time::{Duration as StdDuration, Instant},
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

use analytics::AnalyticsEvent;
use config::{AppConfig, CONFIG_FILE};
use ctx_history_capture::{
    catalog_codex_session_tree, discover_provider_sources, import_antigravity_cli_history,
    import_claude_projects_jsonl_tree, import_codex_history_jsonl, import_codex_session_jsonl,
    import_codex_session_paths, import_codex_session_tree, import_copilot_cli_session_events,
    import_cursor_native_history, import_factory_ai_droid_sessions, import_gemini_cli_history,
    import_opencode_sqlite, import_pi_session_jsonl, import_provider_fixture_jsonl,
    provider_source_for_path, provider_source_spec, stable_capture_uuid,
    AntigravityCliImportOptions, CatalogSummary, ClaudeProjectsImportOptions, CodexEventImportMode,
    CodexHistoryImportOptions, CodexSessionCatalogOptions, CodexSessionImportOptions,
    CodexSessionImportProgress, CodexSessionImportProgressCallback, CodexToolOutputMode,
    CopilotCliImportOptions, CursorNativeImportOptions, FactoryAiDroidImportOptions,
    GeminiCliImportOptions, OpenCodeSqliteImportOptions, PiSessionImportOptions,
    ProviderFixtureImportOptions, ProviderImportSummary, ProviderImportSupport, ProviderSource,
};
use ctx_history_core::{
    database_path, default_data_root, CaptureProvider, ContextCitation, ContextCitationType, Event,
    EventType, Fidelity, HistoryRecord, ProviderRawRetention, Session,
};
use ctx_history_store::{CatalogSession, CatalogSourceIndexUpdate, Store};

const WAL_TRUNCATE_MIN_BYTES: u64 = 64 * 1024 * 1024;
const NORMALIZED_PROVIDER_IMPORT_DEV_ENV: &str = "CTX_PROVIDER_NORMALIZED_IMPORT_DEV";

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
    #[command(about = "Create local ctx storage and show next steps")]
    Setup(SetupArgs),
    #[command(about = "Show local ctx index status")]
    Status(JsonArgs),
    #[command(about = "List configured and discovered agent history sources")]
    Sources(JsonArgs),
    #[command(about = "Index provider history into local search")]
    Import(ImportArgs),
    #[command(about = "List indexed agent history items")]
    List(ListArgs),
    #[command(about = "Show one indexed agent history item")]
    Show(ShowArgs),
    #[command(about = "Search indexed agent history")]
    Search(SearchArgs),
    #[command(about = "Check local ctx health")]
    Doctor(JsonArgs),
    #[command(about = "Validate local ctx storage")]
    Validate(JsonArgs),
}

#[derive(Debug, Args)]
struct SetupArgs {
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
    #[arg(long)]
    all: bool,
    #[arg(long)]
    resume: bool,
    #[arg(long)]
    json: bool,
    #[arg(long, value_enum, default_value_t = ProgressArg::Auto)]
    progress: ProgressArg,
}

impl ImportArgs {
    fn resume_mode(&self) -> &'static str {
        if self.resume {
            "idempotent_rescan"
        } else {
            "normal_scan"
        }
    }
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
    id: Uuid,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct SearchArgs {
    query: Option<String>,
    #[arg(long, default_value_t = 20)]
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
            Self::Show(args) => args.json,
            Self::Search(args) => args.json,
            Self::Doctor(args) => args.json,
            Self::Validate(args) => args.json,
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

#[derive(Debug, Default)]
struct ImportTotals {
    source_files: usize,
    source_bytes: u64,
    imported_sessions: usize,
    imported_events: usize,
    imported_edges: usize,
    skipped: usize,
    failed: usize,
}

impl ImportTotals {
    fn add(&mut self, summary: &ProviderImportSummary, stats: &SourceStats) {
        self.source_files += stats.files;
        self.source_bytes = self.source_bytes.saturating_add(stats.bytes);
        self.imported_sessions += summary.imported_sessions;
        self.imported_events += summary.imported_events;
        self.imported_edges += summary.imported_edges;
        self.skipped += summary.skipped;
        self.failed += summary.failed;
    }
}

#[derive(Debug, Default)]
struct CatalogTotals {
    sources: usize,
    source_files: usize,
    source_bytes: u64,
    cataloged_sessions: usize,
    skipped_sessions: usize,
    failed_sessions: usize,
}

impl CatalogTotals {
    fn add(&mut self, summary: &CatalogSummary) {
        self.sources += 1;
        self.source_files += summary.source_files;
        self.source_bytes = self.source_bytes.saturating_add(summary.source_bytes);
        self.cataloged_sessions += summary.cataloged_sessions;
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
        let path = source.path.display().to_string();
        Some(Arc::new(move |progress: CodexSessionImportProgress| {
            let completed_bytes = source_offset_bytes.saturating_add(progress.completed_bytes);
            reporter.emit(ProgressLine {
                phase: "indexing",
                message: format!("{provider} {path}"),
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
        let path = source.path.display().to_string();
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
                message: format!("{provider} {path}"),
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
            message: format!(
                "imported {} {}",
                source.provider.as_str(),
                source.path.display()
            ),
            completed_bytes,
            total_bytes: self.total_bytes.max(total_bytes).max(completed_bytes),
            completed_files: Some(stats.files),
            total_files: Some(stats.files),
            imported_events: Some(summary.imported_events),
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
        "{} [{}] {:>5.1}% {}/{}{}{} {} - {}",
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
    let data_root = cli
        .data_root
        .clone()
        .map(Ok)
        .unwrap_or_else(default_data_root)
        .context("resolve ctx data root")?;
    let config = AppConfig::load(&data_root)?;

    let result = match cli.command {
        CommandRoot::Setup(args) => run_setup(args, data_root.clone()),
        CommandRoot::Status(args) => run_status(args, data_root.clone()),
        CommandRoot::Sources(args) => run_sources(args),
        CommandRoot::Import(args) => run_import(args, data_root.clone()),
        CommandRoot::List(args) => run_list(args, data_root.clone()),
        CommandRoot::Show(args) => run_show(args, data_root.clone()),
        CommandRoot::Search(args) => run_search(args, data_root.clone()),
        CommandRoot::Doctor(args) => run_doctor(args, data_root.clone()),
        CommandRoot::Validate(args) => run_validate(args, data_root.clone()),
    };
    analytics::send_cli_event(
        &data_root,
        &config,
        AnalyticsEvent {
            action,
            json_output,
            success: result.is_ok(),
            duration: started.elapsed(),
        },
    );
    result
}

fn run_setup(args: SetupArgs, data_root: PathBuf) -> Result<()> {
    fs::create_dir_all(&data_root)?;
    let db_path = database_path(data_root.clone());
    let store = Store::open(&db_path)?;
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

    if args.json {
        print_json(json!({
            "schema_version": 1,
            "data_root": data_root,
            "database_path": store.path(),
            "config_path": data_root.join(CONFIG_FILE),
            "sources": sources_json(&sources),
            "catalog": {
                "sources": catalog.sources,
                "source_files": catalog.source_files,
                "source_bytes": catalog.source_bytes,
                "cataloged_sessions": catalog.cataloged_sessions,
                "indexed_sessions": catalog_counts.indexed,
                "pending_sessions": catalog_counts.pending,
                "skipped_sessions": catalog.skipped_sessions,
                "failed_sessions": catalog.failed_sessions,
                "failed_index_sessions": catalog_counts.failed,
                "stale_sessions": catalog_counts.stale,
            },
            "catalog_sources": catalog_sources,
            "network_required": false,
            "repo_writes": false,
        }))?;
    } else {
        println!("ctx local agent history search is ready");
        println!("data_root: {}", data_root.display());
        println!("database_path: {}", store.path().display());
        println!("config_path: {}", data_root.join(CONFIG_FILE).display());
        println!("cataloged_sessions: {}", catalog.cataloged_sessions);
        println!("indexed_catalog_sessions: {}", catalog_counts.indexed);
        println!("pending_catalog_sessions: {}", catalog_counts.pending);
        println!("failed_catalog_sessions: {}", catalog_counts.failed);
        println!("stale_catalog_sessions: {}", catalog_counts.stale);
        println!("catalog_source_files: {}", catalog.source_files);
        println!("catalog_source_bytes: {}", catalog.source_bytes);
        println!("next_steps:");
        println!("  ctx sources");
        println!("  ctx import --all");
        println!("  ctx search \"what failed before\"");
    }
    Ok(())
}

fn run_status(args: JsonArgs, data_root: PathBuf) -> Result<()> {
    let db_path = database_path(data_root.clone());
    let initialized = db_path.exists();
    let config_path = data_root.join(CONFIG_FILE);
    let (records, sources, catalog_counts) = if initialized {
        let store = Store::open(&db_path)?;
        (
            store.list_records(usize::MAX)?.len() + store.list_sessions()?.len(),
            store.list_capture_sources()?.len(),
            store.catalog_session_counts()?,
        )
    } else {
        (0, 0, Default::default())
    };

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

fn run_sources(args: JsonArgs) -> Result<()> {
    let sources = discovered_sources();
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
            "skipped_sessions": summary.skipped_sessions,
            "failed_sessions": summary.failed_sessions,
        }));
    }
    Ok((totals, catalog_sources))
}

fn run_import(args: ImportArgs, data_root: PathBuf) -> Result<()> {
    fs::create_dir_all(&data_root)?;
    config::write_default_config(&data_root)?;
    let db_path = database_path(data_root);
    let mut store = Store::open(&db_path)?;
    let mut totals = ImportTotals::default();
    let mut imported_sources = Vec::new();

    let requests = import_requests(&args)?;
    if requests.is_empty() {
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

    let progress = ProgressReporter::new(args.progress, args.json, "import", planned_total_bytes);
    progress.message(
        "discovering",
        format!(
            "found {} import source(s), {}",
            planned_sources.len(),
            format_bytes(planned_total_bytes)
        ),
    );

    if should_parallelize_import(&planned_sources) {
        let final_refresh_required = store.event_search_projection_needs_backfill()?
            || planned_sources
                .iter()
                .any(|(source, _)| !source_uses_incremental_event_search(source));
        drop(store);

        if !args.json {
            for (source, stats) in &planned_sources {
                println!(
                    "importing {} {} ({} files, {} bytes)",
                    source.provider.as_str(),
                    source.path.display(),
                    stats.files,
                    stats.bytes
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
                thread::spawn(move || -> Result<ImportSourceOutcome> {
                    let mut store = Store::open(&db_path)?;
                    let summary = import_one_source_without_search_refresh(
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
                    })?;
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
        let mut first_error = None;
        for handle in handles {
            match handle.join() {
                Ok(Ok(outcome)) => outcomes.push(outcome),
                Ok(Err(err)) => {
                    if first_error.is_none() {
                        first_error = Some(err);
                    }
                }
                Err(_) => {
                    if first_error.is_none() {
                        first_error = Some(anyhow!("provider import worker panicked"));
                    }
                }
            }
        }
        if let Some(err) = first_error {
            return Err(err);
        }

        outcomes.sort_by_key(|outcome| outcome.index);
        for outcome in outcomes {
            totals.add(&outcome.summary, &outcome.stats);
            progress.parallel_source_done(
                &outcome.source,
                outcome.index,
                &source_states,
                outcome.stats,
                &outcome.summary,
            );
            if !args.json {
                println!(
                    "source_imported: sessions={} events={} edges={} skipped={} failed={}",
                    outcome.summary.imported_sessions,
                    outcome.summary.imported_events,
                    outcome.summary.imported_edges,
                    outcome.summary.skipped,
                    outcome.summary.failed
                );
            }
            imported_sources.push(source_import_json(
                &outcome.source,
                &outcome.stats,
                &outcome.summary,
            ));
        }

        if final_refresh_required {
            progress.message("finalizing", "refreshing search index");
            let store = Store::open(&db_path)?;
            store.refresh_search_index()?;
        }
    } else {
        let mut completed_source_bytes = 0u64;
        for (source, stats) in planned_sources {
            if !args.json {
                println!(
                    "importing {} {} ({} files, {} bytes)",
                    source.provider.as_str(),
                    source.path.display(),
                    stats.files,
                    stats.bytes
                );
            }
            let source_progress = progress.codex_import_callback(&source, completed_source_bytes);
            let summary = import_one_source(&mut store, &source, source_progress, args.resume)?;
            totals.add(&summary, &stats);
            completed_source_bytes = completed_source_bytes.saturating_add(stats.bytes);
            progress.done(
                "indexing",
                format!("imported {}", source.path.display()),
                completed_source_bytes,
            );
            if !args.json {
                println!(
                    "source_imported: sessions={} events={} edges={} skipped={} failed={}",
                    summary.imported_sessions,
                    summary.imported_events,
                    summary.imported_edges,
                    summary.skipped,
                    summary.failed
                );
            }
            imported_sources.push(source_import_json(&source, &stats, &summary));
        }
    }

    if totals.imported_sessions > 0 || totals.imported_events > 0 || totals.imported_edges > 0 {
        progress.message("finalizing", "optimizing search index");
        Store::open(&db_path)?.optimize_search_index()?;
    }

    progress.message("finalizing", "checkpointing search database");
    Store::open(&db_path)?.checkpoint_wal_truncate_if_larger_than(WAL_TRUNCATE_MIN_BYTES)?;

    if args.json {
        print_json(json!({
            "schema_version": 1,
            "resume": args.resume,
            "resume_mode": args.resume_mode(),
            "totals": {
                "source_files": totals.source_files,
                "source_bytes": totals.source_bytes,
                "imported_sessions": totals.imported_sessions,
                "imported_events": totals.imported_events,
                "imported_edges": totals.imported_edges,
                "skipped": totals.skipped,
                "failed": totals.failed,
            },
            "sources": imported_sources,
        }))?;
    } else {
        println!("source_files: {}", totals.source_files);
        println!("source_bytes: {}", totals.source_bytes);
        println!("imported_sessions: {}", totals.imported_sessions);
        println!("imported_events: {}", totals.imported_events);
        println!("imported_edges: {}", totals.imported_edges);
        println!("skipped: {}", totals.skipped);
        println!("failed: {}", totals.failed);
        println!("resume: {}", args.resume);
        println!("resume_mode: {}", args.resume_mode());
    }
    progress.done(
        "finalizing",
        format!("indexed {} source file(s)", totals.source_files),
        totals.source_bytes,
    );
    Ok(())
}

#[derive(Debug)]
struct ImportSourceOutcome {
    index: usize,
    source: SourceInfo,
    stats: SourceStats,
    summary: ProviderImportSummary,
}

fn should_parallelize_import(planned_sources: &[(SourceInfo, SourceStats)]) -> bool {
    let Some((first, _)) = planned_sources.first() else {
        return false;
    };
    planned_sources
        .iter()
        .any(|(source, _)| source.provider.as_str() != first.provider.as_str())
}

fn source_import_json(
    source: &SourceInfo,
    stats: &SourceStats,
    summary: &ProviderImportSummary,
) -> Value {
    json!({
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
    })
}

fn run_list(args: ListArgs, data_root: PathBuf) -> Result<()> {
    let store = Store::open(database_path(data_root))?;
    let records = store.list_records(args.limit)?;
    let remaining = args.limit.saturating_sub(records.len());
    let sessions = store
        .list_sessions()?
        .into_iter()
        .take(remaining)
        .collect::<Vec<_>>();
    if args.json {
        let mut items = Vec::new();
        for record in records {
            items.push(ListItemDto::record(&record));
        }
        for session in sessions {
            items.push(ListItemDto::session(&session));
        }
        print_json(json!({
            "schema_version": 1,
            "items": items,
        }))?;
    } else {
        for record in records {
            println!("{} {}", record.id, record.title);
        }
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

fn run_show(args: ShowArgs, data_root: PathBuf) -> Result<()> {
    let store = Store::open(database_path(data_root))?;
    let Ok(record) = store.get_record(args.id) else {
        let session = store.get_session(args.id)?;
        let events = store.events_for_session(session.id)?;
        if args.json {
            print_json(compact_json(json!({
                "schema_version": 1,
                "item": ShowDto::session(&store, &session),
                "events": events
                    .iter()
                    .map(|event| ShowDto::event(&store, event))
                    .collect::<Vec<_>>(),
            })))?;
        } else {
            println!("id: {}", session.id);
            println!("kind: session");
            println!("provider: {}", session.provider);
            if let Some(external_session_id) = session.external_session_id {
                println!("external_session_id: {external_session_id}");
            }
            if !events.is_empty() {
                println!();
                println!("events:");
                for event in events.iter().take(20) {
                    println!(
                        "  {} {:?} {}",
                        event.id,
                        event.event_type,
                        event_preview(event)
                    );
                }
            }
        }
        return Ok(());
    };
    let sessions = store.sessions_for_record(record.id)?;
    let events = store.events_for_record(record.id)?;
    if args.json {
        print_json(compact_json(json!({
            "schema_version": 1,
            "item": ShowDto::record(&record),
            "sessions": sessions
                .iter()
                .map(|session| ShowDto::session(&store, session))
                .collect::<Vec<_>>(),
            "events": events
                .iter()
                .map(|event| ShowDto::event(&store, event))
                .collect::<Vec<_>>(),
        })))?;
    } else {
        println!("id: {}", record.id);
        println!("title: {}", record.title);
        if !record.body.trim().is_empty() {
            println!();
            println!("{}", record.body);
        }
        if !sessions.is_empty() {
            println!();
            println!("sessions:");
            for session in sessions {
                println!(
                    "  {} {} {:?}",
                    session.id, session.provider, session.agent_type
                );
            }
        }
        if !events.is_empty() {
            println!();
            println!("events:");
            for event in events.iter().take(20) {
                println!("  {} {}", event.id, event.event_type.as_str());
            }
        }
    }
    Ok(())
}

fn event_preview(event: &Event) -> String {
    for key in ["text", "summary", "command", "message"] {
        if let Some(value) = event.payload.get(key).and_then(|value| value.as_str()) {
            return ctx_history_search::redacted_snippet(value, 120);
        }
    }
    if let Some(body) = event.payload.get("body") {
        for key in [
            "arguments_preview",
            "text",
            "summary",
            "command",
            "message",
            "tool",
            "name",
        ] {
            if let Some(value) = body.get(key).and_then(|value| value.as_str()) {
                return ctx_history_search::redacted_snippet(value, 120);
            }
        }
    }
    format!("{} event", event.event_type.as_str())
}

impl ListItemDto {
    fn record(record: &HistoryRecord) -> Value {
        compact_json(json!({
            "id": record.id,
            "item_id": record.id,
            "item_type": public_record_item_type(record),
            "title": record.title,
            "created_at": record.created_at,
            "updated_at": record.updated_at,
        }))
    }

    fn session(session: &Session) -> Value {
        compact_json(json!({
            "id": session.id,
            "item_id": session.id,
            "item_type": "session",
            "provider": session.provider,
            "external_session_id": session.external_session_id,
            "agent_type": session.agent_type,
            "started_at": session.started_at,
            "ended_at": session.ended_at,
        }))
    }
}

impl ShowDto {
    fn record(record: &HistoryRecord) -> Value {
        compact_json(json!({
            "id": record.id,
            "item_id": record.id,
            "item_type": public_record_item_type(record),
            "title": record.title,
            "text": record.body,
            "tags": record.tags,
            "workspace": record.workspace,
            "created_at": record.created_at,
            "updated_at": record.updated_at,
        }))
    }

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

    fn event(store: &Store, event: &Event) -> Value {
        let source_path = source_path_for(store, event.capture_source_id);
        compact_json(json!({
            "event_id": event.id,
            "item_id": event.id,
            "item_type": "event",
            "session_id": event.session_id,
            "sequence": event.seq,
            "event_type": event.event_type,
            "role": event.role,
            "occurred_at": event.occurred_at,
            "source_id": event.capture_source_id,
            "source_path": source_path,
            "source_exists": source_path_exists(source_path.as_deref()),
            "cursor": event_cursor(event),
            "preview": event_preview(event),
            "redaction_state": event.redaction_state,
        }))
    }
}

impl SearchDto {
    fn packet(store: &Store, packet: &ctx_history_search::SearchPacket) -> Value {
        compact_json(json!({
            "schema_version": packet.schema_version,
            "query": packet.query,
            "filters": packet.filters,
            "generated_at": packet.generated_at,
            "results": packet
                .results
                .iter()
                .map(|result| {
                    compact_json(json!({
                        "item_id": result.record_id,
                        "item_type": search_result_item_type(store, result),
                        "session_id": result.session_id,
                        "event_id": result.event_id,
                        "event_seq": result.event_seq,
                        "title": result.title,
                        "snippet": result.snippet,
                        "rank": result.rank,
                        "provider": result.provider,
                        "timestamp": result.timestamp,
                        "cwd": result.cwd,
                        "source_path": result.raw_source_path,
                        "source_exists": result.raw_source_exists,
                        "cursor": result.cursor,
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

fn public_citations(citations: &[ContextCitation]) -> Vec<Value> {
    citations
        .iter()
        .map(|citation| {
            compact_json(json!({
                "item_id": citation.id,
                "item_type": public_citation_item_type(citation.citation_type),
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
        "" | "record" | "work_record" => "indexed_item".to_owned(),
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

fn run_search(args: SearchArgs, data_root: PathBuf) -> Result<()> {
    refresh_before_search(&args, &data_root)?;
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
    if args.json {
        let packet = ctx_history_search::search_packet(&store, &query, &options)?;
        print_share_safe_value(SearchDto::packet(&store, &packet))?;
    } else {
        let packet = ctx_history_search::search_packet(&store, &query, &options)?;
        for result in packet.results {
            println!("{} {}", result.record_id, result.title);
            println!("  {}", result.snippet);
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

fn refresh_before_search(args: &SearchArgs, data_root: &Path) -> Result<()> {
    let sources = search_refresh_sources(args.provider);
    if sources.is_empty() {
        return Ok(());
    }
    if let Err(err) = refresh_sources_quietly(data_root, sources) {
        if !args.json {
            eprintln!("ctx search refresh skipped: {err:#}");
        }
    }
    Ok(())
}

fn search_refresh_sources(provider: Option<ProviderArg>) -> Vec<SourceInfo> {
    discovered_sources()
        .into_iter()
        .filter(|source| {
            provider.is_none_or(|provider| source.provider == provider.capture_provider())
        })
        .filter(|source| {
            source.exists && matches!(source.import_support, ProviderImportSupport::Native)
        })
        .collect()
}

fn refresh_sources_quietly(data_root: &Path, sources: Vec<SourceInfo>) -> Result<()> {
    fs::create_dir_all(data_root)?;
    config::write_default_config(data_root)?;
    let db_path = database_path(data_root.to_path_buf());
    let mut planned_sources = Vec::new();
    let mut planned_total_bytes = 0u64;
    for source in sources {
        let stats = source_stats(&source.path)
            .with_context(|| format!("scan import source {}", source.path.display()))?;
        planned_total_bytes = planned_total_bytes.saturating_add(stats.bytes);
        planned_sources.push((source, stats));
    }
    if planned_sources.is_empty() {
        return Ok(());
    }

    let progress = ProgressReporter::new(
        ProgressArg::None,
        true,
        "search-refresh",
        planned_total_bytes,
    );
    let mut totals = ImportTotals::default();
    if should_parallelize_import(&planned_sources) {
        let store = Store::open(&db_path)?;
        let final_refresh_required = store.event_search_projection_needs_backfill()?
            || planned_sources
                .iter()
                .any(|(source, _)| !source_uses_incremental_event_search(source));
        drop(store);

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
        if final_refresh_required {
            Store::open(&db_path)?.refresh_search_index()?;
        }
    } else {
        let mut store = Store::open(&db_path)?;
        for (source, stats) in planned_sources {
            let summary = import_one_source(&mut store, &source, None, false)?;
            totals.add(&summary, &stats);
        }
    }

    if totals.imported_sessions > 0 || totals.imported_events > 0 || totals.imported_edges > 0 {
        Store::open(&db_path)?.optimize_search_index()?;
    }
    Store::open(&db_path)?.checkpoint_wal_truncate_if_larger_than(WAL_TRUNCATE_MIN_BYTES)?;
    Ok(())
}

fn run_doctor(args: JsonArgs, data_root: PathBuf) -> Result<()> {
    let store = Store::open(database_path(data_root.clone()))?;
    let mut findings = store.validate()?;
    if !data_root.exists() {
        findings.push(format!("data root does not exist: {}", data_root.display()));
    }
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

fn run_validate(args: JsonArgs, data_root: PathBuf) -> Result<()> {
    let store = Store::open(database_path(data_root))?;
    let findings = store.validate()?;
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
                source.exists && matches!(source.import_support, ProviderImportSupport::Native)
            })
            .collect());
    }
    let provider = args.provider.expect("checked provider").capture_provider();
    let sources = discovered_sources()
        .into_iter()
        .filter(|source| source.provider == provider && source.exists)
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
        ProviderImportSupport::NormalizedDeveloperOnly => {
            if env::var_os(NORMALIZED_PROVIDER_IMPORT_DEV_ENV).is_some() {
                Ok(())
            } else {
                Err(anyhow!(
                    "{} normalized provider JSONL import is a developer-only input; set {}=1 to use it explicitly",
                    source.provider.as_str(),
                    NORMALIZED_PROVIDER_IMPORT_DEV_ENV
                ))
            }
        }
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
    let summary = if source.source_format == "normalized_provider_jsonl" {
        import_provider_fixture_jsonl(
            &source.path,
            store,
            ProviderFixtureImportOptions {
                source_path: Some(source.path.clone()),
                history_record_id: Some(record_id),
                expected_provider: Some(source.provider),
                allow_partial_failures: true,
                source_format: "normalized_provider_jsonl".to_owned(),
                fidelity: Fidelity::Partial,
                ..ProviderFixtureImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from)
    } else {
        match source.provider {
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
        }
    }?;
    if refresh_search_after_import {
        store.refresh_search_index()?;
    }
    Ok(summary)
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

    let paths = pending
        .iter()
        .map(|session| PathBuf::from(&session.source_path))
        .collect::<Vec<_>>();
    let summary = match import_codex_session_paths(
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
            mark_catalog_sessions_failed(store, &pending, &err.to_string())?;
            return Err(err);
        }
    };
    mark_catalog_sessions_indexed(store, &pending, &summary)?;
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
    source.provider == CaptureProvider::Codex
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
    let mut source = source_for_path(provider, path);
    if provider != CaptureProvider::Codex
        && provider != CaptureProvider::Pi
        && source.path.is_file()
        && source.path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
        && looks_like_normalized_provider_jsonl(&source.path)
    {
        source.source_format = "normalized_provider_jsonl";
        source.source_kind = ctx_history_capture::ProviderSourceKind::NormalizedDeveloperInput;
        source.import_support = ProviderImportSupport::NormalizedDeveloperOnly;
        source.status = if source.exists {
            ctx_history_capture::ProviderSourceStatus::Available
        } else {
            ctx_history_capture::ProviderSourceStatus::Missing
        };
        source.unsupported_reason = None;
    }
    source
}

fn looks_like_normalized_provider_jsonl(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    BufReader::new(file)
        .lines()
        .map_while(std::result::Result::ok)
        .find(|line| !line.trim().is_empty())
        .and_then(|line| serde_json::from_str::<Value>(&line).ok())
        .is_some_and(|value| value.get("provider").is_some() && value.get("session").is_some())
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
                "raw_retention": raw_retention_json(source.raw_retention),
                "unsupported_reason": source.unsupported_reason,
            })
        })
        .collect()
}

fn import_support_json(support: ProviderImportSupport) -> &'static str {
    match support {
        ProviderImportSupport::Native => "native",
        ProviderImportSupport::NormalizedDeveloperOnly => "normalized_developer_only",
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
