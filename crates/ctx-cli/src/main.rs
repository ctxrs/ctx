use std::{
    env,
    path::PathBuf,
    time::{Duration as StdDuration, Instant},
};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};

mod analytics;
mod commands;
mod config;
mod docs;
mod execution_capabilities;
mod history_source_plugins;
mod identity;
mod install_marker;
mod integrations;
mod mcp;
mod net;
mod output;
mod progress;
mod provider_args;
mod provider_sources;
mod search_filters;
mod search_render;
mod semantic;
mod skill;
mod store_util;
mod transcript;
mod upgrade;

#[cfg(test)]
mod parser_prop_tests;

use analytics::{AnalyticsEvent, AnalyticsProperties};
use commands::search::{RefreshArg, SearchRefreshReport};
use commands::sql::{parse_sql_timeout, raw_sql_result_json};
use commands::{
    doctor::run_doctor, import::run_import, index::run_index, locate::run_locate,
    search::run_search, setup::run_setup, show::run_show, sources::run_sources, sql::run_sql,
    status::run_status,
};
use config::AppConfig;
use ctx_history_core::{default_data_root, CaptureProvider};
use ctx_history_store::{
    RAW_SQL_DEFAULT_MAX_COLUMNS, RAW_SQL_DEFAULT_MAX_ROWS, RAW_SQL_DEFAULT_MAX_SQL_BYTES,
    RAW_SQL_DEFAULT_MAX_VALUE_BYTES,
};
use output::{compact_json, LocateFormat, OutputFormat, SqlFormat};
use progress::{progress_mode_name, ProgressArg};
use provider_args::{
    cli_supported_provider, parse_native_provider_arg, parse_provider_arg, ImportFormatArg,
    NativeProviderArg, ProviderArg,
};
use provider_sources::{discovered_plugin_sources_json, discovered_sources, sources_json};
use search_filters::{
    search_filters, search_has_intent, SearchFilterInput, SearchIntentInput,
    SourceIdentityFilterArgs,
};
use search_render::SearchDto;
use transcript::{event_window, event_window_json, session_transcript_json, TranscriptMode};

const WAL_TRUNCATE_MIN_BYTES: u64 = 64 * 1024 * 1024;
const LARGE_IMPORT_SOURCE_FILES_WARNING: usize = 10_000;
const LARGE_IMPORT_SOURCE_BYTES_WARNING: u64 = 1024 * 1024 * 1024;
const MAX_SEARCH_LIMIT: usize = 200;
pub(crate) const MAX_EVENT_WINDOW: usize = 50;
const MAX_HISTORY_SOURCE_PLUGIN_JSONL_LINE_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_VISIBLE_SOURCE_PROVIDERS: &[CaptureProvider] = &[
    CaptureProvider::Claude,
    CaptureProvider::Codex,
    CaptureProvider::Cursor,
    CaptureProvider::Pi,
    CaptureProvider::CopilotCli,
    CaptureProvider::OpenCode,
];

#[derive(Debug, Parser)]
#[command(name = "ctx", version, about = "Search local agent history")]
struct Cli {
    #[arg(long, env = "CTX_DATA_ROOT", global = true)]
    data_root: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        help = "Suppress non-essential setup/status output (also via CTX_QUIET=1)"
    )]
    quiet: bool,
    #[command(subcommand)]
    command: CommandRoot,
}

