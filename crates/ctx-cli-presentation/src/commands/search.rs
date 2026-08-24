use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, ValueEnum};

use crate::analytics::{
    count_bucket, duration_bucket, text_length_bucket, RefreshMode, RefreshStatus, SearchBackend,
    SearchTelemetry,
};
use crate::local_usage::CliUsage;
use crate::ui::Ui;

pub const MAX_SEARCH_LIMIT: usize = 200;

#[derive(Debug, Args)]
pub struct SearchArgs {
    #[arg(help = "Natural-language query to search local agent history")]
    pub query: Option<String>,
    #[arg(
        long,
        help = "Add another search query or keyword; repeat to broaden with OR-style merged results"
    )]
    pub term: Vec<String>,
    #[arg(long, default_value_t = 20, value_parser = parse_search_limit, help = "Maximum results to return, from 1 to 200")]
    pub limit: usize,
    #[arg(long, value_parser = crate::parse_provider_arg, hide_possible_values = true, help = "Search only one provider, for example codex, claude, cursor, pi, copilot-cli, or opencode")]
    pub provider: Option<crate::ProviderArg>,
    #[arg(
        long = "history-source",
        help = "Filter custom history imports by plugin/source or provider_key/source_id"
    )]
    pub history_source: Option<String>,
    #[arg(
        long = "provider-key",
        help = "Filter custom history imports by provider_key"
    )]
    pub provider_key: Option<String>,
    #[arg(
        long = "source-id",
        help = "Filter custom history imports by source_id"
    )]
    pub source_id: Option<String>,
    #[arg(
        long = "source-format",
        help = "Filter custom history imports by source_format"
    )]
    pub source_format: Option<String>,
    #[arg(
        long = "source-root",
        value_name = "NAME",
        help = "Search one configured provider root; repeat to union roots"
    )]
    pub source_roots: Vec<String>,
    #[arg(
        long = "source-group",
        value_name = "GROUP",
        help = "Search configured provider roots in this group; repeat to union groups"
    )]
    pub source_groups: Vec<String>,
    #[arg(
        long,
        help = "Filter by stored workspace, cwd, source path, or repo-name text"
    )]
    pub workspace: Option<String>,
    #[arg(
        long,
        help = "Filter to recent history, as RFC3339 or a day window like 30d"
    )]
    pub since: Option<String>,
    #[arg(long, help = "Search only primary agent sessions")]
    pub primary_only: bool,
    #[arg(
        long,
        value_enum,
        conflicts_with = "event_type",
        help = "Search content scope: all, transcript, calls, or outputs"
    )]
    pub content_scope: Option<ContentScopeArg>,
    #[arg(
        long,
        conflicts_with = "content_scope",
        help = "Filter by event type: message, tool_call, tool_output, command_started, command_output, command_finished, file_touched, vcs_change, artifact, summary, or notice"
    )]
    pub event_type: Option<String>,
    #[arg(
        long,
        help = "Filter by indexed touched-file path metadata, not the current filesystem"
    )]
    pub file: Option<PathBuf>,
    #[arg(
        long,
        help = "Search event hits within one ctx session id or unambiguous id prefix"
    )]
    pub session: Option<String>,
    #[arg(
        long = "exclude-session",
        value_name = "SESSION",
        conflicts_with = "session",
        help = "Exclude one exact ctx session id or unambiguous id prefix; repeat to exclude multiple sessions"
    )]
    pub exclude_sessions: Vec<String>,
    #[arg(
        long,
        help = "Return dense event-level results instead of diverse session results"
    )]
    pub events: bool,
    #[arg(
        long,
        value_enum,
        help = "Search backend override: hybrid, semantic, or lexical",
        long_help = "Search backend override. By default ctx uses lexical search unless local semantic search is enabled in config, then hybrid. hybrid combines self-contained Core lexical evidence and semantic vector evidence; lexical uses only the Tantivy Core index; semantic requires local semantic search to be enabled and ready."
    )]
    pub backend: Option<SearchBackendArg>,
    #[arg(long = "semantic-weight", default_value_t = 0.35, value_parser = parse_semantic_weight, help = "Hybrid ranking weight for semantic evidence, from 0.0 to 1.0")]
    pub semantic_weight: f32,
    #[arg(long, value_enum, default_value_t = CliRefreshArg::Background, help = "Index freshness behavior: background, off, or wait", long_help = "Index freshness behavior. background serves the existing index and lets daemon maintenance refresh history/indexes; off searches the existing index only; wait runs or waits for required refresh work before searching.")]
    pub refresh: CliRefreshArg,
    #[arg(long, help = "Include the automatically detected active session tree")]
    pub include_current_session: bool,
    #[arg(long, value_enum, default_value_t = crate::JsonOutputFormat::Text)]
    pub format: crate::JsonOutputFormat,
    #[arg(
        long,
        help = "Print expanded text details such as full ids, provider ids, citations, and next commands"
    )]
    pub verbose: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SearchBackendArg {
    Hybrid,
    Lexical,
    Semantic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ContentScopeArg {
    All,
    Transcript,
    Calls,
    Outputs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliRefreshArg {
    Background,
    Off,
    Wait,
}

impl CliRefreshArg {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Off => "off",
            Self::Wait => "wait",
        }
    }
}

