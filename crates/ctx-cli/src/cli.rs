use std::{path::PathBuf, time::Duration as StdDuration};

use clap::{Args, Parser, Subcommand, ValueEnum};
use ctx_history_relational::{
    RAW_SQL_DEFAULT_MAX_COLUMNS, RAW_SQL_DEFAULT_MAX_ROWS, RAW_SQL_DEFAULT_MAX_SQL_BYTES,
    RAW_SQL_DEFAULT_MAX_VALUE_BYTES,
};

use crate::{
    commands,
    commands::{
        search::RefreshArg,
        show::{ShowEventArgs, ShowSessionArgs},
        sql::parse_sql_timeout,
    },
    docs, integrations, mcp,
    output::{JsonOutputFormat, SqlFormat},
    pro,
    progress::ProgressArg,
    provider_args::{
        parse_native_provider_arg, parse_provider_arg, ImportFormatArg, NativeProviderArg,
        ProviderArg,
    },
    semantic, upgrade,
    value_parsers::parse_daemon_interval_seconds,
};

pub(crate) const MAX_SEARCH_LIMIT: usize = 200;
pub(crate) const MAX_EVENT_WINDOW: usize = 50;

#[derive(Debug, Parser)]
#[command(name = "ctx", version, about = "Search local agent history")]
pub(crate) struct Cli {
    #[arg(long, env = "CTX_DATA_ROOT", global = true)]
    pub(crate) data_root: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        help = "Suppress non-essential setup/status output (also via CTX_QUIET=1)"
    )]
    pub(crate) quiet: bool,
    #[command(subcommand)]
    pub(crate) command: CommandRoot,
}

#[derive(Debug, Subcommand)]
pub(crate) enum CommandRoot {
    #[command(about = "Create local ctx storage and index discovered history")]
    Setup(SetupArgs),
    #[command(about = "Show local ctx index and usage status")]
    Status(StatusArgs),
    #[command(about = "Show, watch, or wait for local indexing progress")]
    Index(commands::index::IndexArgs),
    #[command(about = "List configured and discovered agent history sources")]
    Sources(SourcesArgs),
    #[command(about = "Index provider history into local search")]
    Import(ImportArgs),
    #[command(about = "Show an indexed session or event")]
    Show(ShowArgs),
    #[command(about = "Locate evidence for an indexed session or event")]
    Locate(LocateArgs),
    #[command(about = "Search indexed agent history")]
    Search(SearchArgs),
    #[command(
        about = "Set up, resume, repair, manage, or remove local ctx Pro",
        long_about = "Set up, resume, repair, manage, or remove local ctx Pro. Bare `ctx pro` runs the idempotent setup path; `ctx pro setup` is an explicit synonym. `ctx status` does not mutate canonical history or graph data; entitlement authorization may advance nonsecret anti-clock-rollback metadata."
    )]
    Pro(pro::ProArgs),
    #[command(
        about = "Refer a developer. Earn $10/month toward your agent bill.",
        long_about = "Refer a developer. Earn $10/month toward your agent bill.\n\nUp to $120 per friend. Earn $10 for each of the first 12 distinct qualifying paid monthly invoices from a directly attributed subscription. The first two commissions remain pending until invoice 2 settles and its 14-day hold and authoritative reconciliation complete; invoices 3-12 each have their own 14-day hold and reconciliation. Create a codename, view aggregate ledger totals, or set up payouts."
    )]
    Referral(pro::ReferralArgs),
    #[command(about = "Show cited agent provenance for committed code or a pull request")]
    Blame(commands::blame::BlameArgs),
    #[command(about = "Run read-only SQL against the local ctx index")]
    Sql(SqlArgs),
    #[command(about = "Read embedded ctx documentation")]
    Docs(docs::DocsArgs),
    #[command(about = "Install or inspect ctx integrations")]
    Integrations(integrations::IntegrationsArgs),
    #[command(about = "Serve local ctx tools over MCP")]
    Mcp(mcp::McpArgs),
    #[command(about = "Run or inspect local ctx background maintenance")]
    Daemon(DaemonArgs),
    #[command(about = "Check or apply signed ctx CLI upgrades")]
    Upgrade(upgrade::UpgradeArgs),
    #[command(about = "Check local ctx health")]
    Doctor(DoctorArgs),
}