#[derive(Debug, Subcommand)]
enum CommandRoot {
    #[command(about = "Create local ctx storage and index discovered history")]
    Setup(SetupArgs),
    #[command(about = "Show local ctx index status")]
    Status(JsonArgs),
    #[command(about = "Show, watch, or wait for local indexing progress")]
    Index(commands::index::IndexArgs),
    #[command(about = "List configured and discovered agent history sources")]
    Sources(SourcesArgs),
    #[command(about = "Index provider history into local search")]
    Import(ImportArgs),
    #[command(about = "Show an indexed session transcript or event")]
    Show(ShowArgs),
    #[command(about = "Locate provider/source metadata for an indexed session or event")]
    Locate(LocateArgs),
    #[command(about = "Search indexed agent history")]
    Search(SearchArgs),
    #[command(about = "Run read-only SQL against the local ctx index")]
    Sql(SqlArgs),
    #[command(about = "Read embedded ctx documentation")]
    Docs(docs::DocsArgs),
    #[command(about = "Install or inspect ctx integrations")]
    Integrations(integrations::IntegrationsArgs),
    #[command(about = "Serve read-only ctx tools over MCP")]
    Mcp(mcp::McpArgs),
    #[command(about = "Run or inspect local ctx background maintenance")]
    Daemon(DaemonArgs),
    #[command(about = "Check or apply signed ctx CLI upgrades")]
    Upgrade(upgrade::UpgradeArgs),
    #[command(about = "Check local ctx health")]
    Doctor(DoctorArgs),
}

#[derive(Debug, Args)]
struct SetupArgs {
    #[arg(
        long,
        alias = "no-import",
        help = "Prepare local history inventory without importing searchable history"
    )]
    catalog_only: bool,
    #[arg(long, help = "Do not start daemon maintenance after setup")]
    no_daemon: bool,
    #[arg(long, help = "Wait for foreground lexical indexing before returning")]
    wait: bool,
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

#[derive(Debug, Args, Clone)]
struct SourcesArgs {
    #[arg(long)]
    json: bool,
    #[arg(
        long,
        value_parser = parse_provider_arg,
        hide_possible_values = true,
        help = "Show sources for one provider, for example codex, claude, cursor, pi, copilot-cli, or opencode"
    )]
    provider: Option<ProviderArg>,
    #[arg(long, help = "Show every supported provider location")]
    all: bool,
    #[arg(long, help = "Show missing locations for every known provider")]
    show_missing: bool,
}

#[derive(Debug, Args, Clone)]
struct DoctorArgs {
    #[arg(long)]
    json: bool,
    #[arg(long, value_enum, default_value_t = ProgressArg::Auto)]
    progress: ProgressArg,
}

#[derive(Debug, Args)]
struct ImportArgs {
    #[arg(
        long,
        value_parser = parse_native_provider_arg,
        hide_possible_values = true,
        help = "Import one provider, for example codex, claude, cursor, pi, copilot-cli, or opencode"
    )]
    provider: Option<NativeProviderArg>,
    #[arg(
        long,
        help = "Import exactly this path; native provider paths require --provider"
    )]
    path: Option<PathBuf>,
    #[arg(long = "history-source", conflicts_with_all = ["provider", "path", "format", "all"])]
    history_source: Option<String>,
    #[arg(
        long = "history-source-manifest",
        conflicts_with_all = ["provider", "path", "format"]
    )]
    history_source_manifest: Vec<PathBuf>,
    #[arg(long = "reset-cursor")]
    reset_cursor: bool,
    #[arg(
        long,
        value_enum,
        requires = "path",
        conflicts_with_all = ["provider", "all", "history_source"]
    )]
    format: Option<ImportFormatArg>,
    #[arg(long, conflicts_with_all = ["provider", "path", "format", "history_source"])]
    all: bool,
    #[arg(long)]
    resume: bool,
    #[arg(
        long,
        help = "Allow valid rows in a source to commit when malformed rows are encountered"
    )]
    partial: bool,
    #[arg(long, help = "Do not start daemon maintenance after import")]
    no_daemon: bool,
    #[arg(long)]
    json: bool,
    #[arg(long, value_enum, default_value_t = ProgressArg::Auto)]
    progress: ProgressArg,
}