pub fn run_search(
    args: SearchArgs,
    data_root: PathBuf,
    telemetry: &mut SearchTelemetry,
    config: ctx_history_cli::HistoryCliConfig,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
) -> Result<()> {
    let observation = ctx_history_cli::run_search(
        adapt(args),
        data_root,
        config,
        local_usage,
        ui,
        |observation| apply_search_observation(telemetry, observation),
    )?;
    if let Some(render_duration) = observation.render_duration {
        telemetry.render_duration = Some(duration_bucket(render_duration));
    }
    Ok(())
}

pub fn adapt(args: SearchArgs) -> ctx_history_cli::SearchArgs {
    let provider = args.provider.map(|provider| {
        ctx_history_cli::ProviderArg(ctx_history_cli::HistoryProvider::from(
            provider.capture_provider(),
        ))
    });
    ctx_history_cli::SearchArgs {
        query: args.query,
        term: args.term,
        limit: args.limit,
        provider,
        history_source: args.history_source,
        provider_key: args.provider_key,
        source_id: args.source_id,
        source_format: args.source_format,
        source_roots: args.source_roots,
        source_groups: args.source_groups,
        workspace: args.workspace,
        since: args.since,
        primary_only: args.primary_only,
        content_scope: args.content_scope.map(|scope| match scope {
            ContentScopeArg::All => ctx_history_cli::ContentScopeArg::All,
            ContentScopeArg::Transcript => ctx_history_cli::ContentScopeArg::Transcript,
            ContentScopeArg::Calls => ctx_history_cli::ContentScopeArg::Calls,
            ContentScopeArg::Outputs => ctx_history_cli::ContentScopeArg::Outputs,
        }),
        event_type: args.event_type,
        file: args.file,
        session: args.session,
        exclude_sessions: args.exclude_sessions,
        events: args.events,
        backend: args.backend.map(|backend| match backend {
            SearchBackendArg::Hybrid => ctx_history_cli::SearchBackendArg::Hybrid,
            SearchBackendArg::Lexical => ctx_history_cli::SearchBackendArg::Lexical,
            SearchBackendArg::Semantic => ctx_history_cli::SearchBackendArg::Semantic,
        }),
        semantic_weight: args.semantic_weight,
        refresh: match args.refresh {
            CliRefreshArg::Background => ctx_history_cli::RefreshMode::Background,
            CliRefreshArg::Off => ctx_history_cli::RefreshMode::Off,
            CliRefreshArg::Wait => ctx_history_cli::RefreshMode::Wait,
        },
        include_current_session: args.include_current_session,
        format: match args.format {
            crate::output::JsonOutputFormat::Text => ctx_history_cli::JsonOutputFormat::Text,
            crate::output::JsonOutputFormat::Json => ctx_history_cli::JsonOutputFormat::Json,
        },
        verbose: args.verbose,
    }
}