#[derive(Debug, Args)]
pub(crate) struct SetupArgs {
    #[arg(
        long,
        alias = "no-import",
        help = "Prepare local history inventory without importing searchable history"
    )]
    pub(crate) catalog_only: bool,
    #[arg(
        long,
        help = "Enable local semantic search in config (requires daemon maintenance)"
    )]
    pub(crate) semantic: bool,
    #[arg(long, help = "Do not start daemon maintenance after setup")]
    pub(crate) no_daemon: bool,
    #[arg(long, help = "Wait for foreground lexical indexing before returning")]
    pub(crate) wait: bool,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub(crate) format: JsonOutputFormat,
    #[arg(long, value_enum, default_value_t = ProgressArg::Auto)]
    pub(crate) progress: ProgressArg,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct FormatArgs {
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub(crate) format: JsonOutputFormat,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct StatusArgs {
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub(crate) format: JsonOutputFormat,
    #[arg(
        long,
        value_enum,
        default_value_t = UsageStatusMode::Summary,
        help = "Local usage report/control: summary, detail, enable, disable, or reset"
    )]
    pub(crate) usage: UsageStatusMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum UsageStatusMode {
    Summary,
    Detail,
    Enable,
    Disable,
    Reset,
}

impl UsageStatusMode {
    pub(crate) const fn modifies_state(self) -> bool {
        matches!(self, Self::Enable | Self::Disable | Self::Reset)
    }

    pub(crate) const fn detailed(self) -> bool {
        matches!(self, Self::Detail)
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Detail => "detail",
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::Reset => "reset",
        }
    }
}

#[derive(Debug, Args, Clone)]
pub(crate) struct SourcesArgs {
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub(crate) format: JsonOutputFormat,
    #[arg(
        long,
        value_parser = parse_provider_arg,
        hide_possible_values = true,
        help = "Show sources for one provider, for example codex, claude, cursor, pi, copilot-cli, or opencode"
    )]
    pub(crate) provider: Option<ProviderArg>,
    #[arg(long, help = "Show every supported provider location")]
    pub(crate) all: bool,
    #[arg(long, help = "Show missing locations for every known provider")]
    pub(crate) show_missing: bool,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct DoctorArgs {
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub(crate) format: JsonOutputFormat,
}

#[derive(Debug, Args)]
pub(crate) struct ImportArgs {
    #[arg(
        long,
        value_parser = parse_native_provider_arg,
        hide_possible_values = true,
        help = "Import one provider, for example codex, claude, cursor, pi, copilot-cli, or opencode"
    )]
    pub(crate) provider: Option<NativeProviderArg>,
    #[arg(
        long,
        help = "Import exactly this path; native provider paths require --provider"
    )]
    pub(crate) path: Option<PathBuf>,
    #[arg(
        long = "history-source",
        conflicts_with_all = ["provider", "path", "input_format", "all"]
    )]
    pub(crate) history_source: Option<String>,
    #[arg(
        long = "history-source-manifest",
        conflicts_with_all = ["provider", "path", "input_format"]
    )]
    pub(crate) history_source_manifest: Vec<PathBuf>,
    #[arg(long = "reset-cursor")]
    pub(crate) reset_cursor: bool,
    #[arg(
        long = "input-format",
        value_enum,
        requires = "path",
        conflicts_with_all = ["provider", "all", "history_source"]
    )]
    pub(crate) input_format: Option<ImportFormatArg>,
    #[arg(
        long,
        conflicts_with_all = ["provider", "path", "input_format", "history_source"]
    )]
    pub(crate) all: bool,
    #[arg(long)]
    pub(crate) resume: bool,
    #[arg(long, hide = true)]
    pub(crate) partial: bool,
    #[arg(long, help = "Do not start daemon maintenance after import")]
    pub(crate) no_daemon: bool,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub(crate) format: JsonOutputFormat,
    #[arg(long, value_enum, default_value_t = ProgressArg::Auto)]
    pub(crate) progress: ProgressArg,
}