impl ImportArgs {
    fn should_autostart_daemon(&self) -> bool {
        !self.no_daemon
            && self.format.is_none()
            && self.history_source.is_none()
            && self.history_source_manifest.is_empty()
    }
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
    #[arg(help = "ctx session id or unambiguous id prefix")]
    id: Option<String>,
    #[arg(long, value_parser = parse_provider_arg)]
    #[arg(hide_possible_values = true)]
    provider: Option<ProviderArg>,
    #[arg(long = "provider-session")]
    provider_session: Option<String>,
    #[arg(long, value_enum, default_value_t = TranscriptMode::Lite)]
    mode: TranscriptMode,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ShowEventArgs {
    #[arg(help = "ctx event id or unambiguous id prefix")]
    id: String,
    #[arg(long, default_value_t = 0, value_parser = parse_event_window_limit)]
    before: usize,
    #[arg(long, default_value_t = 0, value_parser = parse_event_window_limit)]
    after: usize,
    #[arg(long, value_parser = parse_event_window_limit)]
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
    #[arg(help = "ctx session id or unambiguous id prefix")]
    id: Option<String>,
    #[arg(long, value_parser = parse_provider_arg)]
    #[arg(hide_possible_values = true)]
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
    #[arg(help = "ctx event id or unambiguous id prefix")]
    id: String,
    #[arg(long, value_enum, default_value_t = LocateFormat::Text)]
    format: LocateFormat,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct SearchArgs {
    #[arg(help = "Natural-language query to search local agent history")]
    query: Option<String>,
    #[arg(
        long,
        help = "Add another search query or keyword; repeat to broaden with OR-style merged results"
    )]
    term: Vec<String>,
    #[arg(
        long,
        default_value_t = 20,
        value_parser = parse_search_limit,
        help = "Maximum results to return, from 1 to 200"
    )]
    limit: usize,
    #[arg(
        long,
        value_parser = parse_provider_arg,
        hide_possible_values = true,
        help = "Search only one provider, for example codex, claude, cursor, pi, copilot-cli, or opencode"
    )]
    provider: Option<ProviderArg>,
    #[arg(
        long = "history-source",
        help = "Filter custom history imports by plugin/source or provider_key/source_id"
    )]
    history_source: Option<String>,
    #[arg(
        long = "provider-key",
        help = "Filter custom history imports by provider_key"
    )]
    provider_key: Option<String>,
    #[arg(
        long = "source-id",
        help = "Filter custom history imports by source_id"
    )]
    source_id: Option<String>,
    #[arg(
        long = "source-format",
        help = "Filter custom history imports by source_format"
    )]
    source_format: Option<String>,
    #[arg(
        long,
        help = "Filter by stored workspace, cwd, source path, or repo-name text"
    )]
    workspace: Option<String>,
    #[arg(
        long,
        help = "Filter to recent history, as RFC3339 or a day window like 30d"
    )]
    since: Option<String>,
    #[arg(
        long,
        hide = true,
        help = "Deprecated alias for the default primary-agent search scope"
    )]
    primary_only: bool,
    #[arg(
        long,
        help = "Include subagent sessions in addition to primary-agent sessions"
    )]
    include_subagents: bool,
    #[arg(
        long,
        help = "Filter by event type: message, tool_call, tool_output, command_started, command_output, command_finished, file_touched, vcs_change, artifact, summary, or notice"
    )]
    event_type: Option<String>,
    #[arg(
        long,
        help = "Filter by indexed touched-file path metadata, not the current filesystem"
    )]
    file: Option<PathBuf>,
    #[arg(
        long,
        help = "Search event hits within one ctx session id or unambiguous id prefix"
    )]
    session: Option<String>,
    #[arg(
        long,
        help = "Return dense event-level results instead of diverse session results"
    )]
    events: bool,
    #[arg(
        long,
        value_enum,
        help = "Search backend override: hybrid, semantic, or lexical",
        long_help = "Search backend override. By default ctx uses lexical search unless local semantic search is enabled in config, then hybrid. hybrid combines SQLite FTS/BM25 and semantic vector evidence; lexical uses only the SQLite FTS/BM25 path; semantic requires local semantic search to be enabled and ready."
    )]
    backend: Option<SearchBackendArg>,
    #[arg(
        long = "semantic-weight",
        default_value_t = 0.35,
        value_parser = parse_semantic_weight,
        help = "Hybrid ranking weight for semantic evidence, from 0.0 to 1.0"
    )]
    semantic_weight: f32,
    #[arg(
        long,
        value_enum,
        default_value_t = RefreshArg::Background,
        help = "Index freshness behavior: background, off, or wait",
        long_help = "Index freshness behavior. background serves the existing index and lets daemon maintenance refresh history/indexes; off searches the existing index only; wait runs or waits for required refresh work before searching."
    )]
    refresh: RefreshArg,
    #[arg(
        long,
        help = "Include the active Codex session tree when CODEX_THREAD_ID is set"
    )]
    include_current_session: bool,
    #[arg(long, help = "Print machine-readable JSON")]
    json: bool,
    #[arg(
        long,
        help = "Print expanded text details such as full ids, provider ids, citations, and next commands"
    )]
    verbose: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum SearchBackendArg {
    Hybrid,
    Lexical,
    Semantic,
}

