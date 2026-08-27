use std::path::PathBuf;

use anstyle::{AnsiColor, Color, Style};
use clap::{Args, Parser, Subcommand, ValueEnum};
use ctx_cli_presentation::commands::{DoctorArgs, SemanticArgs, SetupArgs, StatusArgs};

use crate::{
    commands,
    commands::{
        list::ListArgs, locate::LocateArgs, search::SearchArgs, show::ShowArgs,
        sources::SourcesArgs, stats::StatsArgs,
    },
    docs, integrations, mcp,
    output::JsonOutputFormat,
    progress::ProgressArg,
    provider_args::{parse_native_provider_arg, ImportFormatArg, NativeProviderArg},
    semantic,
    ui::ColorMode,
    upgrade,
    value_parsers::parse_daemon_interval_seconds,
};

#[cfg(test)]
pub(crate) const MAX_EVENT_WINDOW: usize = 50;

#[cfg(test)]
use crate::commands::search::ContentScopeArg;
#[cfg(test)]
pub(crate) use crate::commands::search::{parse_search_limit, MAX_SEARCH_LIMIT};

pub(crate) const CLAP_STYLES: clap::builder::styling::Styles =
    clap::builder::styling::Styles::styled()
        .header(Style::new().bold())
        .usage(Style::new().bold())
        .literal(Style::new().fg_color(Some(Color::Ansi(AnsiColor::Cyan))))
        .placeholder(Style::new().dimmed())
        .error(
            Style::new()
                .fg_color(Some(Color::Ansi(AnsiColor::Red)))
                .bold(),
        )
        .valid(Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green))))
        .invalid(Style::new().fg_color(Some(Color::Ansi(AnsiColor::Yellow))))
        .context(Style::new().dimmed())
        .context_value(Style::new());

/// Clap-facing color argument. Rendering stays terminal-owned and receives
/// this value only after parsing at the CLI composition boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum CliColorMode {
    #[default]
    Auto,
    Always,
    Never,
}

impl From<CliColorMode> for ColorMode {
    fn from(value: CliColorMode) -> Self {
        match value {
            CliColorMode::Auto => Self::Auto,
            CliColorMode::Always => Self::Always,
            CliColorMode::Never => Self::Never,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "ctx",
    bin_name = "ctx",
    version,
    about = "Search local agent history",
    max_term_width = 100,
    styles = crate::cli::CLAP_STYLES
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
    pub(crate) color: CliColorMode,
    #[arg(
        long,
        global = true,
        help = "Suppress non-essential setup/status output (also via CTX_QUIET=1)"
    )]
    pub(crate) quiet: bool,
    #[command(subcommand)]
    pub(crate) command: CommandRoot,
}

#[cfg(test)]
#[derive(Debug, Parser)]
#[command(
    name = commands::index::dashboard_fixture::COMMAND_NAME,
    disable_help_subcommand = true,
    styles = crate::cli::CLAP_STYLES
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
    pub(crate) color: CliColorMode,
}

#[derive(Debug, Subcommand)]
pub(crate) enum CommandRoot {
    #[command(about = "Run companion-owned ctx Pro commands")]
    Pro,
    #[command(about = "Run companion-owned agent provenance")]
    Blame,
    #[command(about = "Run companion-owned referral commands")]
    Referral,
    #[command(about = "Create local ctx storage and index discovered history")]
    Setup(SetupArgs),
    #[command(about = "Manage local semantic search")]
    Semantic(SemanticArgs),
    #[command(about = "Show local ctx index and health status")]
    Status(StatusArgs),
    #[command(about = "Show local history retrieval and value statistics")]
    Stats(StatsArgs),
    #[command(about = "Show or configure local indexing and follow progress")]
    Index(commands::index::IndexArgs),
    #[command(about = "List configured and discovered agent history sources")]
    Sources(SourcesArgs),
    #[command(about = "Index provider history into local search")]
    Import(ImportArgs),
    #[command(about = "Show an indexed session or event")]
    Show(ShowArgs),
    #[command(about = "List filtered events from one immutable Core generation")]
    List(ListArgs),
    #[command(about = "Locate Core source identity for an indexed session or event")]
    Locate(LocateArgs),
    #[command(about = "Search indexed agent history")]
    Search(SearchArgs),
    #[command(about = "Read embedded ctx documentation")]
    Docs(docs::DocsArgs),
    #[command(about = "Install or inspect ctx integrations")]
    Integrations(integrations::IntegrationsArgs),
    #[command(about = "Serve local ctx tools over MCP")]
    Mcp(mcp::McpArgs),
    #[command(about = "Run local ctx background maintenance")]
    Daemon(DaemonArgs),
    #[command(about = "Check or apply signed ctx CLI upgrades")]
    Upgrade(upgrade::UpgradeArgs),
    #[command(about = "Check local ctx health")]
    Doctor(DoctorArgs),
}