#[derive(Debug, Args)]
pub(crate) struct ShowArgs {
    #[command(subcommand)]
    pub(crate) target: ShowTarget,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ShowTarget {
    #[command(about = "Show a session transcript")]
    Session(ShowSessionArgs),
    #[command(about = "Show one event or a surrounding event window")]
    Event(ShowEventArgs),
}

#[derive(Debug, Args)]
pub(crate) struct LocateArgs {
    #[command(subcommand)]
    pub(crate) target: LocateTarget,
}

#[derive(Debug, Subcommand)]
pub(crate) enum LocateTarget {
    #[command(about = "Locate provider/source metadata for a session")]
    Session(LocateSessionArgs),
    #[command(about = "Locate provider/source metadata for an event")]
    Event(LocateEventArgs),
}

#[derive(Debug, Args)]
pub(crate) struct LocateSessionArgs {
    #[arg(help = "ctx session id or unambiguous id prefix")]
    pub(crate) id: Option<String>,
    #[arg(long, value_parser = parse_provider_arg)]
    #[arg(hide_possible_values = true)]
    pub(crate) provider: Option<ProviderArg>,
    #[arg(long = "provider-session")]
    pub(crate) provider_session: Option<String>,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub(crate) format: JsonOutputFormat,
}

#[derive(Debug, Args)]
pub(crate) struct LocateEventArgs {
    #[arg(help = "ctx event id or unambiguous id prefix")]
    pub(crate) id: String,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub(crate) format: JsonOutputFormat,
}

#[derive(Debug, Args)]
pub(crate) struct SearchArgs {
    #[arg(help = "Natural-language query to search local agent history")]
    pub(crate) query: Option<String>,
    #[arg(
        long,
        help = "Add another search query or keyword; repeat to broaden with OR-style merged results"
    )]
    pub(crate) term: Vec<String>,
    #[arg(
        long,
        default_value_t = 20,
        value_parser = parse_search_limit,
        help = "Maximum results to return, from 1 to 200"
    )]
    pub(crate) limit: usize,
    #[arg(
        long,
        value_parser = parse_provider_arg,
        hide_possible_values = true,
        help = "Search only one provider, for example codex, claude, cursor, pi, copilot-cli, or opencode"
    )]
    pub(crate) provider: Option<ProviderArg>,
    #[arg(
        long = "history-source",
        help = "Filter custom history imports by plugin/source or provider_key/source_id"
    )]
    pub(crate) history_source: Option<String>,
    #[arg(
        long = "provider-key",
        help = "Filter custom history imports by provider_key"
    )]
    pub(crate) provider_key: Option<String>,
    #[arg(
        long = "source-id",
        help = "Filter custom history imports by source_id"
    )]
    pub(crate) source_id: Option<String>,
    #[arg(
        long = "source-format",
        help = "Filter custom history imports by source_format"
    )]
    pub(crate) source_format: Option<String>,
    #[arg(
        long,
        help = "Filter by stored workspace, cwd, source path, or repo-name text"
    )]
    pub(crate) workspace: Option<String>,
    #[arg(
        long,
        help = "Filter to recent history, as RFC3339 or a day window like 30d"
    )]
    pub(crate) since: Option<String>,
    #[arg(
        long,
        hide = true,
        help = "Deprecated alias for the default primary-agent search scope"
    )]
    pub(crate) primary_only: bool,
    #[arg(
        long,
        help = "Include subagent sessions in addition to primary-agent sessions"
    )]
    pub(crate) include_subagents: bool,
    #[arg(
        long,
        help = "Filter by event type: message, tool_call, tool_output, command_started, command_output, command_finished, file_touched, vcs_change, artifact, summary, or notice"
    )]
    pub(crate) event_type: Option<String>,
    #[arg(
        long,
        help = "Filter by indexed touched-file path metadata, not the current filesystem"
    )]
    pub(crate) file: Option<PathBuf>,
    #[arg(
        long,
        help = "Search event hits within one ctx session id or unambiguous id prefix"
    )]
    pub(crate) session: Option<String>,
    #[arg(
        long,
        help = "Return dense event-level results instead of diverse session results"
    )]
    pub(crate) events: bool,
    #[arg(
        long,
        value_enum,
        help = "Search backend override: hybrid, semantic, or lexical",
        long_help = "Search backend override. By default ctx uses lexical search unless local semantic search is enabled in config, then hybrid. hybrid combines Tantivy source-backed lexical evidence and semantic vector evidence; lexical uses only the Tantivy source-backed lexical index; semantic requires local semantic search to be enabled and ready."
    )]
    pub(crate) backend: Option<SearchBackendArg>,
    #[arg(
        long = "semantic-weight",
        default_value_t = 0.35,
        value_parser = parse_semantic_weight,
        help = "Hybrid ranking weight for semantic evidence, from 0.0 to 1.0"
    )]
    pub(crate) semantic_weight: f32,
    #[arg(
        long,
        value_enum,
        default_value_t = RefreshArg::Background,
        help = "Index freshness behavior: background, off, or wait",
        long_help = "Index freshness behavior. background serves the existing index and lets daemon maintenance refresh history/indexes; off searches the existing index only; wait runs or waits for required refresh work before searching."
    )]
    pub(crate) refresh: RefreshArg,
    #[arg(
        long,
        help = "Include the active Codex session tree when CODEX_THREAD_ID is set"
    )]
    pub(crate) include_current_session: bool,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub(crate) format: JsonOutputFormat,
    #[arg(
        long,
        help = "Print expanded text details such as full ids, provider ids, citations, and next commands"
    )]
    pub(crate) verbose: bool,
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
    pub(crate) command: DaemonCommand,
}