impl SearchBackendArg {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Hybrid => "hybrid",
            Self::Lexical => "lexical",
            Self::Semantic => "semantic",
        }
    }
}

#[derive(Debug, Args, Clone)]
pub(crate) struct DaemonArgs {
    #[command(subcommand)]
    command: DaemonCommand,
}

#[derive(Debug, Subcommand, Clone)]
pub(crate) enum DaemonCommand {
    #[command(about = "Run ctx background maintenance in the foreground")]
    Run(DaemonRunArgs),
    #[command(about = "Show ctx daemon status")]
    Status(JsonArgs),
    #[command(about = "Enable ctx daemon maintenance")]
    Enable(JsonArgs),
    #[command(about = "Disable ctx daemon maintenance")]
    Disable(JsonArgs),
}

#[derive(Debug, Args, Clone)]
pub(crate) struct DaemonRunArgs {
    #[arg(long, conflicts_with = "once", hide = true)]
    foreground: bool,
    #[arg(long, help = "Run one maintenance pass and exit")]
    once: bool,
    #[arg(long, value_parser = parse_daemon_idle_exit_seconds)]
    idle_exit_seconds: Option<u64>,
    #[arg(long, value_parser = parse_daemon_interval_seconds)]
    loop_interval_seconds: Option<u64>,
    #[arg(long, value_parser = parse_semantic_worker_batch)]
    max_chunks: Option<usize>,
    #[arg(skip)]
    max_seconds: Option<u64>,
    #[arg(long, help = "Run even when daemon.enabled is false")]
    force: bool,
    #[arg(long, value_enum, hide = true)]
    start_mode: Option<DaemonStartModeArg>,
    #[arg(long, value_enum, hide = true)]
    trigger_command: Option<DaemonTriggerCommandArg>,
    #[arg(long, help = "Print machine-readable JSON")]
    json: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum DaemonStartModeArg {
    Auto,
    Manual,
}

impl DaemonStartModeArg {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum DaemonTriggerCommandArg {
    Setup,
    Import,
    Search,
}

impl DaemonTriggerCommandArg {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Import => "import",
            Self::Search => "search",
        }
    }
}

#[derive(Debug, Args)]
struct SqlArgs {
    #[arg(help = "Read-only SQL statement to run; pass '-' to read SQL from stdin")]
    sql: Option<String>,
    #[arg(long, conflicts_with = "sql", help = "Read SQL from a file")]
    file: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = SqlFormat::Table)]
    format: SqlFormat,
    #[arg(long, help = "Alias for --format json")]
    json: bool,
    #[arg(long, default_value_t = RAW_SQL_DEFAULT_MAX_ROWS)]
    max_rows: usize,
    #[arg(long, default_value_t = RAW_SQL_DEFAULT_MAX_COLUMNS)]
    max_columns: usize,
    #[arg(long, default_value_t = RAW_SQL_DEFAULT_MAX_VALUE_BYTES)]
    max_value_bytes: usize,
    #[arg(long, default_value_t = RAW_SQL_DEFAULT_MAX_SQL_BYTES)]
    max_sql_bytes: usize,
    #[arg(long, default_value = "10s", value_parser = parse_sql_timeout)]
    timeout: StdDuration,
    #[arg(long, help = "Omit the header row for CSV output")]
    no_header: bool,
}

