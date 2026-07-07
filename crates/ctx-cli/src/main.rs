use std::{
    env,
    path::PathBuf,
    time::{Duration as StdDuration, Instant},
};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};

mod analytics;
mod commands;
mod config;
mod docs;
mod history_source_plugins;
mod identity;
mod install_marker;
mod mcp;
mod net;
mod output;
mod progress;
mod provider_args;
mod provider_sources;
mod search_filters;
mod search_render;
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
    doctor::run_doctor, import::run_import, locate::run_locate, search::run_search,
    setup::run_setup, show::run_show, sources::run_sources, sql::run_sql, status::run_status,
};
use config::AppConfig;
use ctx_history_core::{default_data_root, CaptureProvider};
use ctx_history_store::{
    RAW_SQL_DEFAULT_MAX_COLUMNS, RAW_SQL_DEFAULT_MAX_ROWS, RAW_SQL_DEFAULT_MAX_SQL_BYTES,
    RAW_SQL_DEFAULT_MAX_VALUE_BYTES,
};
use output::{compact_json, mark_share_safe, LocateFormat, OutputFormat, SqlFormat};
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
    #[command(about = "Install or inspect the bundled ctx agent skill")]
    Skill(skill::SkillArgs),
    #[command(about = "Serve read-only ctx tools over MCP")]
    Mcp(mcp::McpArgs),
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
    #[arg(long)]
    json: bool,
    #[arg(long, value_enum, default_value_t = ProgressArg::Auto)]
    progress: ProgressArg,
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
        default_value_t = RefreshArg::Auto,
        help = "Pre-search refresh behavior: auto, off, or strict",
        long_help = "Pre-search refresh behavior. auto best-effort refreshes discovered native provider sources and enabled auto history-source plugins, then serves the existing index if refresh fails; off searches the existing index only; strict fails if the refresh cannot run or import successfully."
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
            Self::Sources(_) => "sources",
            Self::Import(_) => "import",
            Self::Show(_) => "show",
            Self::Locate(_) => "locate",
            Self::Search(_) => "search",
            Self::Sql(_) => "sql",
            Self::Docs(_) => "docs",
            Self::Skill(_) => "skill",
            Self::Mcp(_) => "mcp",
            Self::Upgrade(_) => "upgrade",
            Self::Doctor(_) => "doctor",
        }
    }

    fn sends_analytics(&self) -> bool {
        !matches!(self, Self::Status(_) | Self::Sql(_) | Self::Mcp(_))
    }

    fn json_output(&self) -> bool {
        match self {
            Self::Setup(args) => args.json,
            Self::Status(args) => args.json,
            Self::Sources(args) => args.json,
            Self::Import(args) => args.json,
            Self::Show(args) => args.json_output(),
            Self::Locate(args) => args.json_output(),
            Self::Search(args) => args.json,
            Self::Sql(args) => args.json_output(),
            Self::Docs(args) => args.json_output(),
            Self::Skill(args) => args.json_output(),
            Self::Mcp(_) => false,
            Self::Upgrade(args) => args.json_output(),
            Self::Doctor(args) => args.json,
        }
    }

    fn allows_background_upgrade(&self) -> bool {
        !matches!(
            self,
            Self::Status(_) | Self::Docs(_) | Self::Mcp(_) | Self::Sql(_) | Self::Upgrade(_)
        )
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
        CommandRoot::Setup(args) => {
            run_setup(args, data_root.clone(), &mut analytics_properties, quiet)
        }
        CommandRoot::Status(args) => run_status(args, data_root.clone(), quiet),
        CommandRoot::Sources(args) => {
            run_sources(args, data_root.clone(), &mut analytics_properties)
        }
        CommandRoot::Import(args) => run_import(args, data_root.clone(), &mut analytics_properties),
        CommandRoot::Show(args) => run_show(args, data_root.clone(), &mut analytics_properties),
        CommandRoot::Locate(args) => run_locate(args, data_root.clone(), &mut analytics_properties),
        CommandRoot::Search(args) => run_search(args, data_root.clone(), &mut analytics_properties),
        CommandRoot::Sql(args) => run_sql(args, data_root.clone()),
        CommandRoot::Docs(args) => docs::run(args),
        CommandRoot::Skill(args) => skill::run(args, &mut analytics_properties),
        CommandRoot::Mcp(args) => mcp::run(args, data_root.clone()),
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
        | CommandRoot::Sql(_)
        | CommandRoot::Doctor(_) => {}
        CommandRoot::Import(args) => {
            analytics::insert_bool(&mut properties, "resume", args.resume);
            analytics::insert_bool(&mut properties, "all_sources", args.all);
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
        CommandRoot::Docs(_) => {}
        CommandRoot::Skill(args) => {
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