#[derive(Debug, Subcommand, Clone)]
pub(crate) enum DaemonCommand {
    #[command(about = "Run ctx background maintenance in the foreground")]
    Run(DaemonRunArgs),
    #[command(about = "Show ctx daemon status")]
    Status(FormatArgs),
    #[command(about = "Enable ctx daemon maintenance")]
    Enable(FormatArgs),
    #[command(about = "Disable ctx daemon maintenance")]
    Disable(FormatArgs),
}

#[derive(Debug, Args, Clone)]
pub(crate) struct DaemonRunArgs {
    #[arg(long, conflicts_with = "once", hide = true)]
    pub(crate) foreground: bool,
    #[arg(long, help = "Run one maintenance pass and exit")]
    pub(crate) once: bool,
    #[arg(long, value_parser = parse_daemon_idle_exit_seconds)]
    pub(crate) idle_exit_seconds: Option<u64>,
    #[arg(long, value_parser = parse_daemon_interval_seconds)]
    pub(crate) loop_interval_seconds: Option<u64>,
    #[arg(long, value_parser = parse_semantic_worker_batch)]
    pub(crate) max_chunks: Option<usize>,
    #[arg(skip)]
    pub(crate) max_seconds: Option<u64>,
    #[arg(long, help = "Run even when daemon.enabled is false")]
    pub(crate) force: bool,
    #[arg(long, value_enum, hide = true)]
    pub(crate) start_mode: Option<DaemonStartModeArg>,
    #[arg(long, value_enum, hide = true)]
    pub(crate) trigger_command: Option<DaemonTriggerCommandArg>,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub(crate) format: JsonOutputFormat,
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
pub(crate) struct SqlArgs {
    #[arg(help = "Read-only SQL statement to run; pass '-' to read SQL from stdin")]
    pub(crate) sql: Option<String>,
    #[arg(long, conflicts_with = "sql", help = "Read SQL from a file")]
    pub(crate) file: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = SqlFormat::Table)]
    pub(crate) format: SqlFormat,
    #[arg(long, default_value_t = RAW_SQL_DEFAULT_MAX_ROWS)]
    pub(crate) max_rows: usize,
    #[arg(long, default_value_t = RAW_SQL_DEFAULT_MAX_COLUMNS)]
    pub(crate) max_columns: usize,
    #[arg(long, default_value_t = RAW_SQL_DEFAULT_MAX_VALUE_BYTES)]
    pub(crate) max_value_bytes: usize,
    #[arg(long, default_value_t = RAW_SQL_DEFAULT_MAX_SQL_BYTES)]
    pub(crate) max_sql_bytes: usize,
    #[arg(long, default_value = "10s", value_parser = parse_sql_timeout)]
    pub(crate) timeout: StdDuration,
    #[arg(long, help = "Omit the header row for CSV output")]
    pub(crate) no_header: bool,
}

impl SqlArgs {
    pub(crate) fn output_format(&self) -> SqlFormat {
        self.format
    }
}

pub(crate) fn parse_search_limit(value: &str) -> Result<usize, String> {
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

fn parse_semantic_weight(value: &str) -> Result<f32, String> {
    let weight = value
        .parse::<f32>()
        .map_err(|err| format!("invalid semantic weight: {err}"))?;
    if !(0.0..=1.0).contains(&weight) || !weight.is_finite() {
        return Err("semantic weight must be between 0.0 and 1.0".to_owned());
    }
    Ok(weight)
}

fn parse_semantic_worker_batch(value: &str) -> Result<usize, String> {
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

fn parse_daemon_idle_exit_seconds(value: &str) -> Result<u64, String> {
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