pub fn parse_search_limit(value: &str) -> Result<usize, String> {
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

fn apply_search_observation(
    telemetry: &mut SearchTelemetry,
    observation: ctx_history_cli::SearchExecutionObservation,
) {
    telemetry.refresh_mode = Some(match observation.refresh_mode {
        ctx_history_cli::RefreshMode::Background => RefreshMode::Background,
        ctx_history_cli::RefreshMode::Off => RefreshMode::Off,
        ctx_history_cli::RefreshMode::Wait => RefreshMode::Wait,
    });
    telemetry.refresh_status = Some(match observation.refresh_status {
        ctx_history_cli::SearchRefreshStatus::ExistingGeneration => {
            RefreshStatus::from_safe_summary("existing_generation")
        }
        ctx_history_cli::SearchRefreshStatus::DaemonBackground => {
            RefreshStatus::from_safe_summary("daemon_background")
        }
        ctx_history_cli::SearchRefreshStatus::DaemonUnavailable => {
            RefreshStatus::from_safe_summary("daemon_unavailable")
        }
        ctx_history_cli::SearchRefreshStatus::Completed => {
            RefreshStatus::from_safe_summary("completed")
        }
    });
    telemetry.refresh_source_count = Some(count_bucket(observation.refresh_source_count));
    telemetry.refresh_duration = Some(duration_bucket(observation.refresh_duration));
    telemetry.query_duration = Some(duration_bucket(observation.query_duration));
    if let Some(render_duration) = observation.render_duration {
        telemetry.render_duration = Some(duration_bucket(render_duration));
    }
    telemetry.backend_requested = Some(search_backend(observation.backend_requested));
    telemetry.backend_effective = Some(search_backend(observation.backend_effective));
    telemetry.result_count = Some(count_bucket(observation.result_count));
    telemetry.citation_count = Some(count_bucket(observation.citation_count));
    telemetry.zero_result = Some(observation.zero_result);
    telemetry.has_indexed_content_after = Some(observation.has_indexed_content_after);
    telemetry.query_length = Some(text_length_bucket(observation.query_length as usize));
    telemetry.query_term_count = Some(count_bucket(observation.query_term_count));
}

const fn search_backend(value: ctx_history_read_application::SearchBackend) -> SearchBackend {
    match value {
        ctx_history_read_application::SearchBackend::Hybrid => SearchBackend::Hybrid,
        ctx_history_read_application::SearchBackend::Lexical => SearchBackend::Lexical,
        ctx_history_read_application::SearchBackend::Semantic => SearchBackend::Semantic,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use clap::Parser;

    use super::*;

    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(flatten)]
        search: SearchArgs,
    }

    fn telemetry() -> SearchTelemetry {
        SearchTelemetry {
            has_query: true,
            has_provider_filter: false,
            has_workspace_filter: false,
            has_since_filter: false,
            has_event_type_filter: false,
            has_file_filter: false,
            has_session_filter: false,
            event_results: false,
            primary_only: false,
            include_current_session: false,
            limit: count_bucket(10),
            provider_filter: None,
            refresh_duration: None,
            refresh_mode: None,
            refresh_status: None,
            refresh_source_count: None,
            has_indexed_content_after: None,
            query_length: None,
            query_term_count: None,
            query_duration: None,
            backend_requested: None,
            backend_effective: None,
            result_count: None,
            citation_count: None,
            zero_result: None,
            render_duration: None,
        }
    }

    #[test]
    fn observation_mapping_populates_every_final_analytics_field_once() {
        let mut telemetry = telemetry();
        apply_search_observation(
            &mut telemetry,
            ctx_history_cli::SearchExecutionObservation {
                refresh_mode: ctx_history_cli::RefreshMode::Wait,
                refresh_status: ctx_history_cli::SearchRefreshStatus::Completed,
                refresh_source_count: 3,
                refresh_duration: Duration::from_millis(1),
                query_duration: Duration::from_millis(2),
                render_duration: Some(Duration::from_millis(3)),
                backend_requested: ctx_history_read_application::SearchBackend::Hybrid,
                backend_effective: ctx_history_read_application::SearchBackend::Lexical,
                result_count: 4,
                citation_count: 5,
                zero_result: false,
                has_indexed_content_after: true,
                query_length: 6,
                query_term_count: 2,
            },
        );

        assert_eq!(telemetry.refresh_mode, Some(RefreshMode::Wait));
        assert_eq!(telemetry.refresh_status, Some(RefreshStatus::Completed));
        assert_eq!(telemetry.backend_requested, Some(SearchBackend::Hybrid));
        assert_eq!(telemetry.backend_effective, Some(SearchBackend::Lexical));
        assert_eq!(telemetry.zero_result, Some(false));
        assert_eq!(telemetry.has_indexed_content_after, Some(true));
        assert!(telemetry.refresh_source_count.is_some());
        assert!(telemetry.refresh_duration.is_some());
        assert!(telemetry.query_duration.is_some());
        assert!(telemetry.render_duration.is_some());
        assert!(telemetry.query_length.is_some());
        assert!(telemetry.query_term_count.is_some());
        assert!(telemetry.result_count.is_some());
        assert!(telemetry.citation_count.is_some());
    }

    #[test]
    fn query_observation_populates_analytics_before_render_completes() {
        let mut telemetry = telemetry();
        apply_search_observation(
            &mut telemetry,
            ctx_history_cli::SearchExecutionObservation {
                refresh_mode: ctx_history_cli::RefreshMode::Off,
                refresh_status: ctx_history_cli::SearchRefreshStatus::ExistingGeneration,
                refresh_source_count: 1,
                refresh_duration: Duration::from_millis(1),
                query_duration: Duration::from_millis(2),
                render_duration: None,
                backend_requested: ctx_history_read_application::SearchBackend::Lexical,
                backend_effective: ctx_history_read_application::SearchBackend::Lexical,
                result_count: 1,
                citation_count: 1,
                zero_result: false,
                has_indexed_content_after: true,
                query_length: 6,
                query_term_count: 1,
            },
        );

        assert!(telemetry.query_duration.is_some());
        assert!(telemetry.result_count.is_some());
        assert_eq!(telemetry.backend_effective, Some(SearchBackend::Lexical));
        assert_eq!(telemetry.render_duration, None);
    }

    #[test]
    fn search_clap_adapter_accepts_omitted_and_explicit_all_scope() {
        let omitted = TestCli::try_parse_from(["ctx", "needle"]).unwrap().search;
        let explicit = TestCli::try_parse_from(["ctx", "needle", "--content-scope", "all"])
            .unwrap()
            .search;
        assert_eq!(omitted.query.as_deref(), Some("needle"));
        assert_eq!(omitted.content_scope, None);
        assert!(matches!(explicit.content_scope, Some(ContentScopeArg::All)));
    }

    #[test]
    fn search_clap_adapter_forwards_content_scope_controls() {
        let args = TestCli::try_parse_from([
            "ctx",
            "needle",
            "--content-scope",
            "calls",
            "--events",
            "--include-current-session",
        ])
        .unwrap()
        .search;
        assert!(matches!(args.content_scope, Some(ContentScopeArg::Calls)));
        assert!(args.events && args.include_current_session);
    }

    #[test]
    fn search_clap_accepts_source_groups_and_rejects_the_unreleased_scope_spelling() {
        let args = TestCli::try_parse_from([
            "ctx",
            "needle",
            "--source-group",
            "personal",
            "--source-group=work",
        ])
        .unwrap()
        .search;
        assert_eq!(args.source_groups, ["personal", "work"]);

        let error = TestCli::try_parse_from(["ctx", "needle", "--scope", "work"]).unwrap_err();
        assert!(error.to_string().contains("--scope"));
    }

    #[test]
    fn search_clap_adapter_accepts_repeatable_session_exclusions_and_conflicts_with_session() {
        let args = TestCli::try_parse_from([
            "ctx",
            "needle",
            "--exclude-session",
            "first",
            "--exclude-session=second",
        ])
        .unwrap()
        .search;
        assert_eq!(args.exclude_sessions, ["first", "second"]);
        assert_eq!(adapt(args).exclude_sessions, ["first", "second"]);

        let error = TestCli::try_parse_from([
            "ctx",
            "needle",
            "--session",
            "positive",
            "--exclude-session",
            "negative",
        ])
        .unwrap_err();
        assert!(error.to_string().contains("cannot be used with"));
    }

    #[test]
    fn primary_only_is_the_only_session_scope_override() {
        let args = TestCli::try_parse_from(["ctx", "needle", "--primary-only"])
            .unwrap()
            .search;
        assert!(args.primary_only);

        let error = TestCli::try_parse_from(["ctx", "needle", "--include-subagents"]).unwrap_err();
        assert!(error.to_string().contains("--include-subagents"));
    }

    #[test]
    fn empty_search_action_is_a_valid_positional_query() {
        TestCli::try_parse_from(["ctx", "<term>"])
            .expect("empty-state action must be a valid positional search invocation");
    }
}
