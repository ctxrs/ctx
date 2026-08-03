use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::{
    commands,
    commands::{
        locate::LocateArgs,
        search::RefreshArg,
        show::{ShowEventArgs, ShowEventsArgs, ShowSessionArgs},
    },
    docs, integrations, mcp,
    output::JsonOutputFormat,
    pro,
    progress::ProgressArg,
    provider_args::{
        parse_native_provider_arg, parse_provider_arg, ImportFormatArg, NativeProviderArg,
        ProviderArg,
    },
    semantic,
    ui::ColorMode,
    upgrade,
    value_parsers::parse_daemon_interval_seconds,
};

pub(crate) const MAX_SEARCH_LIMIT: usize = 200;
pub(crate) const MAX_EVENT_WINDOW: usize = 50;

#[derive(Debug, Parser)]
#[command(
    name = "ctx",
    bin_name = "ctx",
    version,
    about = "Search local agent history",
    max_term_width = 100,
    styles = crate::ui::CLAP_STYLES
)]
pub(crate) struct Cli {
    #[arg(long, env = "CTX_DATA_ROOT", hide_env_values = true, global = true)]
    pub(crate) data_root: Option<PathBuf>,
    #[arg(
        long,
        value_enum,
        default_value = "auto",
        global = true,
        help = "Control color in human output: auto, always, or never"
    )]
    pub(crate) color: ColorMode,
    #[arg(
        long,
        global = true,
        help = "Suppress non-essential setup/status output (also via CTX_QUIET=1)"
    )]
    pub(crate) quiet: bool,
    #[command(subcommand)]
    pub(crate) command: CommandRoot,
}

#[cfg(any(test, ctx_pro_test_helper))]
#[derive(Debug, Parser)]
#[command(
    name = commands::index::dashboard_fixture::COMMAND_NAME,
    disable_help_subcommand = true,
    styles = crate::ui::CLAP_STYLES
)]
pub(crate) struct IndexDashboardFixtureArgs {
    #[arg(long, value_enum)]
    pub(crate) case: commands::index::dashboard_fixture::FixtureCase,
    #[arg(
        long,
        value_parser = commands::index::dashboard_fixture::parse_columns
    )]
    pub(crate) columns: usize,
    #[arg(long, value_parser = commands::index::dashboard_fixture::parse_rows)]
    pub(crate) rows: usize,
    #[arg(long)]
    pub(crate) clock: String,
    #[arg(long = "random-seed")]
    pub(crate) random_seed: String,
    #[arg(long, value_enum, default_value = "auto")]
    pub(crate) color: ColorMode,
}

#[derive(Debug, Subcommand)]
pub(crate) enum CommandRoot {
    #[command(about = "Create local ctx storage and index discovered history")]
    Setup(SetupArgs),
    #[command(about = "Show local ctx index and health status")]
    Status(StatusArgs),
    #[command(about = "Show local history retrieval and value statistics")]
    Stats(StatsArgs),
    #[command(about = "Show, watch, or wait for local indexing progress")]
    Index(commands::index::IndexArgs),
    #[command(about = "List configured and discovered agent history sources")]
    Sources(SourcesArgs),
    #[command(about = "Index provider history into local search")]
    Import(ImportArgs),
    #[command(about = "Show an indexed session or event")]
    Show(ShowArgs),
    #[command(about = "Locate Core source identity for an indexed session or event")]
    Locate(LocateArgs),
    #[command(about = "Search indexed agent history")]
    Search(SearchArgs),
    #[command(about = "Export bounded data from one immutable Core generation")]
    Export(commands::export::ExportArgs),
    #[command(
        about = "Set up, resume, repair, manage, or remove local ctx Pro",
        long_about = "Set up, resume, repair, manage, or remove local ctx Pro. Bare `ctx pro` runs the idempotent setup path; `ctx pro setup` is an explicit synonym. `ctx status` does not mutate canonical history or graph data; entitlement authorization may advance nonsecret anti-clock-rollback metadata.",
        after_help = format!("Price: {}", pro::PRO_MONTHLY_PRICE_DISPLAY)
    )]
    Pro(pro::ProArgs),
    #[command(
        about = "Refer a developer. Earn $10/month toward your agent bill.",
        long_about = "Refer a developer. Earn $10/month toward your agent bill.\n\nUp to $120 per friend. Earn $10 for each of the first 12 distinct qualifying paid monthly invoices from a directly attributed subscription. The first two commissions remain pending until invoice 2 settles and its 14-day hold and authoritative reconciliation complete; invoices 3-12 each have their own 14-day hold and reconciliation. Create a codename, view aggregate ledger totals, or set up payouts."
    )]
    Referral(pro::ReferralArgs),
    #[command(about = "Show cited agent provenance for committed code or a pull request")]
    Blame(commands::blame::BlameArgs),
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
    #[arg(
        long,
        help = "Wait for the daemon-owned lexical refresh to publish before returning"
    )]
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
        help = "Local usage control: enable, disable, or reset"
    )]
    pub(crate) usage: Option<UsageStatusMode>,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct StatsArgs {
    #[arg(long, help = "Show CLI/MCP operation and latency breakdowns")]
    pub(crate) detail: bool,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub(crate) format: JsonOutputFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum UsageStatusMode {
    Enable,
    Disable,
    Reset,
}

impl UsageStatusMode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
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
    #[command(about = "Query deterministic Core event pages")]
    Events(Box<ShowEventsArgs>),
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
        long_help = "Search backend override. By default ctx uses lexical search unless local semantic search is enabled in config, then hybrid. hybrid combines self-contained Core lexical evidence and semantic vector evidence; lexical uses only the Tantivy Core index; semantic requires local semantic search to be enabled and ready."
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
    Disable(DaemonDisableArgs),
}