impl SqlArgs {
    fn output_format(&self) -> SqlFormat {
        if self.json {
            SqlFormat::Json
        } else {
            self.format
        }
    }

    fn json_output(&self) -> bool {
        self.output_format() == SqlFormat::Json
    }
}

impl CommandRoot {
    fn name(&self) -> &'static str {
        match self {
            Self::Setup(_) => "setup",
            Self::Status(_) => "status",
            Self::Index(_) => "index",
            Self::Sources(_) => "sources",
            Self::Import(_) => "import",
            Self::Show(_) => "show",
            Self::Locate(_) => "locate",
            Self::Search(_) => "search",
            Self::Sql(_) => "sql",
            Self::Docs(_) => "docs",
            Self::Integrations(_) => "integrations",
            Self::Mcp(_) => "mcp",
            Self::Daemon(_) => "daemon",
            Self::Upgrade(_) => "upgrade",
            Self::Doctor(_) => "doctor",
        }
    }

    fn sends_analytics(&self) -> bool {
        match self {
            Self::Status(_) | Self::Index(_) | Self::Sql(_) | Self::Mcp(_) => false,
            Self::Daemon(args) => !matches!(&args.command, DaemonCommand::Status(_)),
            _ => true,
        }
    }

    fn json_output(&self) -> bool {
        match self {
            Self::Setup(args) => args.json,
            Self::Status(args) => args.json,
            Self::Index(args) => args.json_output(),
            Self::Sources(args) => args.json,
            Self::Import(args) => args.json,
            Self::Show(args) => args.json_output(),
            Self::Locate(args) => args.json_output(),
            Self::Search(args) => args.json,
            Self::Sql(args) => args.json_output(),
            Self::Docs(args) => args.json_output(),
            Self::Integrations(args) => args.json_output(),
            Self::Mcp(_) => false,
            Self::Daemon(args) => match &args.command {
                DaemonCommand::Run(args) => args.json,
                DaemonCommand::Status(args)
                | DaemonCommand::Enable(args)
                | DaemonCommand::Disable(args) => args.json,
            },
            Self::Upgrade(args) => args.json_output(),
            Self::Doctor(args) => args.json,
        }
    }

    fn allows_background_upgrade(&self) -> bool {
        !matches!(
            self,
            Self::Status(_)
                | Self::Index(_)
                | Self::Docs(_)
                | Self::Mcp(_)
                | Self::Sql(_)
                | Self::Upgrade(_)
                | Self::Daemon(_)
        )
    }

    fn daemon_autostart_trigger(&self) -> Option<DaemonTriggerCommandArg> {
        match self {
            Self::Setup(args) if !args.catalog_only && !args.no_daemon => {
                Some(DaemonTriggerCommandArg::Setup)
            }
            Self::Import(args) if args.should_autostart_daemon() => {
                Some(DaemonTriggerCommandArg::Import)
            }
            _ => None,
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

fn main() -> Result<()> {
    let started = Instant::now();
    let cli = Cli::parse();
    let action = cli.command.name();
    let sends_analytics = cli.command.sends_analytics();
    let is_setup = matches!(&cli.command, CommandRoot::Setup(_));
    let json_output = cli.command.json_output();
    let allow_background_upgrade = cli.command.allows_background_upgrade();
    let daemon_autostart_trigger = cli.command.daemon_autostart_trigger();
    let mut analytics_properties = command_analytics_properties(&cli.command);
    let quiet = quiet_output(cli.quiet);
    let data_root = cli
        .data_root
        .clone()
        .map(Ok)
        .unwrap_or_else(default_data_root)
        .context("resolve ctx data root")?;
    let config = AppConfig::load(&data_root)?;
    if is_setup && sends_analytics {
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
        CommandRoot::Setup(args) => run_setup(
            args,
            data_root.clone(),
            &mut analytics_properties,
            quiet,
            &config,
        ),
        CommandRoot::Status(args) => run_status(args, data_root.clone(), quiet),
        CommandRoot::Index(args) => run_index(args, data_root.clone(), quiet),
        CommandRoot::Sources(args) => {
            run_sources(args, data_root.clone(), &mut analytics_properties)
        }
        CommandRoot::Import(args) => run_import(args, data_root.clone(), &mut analytics_properties),
        CommandRoot::Show(args) => run_show(args, data_root.clone(), &mut analytics_properties),
        CommandRoot::Locate(args) => run_locate(args, data_root.clone(), &mut analytics_properties),
        CommandRoot::Search(args) => {
            run_search(args, data_root.clone(), &mut analytics_properties, &config)
        }
        CommandRoot::Sql(args) => run_sql(args, data_root.clone()),
        CommandRoot::Docs(args) => docs::run(args),
        CommandRoot::Integrations(args) => integrations::run(args, &mut analytics_properties),
        CommandRoot::Mcp(args) => mcp::run(args, data_root.clone()),
        CommandRoot::Daemon(args) => semantic::run_daemon_command(args, data_root.clone(), &config),
        CommandRoot::Upgrade(args) => upgrade::run(
            args,
            data_root.clone(),
            config.clone(),
            &mut analytics_properties,
        ),
        CommandRoot::Doctor(args) => run_doctor(args, data_root.clone(), &mut analytics_properties),
    };
    if is_setup {
        analytics::insert_bool(&mut analytics_properties, "setup_completed", result.is_ok());
        analytics::insert_str(
            &mut analytics_properties,
            "setup_result",
            if result.is_ok() { "success" } else { "failure" },
        );
    }
    if result.is_ok() && allow_background_upgrade {
        upgrade::maybe_spawn_auto_upgrade(
            &data_root,
            &config,
            json_output,
            &mut analytics_properties,
        );
    }
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
    if result.is_ok() {
        if let Some(trigger) = daemon_autostart_trigger {
            semantic::maybe_autostart_daemon(&data_root, &config, trigger, json_output);
        }
    }
    result
}

fn command_analytics_properties(command: &CommandRoot) -> AnalyticsProperties {
    let mut properties = analytics::empty_properties();
    match command {
        CommandRoot::Setup(args) => {
            analytics::insert_bool(&mut properties, "catalog_only", args.catalog_only);
            analytics::insert_bool(&mut properties, "no_daemon", args.no_daemon);
            analytics::insert_str(
                &mut properties,
                "progress_mode",
                progress_mode_name(args.progress),
            );
        }
        CommandRoot::Status(_)
        | CommandRoot::Index(_)
        | CommandRoot::Sources(_)
        | CommandRoot::Sql(_)
        | CommandRoot::Doctor(_) => {}
        CommandRoot::Import(args) => {
            analytics::insert_bool(&mut properties, "resume", args.resume);
            analytics::insert_bool(&mut properties, "all_sources", args.all);
            analytics::insert_bool(&mut properties, "no_daemon", args.no_daemon);
            analytics::insert_str(
                &mut properties,
                "source_mode",
                if args.format.is_some() {
                    "explicit_format"
                } else if args.history_source.is_some() {
                    "history_source_plugin"
                } else if args.path.is_some() {
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
            analytics::insert_bool(&mut properties, "reset_cursor", args.reset_cursor);
            analytics::insert_str(
                &mut properties,
                "progress_mode",
                progress_mode_name(args.progress),
            );
        }
        CommandRoot::Show(args) => match &args.target {
            ShowTarget::Session(args) => {
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
        CommandRoot::Search(args) => {
            analytics::insert_bool(&mut properties, "has_query", args.query.is_some());
            analytics::insert_bool(
                &mut properties,
                "has_provider_filter",
                args.provider.is_some(),
            );
            analytics::insert_bool(
                &mut properties,
                "has_workspace_filter",
                args.workspace.is_some(),
            );
            analytics::insert_bool(&mut properties, "has_since_filter", args.since.is_some());
            analytics::insert_bool(
                &mut properties,
                "has_event_type_filter",
                args.event_type.is_some(),
            );
            analytics::insert_bool(&mut properties, "has_file_filter", args.file.is_some());
            analytics::insert_bool(
                &mut properties,
                "has_session_filter",
                args.session.is_some(),
            );
            analytics::insert_bool(
                &mut properties,
                "event_results",
                args.events || args.session.is_some(),
            );
            analytics::insert_bool(&mut properties, "primary_only", args.primary_only);
            analytics::insert_bool(&mut properties, "include_subagents", args.include_subagents);
            analytics::insert_bool(
                &mut properties,
                "include_current_session",
                args.include_current_session,
            );
            analytics::insert_count_bucket(&mut properties, "limit_bucket", args.limit as u64);
            if let Some(provider) = args.provider {
                analytics::insert_str(
                    &mut properties,
                    "provider_filter",
                    provider.capture_provider().as_str(),
                );
            }
        }
        CommandRoot::Mcp(_) => {}
        CommandRoot::Daemon(args) => match &args.command {
            DaemonCommand::Run(args) => {
                analytics::insert_str(&mut properties, "daemon_command", "run");
                analytics::insert_bool(&mut properties, "once", args.once);
                analytics::insert_bool(&mut properties, "force", args.force);
                if let Some(start_mode) = args.start_mode {
                    analytics::insert_str(&mut properties, "start_mode", start_mode.as_str());
                }
                if let Some(trigger) = args.trigger_command {
                    analytics::insert_str(&mut properties, "trigger_command", trigger.as_str());
                }
            }
            DaemonCommand::Status(_) => {
                analytics::insert_str(&mut properties, "daemon_command", "status");
            }
            DaemonCommand::Enable(_) => {
                analytics::insert_str(&mut properties, "daemon_command", "enable");
            }
            DaemonCommand::Disable(_) => {
                analytics::insert_str(&mut properties, "daemon_command", "disable");
            }
        },
        CommandRoot::Docs(_) => {}
        CommandRoot::Integrations(args) => {
            args.add_initial_analytics(&mut properties);
        }
        CommandRoot::Upgrade(args) => {
            analytics::insert_bool(&mut properties, "dry_run", args.dry_run);
            analytics::insert_bool(&mut properties, "background", args.background());
            analytics::insert_str(&mut properties, "upgrade_mode", args.mode());
            analytics::insert_str(&mut properties, "upgrade_operation", args.operation());
        }
    }
    properties
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

fn quiet_output(flag: bool) -> bool {
    flag || env_truthy("CTX_QUIET")
}

fn env_truthy(key: &str) -> bool {
    env::var_os(key).is_some_and(|value| {
        let value = value.to_string_lossy();
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        )
    })
}

fn parse_semantic_weight(value: &str) -> std::result::Result<f32, String> {
    let weight = value
        .parse::<f32>()
        .map_err(|err| format!("invalid semantic weight: {err}"))?;
    if !(0.0..=1.0).contains(&weight) || !weight.is_finite() {
        return Err("semantic weight must be between 0.0 and 1.0".to_owned());
    }
    Ok(weight)
}

fn parse_semantic_worker_batch(value: &str) -> std::result::Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|err| format!("invalid semantic worker batch: {err}"))?;
    if parsed == 0 || parsed > semantic::SEMANTIC_WORKER_BATCH_MAX {
        return Err(format!(
            "semantic worker batch must be between 1 and {}",
            semantic::SEMANTIC_WORKER_BATCH_MAX
        ));
    }
    Ok(parsed)
}

fn parse_daemon_idle_exit_seconds(value: &str) -> std::result::Result<u64, String> {
    let parsed = value
        .parse::<u64>()
        .map_err(|err| format!("invalid daemon seconds: {err}"))?;
    if parsed == 0 || parsed > semantic::DAEMON_IDLE_EXIT_SECONDS_CAP {
        return Err(format!(
            "daemon seconds must be between 1 and {}",
            semantic::DAEMON_IDLE_EXIT_SECONDS_CAP
        ));
    }
    Ok(parsed)
}

fn parse_daemon_interval_seconds(value: &str) -> std::result::Result<u64, String> {
    let parsed = value
        .parse::<u64>()
        .map_err(|err| format!("invalid daemon loop interval seconds: {err}"))?;
    if !(1..=3_600).contains(&parsed) {
        return Err("daemon loop interval seconds must be between 1 and 3600".to_owned());
    }
    Ok(parsed)
}

fn parse_event_window_limit(value: &str) -> std::result::Result<usize, String> {
    let limit = value
        .parse::<usize>()
        .map_err(|err| format!("invalid event window: {err}"))?;
    if limit > MAX_EVENT_WINDOW {
        return Err(format!(
            "event window must be between 0 and {MAX_EVENT_WINDOW}"
        ));
    }
    Ok(limit)
}

#[cfg(test)]
mod tests {
    use super::{parse_event_window_limit, parse_search_limit, parse_sql_timeout};
    use crate::commands::import::{catalog_import_checkpoint_matches, sha256_file_prefix_hex};
    use crate::search_filters::parse_since_filter;
    use crate::transcript::{normalize_uuid_prefix, shell_quote_arg};
    use std::{fs, io::Write, panic};
    use tempfile::tempdir;

    #[test]
    fn shell_quote_arg_uses_single_quotes_for_shell_metacharacters() {
        assert_eq!(shell_quote_arg("onboarding"), "onboarding");
        assert_eq!(
            shell_quote_arg("$(touch /tmp/ctx-owned)'s"),
            "'$(touch /tmp/ctx-owned)'\\''s'"
        );
    }

    #[test]
    fn parse_since_filter_rejects_large_day_window() {
        let err = parse_since_filter("500000000d").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("invalid --since day window"),
            "expected error about invalid day window, got: {msg}"
        );
    }

    #[test]
    fn cli_value_parsers_do_not_panic_on_adversarial_inputs() {
        let inputs = [
            "",
            " ",
            "0",
            "-1",
            "1",
            "30d",
            "500000000d",
            "9223372036854775807d",
            "-9223372036854775808d",
            "999999999999999999999999999999d",
            "NaN",
            "inf",
            "1e309",
            "1.5d",
            "1970-01-01T00:00:00Z",
            "999999-99-99T99:99:99Z",
            "zzzzzzzz",
            "ffffffff",
            "ffffffff-ffff-ffff-ffff-ffffffffffff",
            "\0",
            "１２３",
        ];

        for input in inputs {
            assert!(
                panic::catch_unwind(|| parse_since_filter(input)).is_ok(),
                "parse_since_filter panicked for {input:?}"
            );
            assert!(
                panic::catch_unwind(|| parse_search_limit(input)).is_ok(),
                "parse_search_limit panicked for {input:?}"
            );
            assert!(
                panic::catch_unwind(|| parse_event_window_limit(input)).is_ok(),
                "parse_event_window_limit panicked for {input:?}"
            );
            assert!(
                panic::catch_unwind(|| parse_sql_timeout(input)).is_ok(),
                "parse_sql_timeout panicked for {input:?}"
            );
            assert!(
                panic::catch_unwind(|| normalize_uuid_prefix(input, "test")).is_ok(),
                "normalize_uuid_prefix panicked for {input:?}"
            );
        }
    }

    #[test]
    fn catalog_import_checkpoint_requires_matching_hash() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("session.jsonl");
        {
            let mut file = fs::File::create(&path).unwrap();
            writeln!(file, "prefix").unwrap();
        }
        let prefix_hash = sha256_file_prefix_hex(&path, 7).unwrap();
        assert!(catalog_import_checkpoint_matches(&path, 7, Some(&prefix_hash)).unwrap());
        assert!(catalog_import_checkpoint_matches(&path, 7, None).unwrap());

        fs::write(&path, "mutated\n").unwrap();
        assert!(!catalog_import_checkpoint_matches(&path, 7, Some(&prefix_hash)).unwrap());
    }
}