#[derive(Debug, Args, Clone)]
pub(crate) struct FormatArgs {
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
        long = "relocate-from",
        requires = "path",
        conflicts_with_all = ["all", "history_source", "history_source_manifest"],
        help = "Relocate one active exact source from this unavailable path to --path"
    )]
    pub(crate) relocate_from: Option<PathBuf>,
    #[arg(
        long = "history-source",
        conflicts_with_all = ["provider", "path", "relocate_from", "input_format", "all"]
    )]
    pub(crate) history_source: Option<String>,
    #[arg(
        long = "history-source-manifest",
        conflicts_with_all = ["provider", "path", "relocate_from", "input_format"]
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
    #[arg(
        long,
        help = "Do not start or restart the daemon; require an already-running daemon"
    )]
    pub(crate) no_daemon: bool,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub(crate) format: JsonOutputFormat,
    #[arg(long, value_enum, default_value_t = ProgressArg::Auto)]
    pub(crate) progress: ProgressArg,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct DaemonArgs {
    #[command(subcommand)]
    pub(crate) command: DaemonCommand,
}

#[derive(Debug, Subcommand, Clone)]
pub(crate) enum DaemonCommand {
    #[command(
        about = "Run ctx background maintenance in the foreground until stopped",
        long_about = "Run ctx background maintenance in the foreground until stopped. This command blocks the terminal and does not change the configured indexing mode."
    )]
    Run(DaemonRunArgs),
    #[command(about = "Show ctx daemon status", hide = true)]
    Status(FormatArgs),
    #[command(
        about = "Use automatic indexing and enable persistent maintenance",
        hide = true
    )]
    Enable(FormatArgs),
    #[command(
        about = "Use manual indexing and remove persistent maintenance",
        hide = true
    )]
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
    #[arg(long, hide = true)]
    pub(crate) finite_core_worker: bool,
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
    #[arg(long, help = "Run even when automatic indexing is disabled")]
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

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum DaemonTriggerCommandArg {
    Setup,
    Import,
    Search,
    Semantic,
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
    fn daemon_run_rejects_removed_lifetime_controls_and_keeps_loop_interval() {
        for removed in ["--once", "--idle-exit-seconds"] {
            let error = Cli::try_parse_from(["ctx", "daemon", "run", removed])
                .expect_err("removed daemon lifetime controls must not parse");
            assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
        }

        let cli =
            Cli::try_parse_from(["ctx", "daemon", "run", "--loop-interval-seconds", "1"]).unwrap();
        let CommandRoot::Daemon(DaemonArgs {
            command: DaemonCommand::Run(args),
        }) = cli.command
        else {
            panic!("expected daemon run command");
        };

        assert_eq!(args.loop_interval_seconds, Some(1));

        let help = Cli::try_parse_from(["ctx", "daemon", "run", "--help"])
            .unwrap_err()
            .to_string();
        for expected in [
            "Wait this many seconds between maintenance passes",
            "Process at most this many semantic chunks per pass",
        ] {
            assert!(help.contains(expected), "{help}");
        }
        assert!(!help.contains("--once"), "{help}");
        assert!(!help.contains("--idle-exit-seconds"), "{help}");
    }

    #[test]
    fn search_content_scope_is_typed_and_conflicts_with_event_type() {
        for (value, expected) in [
            ("all", ContentScopeArg::All),
            ("transcript", ContentScopeArg::Transcript),
            ("calls", ContentScopeArg::Calls),
            ("outputs", ContentScopeArg::Outputs),
        ] {
            let cli = Cli::try_parse_from(["ctx", "search", "needle", "--content-scope", value])
                .expect("documented content scopes must parse");
            let CommandRoot::Search(args) = cli.command else {
                panic!("expected search command");
            };
            assert_eq!(args.content_scope, Some(expected));
        }

        let invalid =
            Cli::try_parse_from(["ctx", "search", "needle", "--content-scope", "messages"])
                .expect_err("unknown content scopes must be rejected by clap");
        assert_eq!(invalid.kind(), clap::error::ErrorKind::InvalidValue);

        for scope in ["all", "transcript", "calls", "outputs"] {
            let conflict = Cli::try_parse_from([
                "ctx",
                "search",
                "needle",
                "--content-scope",
                scope,
                "--event-type",
                "message",
            ])
            .expect_err("content scope and event type must conflict unconditionally");
            assert_eq!(conflict.kind(), clap::error::ErrorKind::ArgumentConflict);
        }
    }
}