#[derive(Debug, Args, Clone)]
pub(crate) struct DaemonDisableArgs {
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub(crate) format: JsonOutputFormat,
    #[arg(long, hide = true)]
    pub(crate) prepare_uninstall: bool,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct DaemonRunArgs {
    #[arg(long, hide = true)]
    pub(crate) foreground: bool,
    #[arg(
        long,
        value_parser = parse_daemon_idle_exit_seconds,
        help = "Exit after this many seconds without maintenance work"
    )]
    pub(crate) idle_exit_seconds: Option<u64>,
    #[arg(
        long,
        value_parser = parse_daemon_interval_seconds,
        help = "Wait this many seconds between maintenance passes"
    )]
    pub(crate) loop_interval_seconds: Option<u64>,
    #[arg(
        long,
        value_parser = parse_semantic_worker_batch,
        help = "Process at most this many semantic chunks per pass"
    )]
    pub(crate) max_chunks: Option<usize>,
    #[cfg(test)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use std::{env, ffi::OsString, sync::Mutex};

    const CONFIGURED_DATA_ROOT: &str =
        "/configured/ctx-data-root-marker/secret-segment-one/secret-segment-two";
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct DataRootEnvGuard {
        previous: Option<OsString>,
    }

    impl DataRootEnvGuard {
        fn set() -> Self {
            let previous = env::var_os("CTX_DATA_ROOT");
            env::set_var("CTX_DATA_ROOT", CONFIGURED_DATA_ROOT);
            Self { previous }
        }
    }

    impl Drop for DataRootEnvGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(previous) => env::set_var("CTX_DATA_ROOT", previous),
                None => env::remove_var("CTX_DATA_ROOT"),
            }
        }
    }

    fn assert_data_root_help_contract(help: &str, command: &str, width: usize) {
        assert!(
            help.contains("CTX_DATA_ROOT"),
            "{command} help at width {width} omitted CTX_DATA_ROOT:\n{help}"
        );

        let unwrapped = help.split_whitespace().collect::<String>();
        assert!(
            unwrapped.contains("Usage:ctx"),
            "{command} help at width {width} lost the public program name:\n{help}"
        );
        assert!(
            !unwrapped.contains(CONFIGURED_DATA_ROOT),
            "{command} help at width {width} leaked the configured data root:\n{help}"
        );
        for fragment in [
            "ctx-data-root-marker",
            "secret-segment-one",
            "secret-segment-two",
        ] {
            assert!(
                !help.contains(fragment),
                "{command} help at width {width} leaked configured path fragment {fragment}:\n{help}"
            );
        }
    }

    #[test]
    fn root_help_hides_the_configured_data_root_at_narrow_and_wide_widths() {
        let _lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = DataRootEnvGuard::set();

        for width in [32, 80, 100, 120] {
            let help = Cli::command()
                .term_width(width)
                .render_long_help()
                .to_string();
            assert_data_root_help_contract(&help, "root", width);
        }
    }

    #[test]
    fn leaf_help_hides_the_configured_data_root_at_narrow_and_wide_widths() {
        let _lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = DataRootEnvGuard::set();

        for width in [32, 80, 100, 120] {
            let mut command = Cli::command().term_width(width);
            command.build();
            let help = command
                .find_subcommand_mut("search")
                .expect("search must remain a public leaf command")
                .render_long_help()
                .to_string();
            assert_data_root_help_contract(&help, "search", width);
        }
    }

    #[test]
    fn daemon_run_rejects_once_and_keeps_finite_idle_controls() {
        let error = Cli::try_parse_from(["ctx", "daemon", "run", "--once"])
            .expect_err("the removed --once surface must not parse");
        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);

        let cli = Cli::try_parse_from([
            "ctx",
            "daemon",
            "run",
            "--idle-exit-seconds",
            "2",
            "--loop-interval-seconds",
            "1",
        ])
        .unwrap();
        let CommandRoot::Daemon(DaemonArgs {
            command: DaemonCommand::Run(args),
        }) = cli.command
        else {
            panic!("expected daemon run command");
        };

        assert_eq!(args.idle_exit_seconds, Some(2));
        assert_eq!(args.loop_interval_seconds, Some(1));
        assert_eq!(args.max_seconds, None);

        let help = Cli::try_parse_from(["ctx", "daemon", "run", "--help"])
            .unwrap_err()
            .to_string();
        for expected in [
            "Exit after this many seconds without maintenance work",
            "Wait this many seconds between maintenance passes",
            "Process at most this many semantic chunks per pass",
        ] {
            assert!(help.contains(expected), "{help}");
        }
        assert!(!help.contains("--once"), "{help}");
    }
}
