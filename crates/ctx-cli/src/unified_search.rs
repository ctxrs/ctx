//! Explicit composition of independent history and repository search results.

use std::{
    ops::Deref,
    path::{Path, PathBuf},
};

use anyhow::{bail, ensure, Context, Result};
use clap::{parser::ValueSource, Args, CommandFactory, ValueEnum};
use serde::Serialize;
use serde_json::Value;

use crate::commands::search::{SearchArgs, SearchBackendArg};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SearchScope {
    #[default]
    History,
    Graph,
    All,
}

#[derive(Debug, Args)]
pub(crate) struct ScopedSearchArgs {
    #[command(flatten)]
    pub(crate) history: SearchArgs,
    /// Search history, the indexed code/document graph, or both independently.
    #[arg(long, value_enum, default_value_t = SearchScope::History)]
    pub(crate) scope: SearchScope,
    /// Select a graph database; otherwise find the nearest .graf/index.db.
    #[arg(long, value_name = "PATH")]
    pub(crate) graph_db: Option<PathBuf>,
}

impl Deref for ScopedSearchArgs {
    type Target = SearchArgs;

    fn deref(&self) -> &Self::Target {
        &self.history
    }
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ScopeResult {
    Ok {
        result: Value,
    },
    Unavailable {
        error: String,
        next_action: &'static str,
    },
}

impl ScopeResult {
    fn from_result(result: Result<Value>, next_action: &'static str) -> Self {
        match result {
            Ok(result) => Self::Ok { result },
            Err(error) => Self::Unavailable {
                error: format!("{error:#}"),
                next_action,
            },
        }
    }

    fn available(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }
}

#[derive(Serialize)]
struct ScopedResults {
    schema_version: u32,
    scope: SearchScope,
    limit_per_scope: usize,
    partial: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    history: Option<ScopeResult>,
    graph: ScopeResult,
}

/// Validate scope-specific options using Clap's actual value sources, so a query
/// containing a flag-looking word is never mistaken for an option.
pub(crate) fn validate(args: &ScopedSearchArgs) -> Result<()> {
    if args.scope == SearchScope::History {
        ensure!(
            args.graph_db.is_none(),
            "--graph-db requires --scope graph or all"
        );
        return Ok(());
    }
    let matches = crate::Cli::command().try_get_matches_from(std::env::args_os())?;
    let search = matches
        .subcommand_matches("search")
        .context("missing search arguments")?;
    for id in [
        "term",
        "provider",
        "history_source",
        "provider_key",
        "source_id",
        "source_format",
        "source_roots",
        "source_groups",
        "workspace",
        "since",
        "primary_only",
        "content_scope",
        "event_type",
        "file",
        "session",
        "exclude_sessions",
        "events",
        "backend",
        "semantic_weight",
        "refresh",
        "include_current_session",
    ] {
        ensure!(search.value_source(id) != Some(ValueSource::CommandLine),
            "--{} is a history-only filter; use --scope history (or ctx graph query for graph filters)",
            id.replace('_', "-"));
    }
    Ok(())
}

pub(crate) fn run(
    mut args: ScopedSearchArgs,
    data_root: Option<PathBuf>,
    color: crate::ui::ColorMode,
) -> Result<()> {
    let query = args.history.query.clone().unwrap_or_default();
    ensure!(!query.trim().is_empty(), "provide a search query");
    let scope = args.scope;
    let limit = args.history.limit;
    let json = args.history.format.is_json();
    let verbose = args.history.verbose;
    let ui = crate::ui::Ui::stdio(color);
    let graph = ScopeResult::from_result(
        graph_query(&query, limit, args.graph_db.as_deref()),
        "Run ctx graph index in the repository or pass --graph-db PATH.",
    );
    let history = if scope == SearchScope::All {
        // Combined search reads saved snapshots. It never starts a daemon or
        // invokes an embedding endpoint merely because another corpus was added.
        args.history.backend = Some(SearchBackendArg::Lexical);
        Some(ScopeResult::from_result(
            history_query(args.history, data_root),
            "Run ctx status to inspect history; ctx setup initializes it explicitly.",
        ))
    } else {
        None
    };
    let available = graph.available() || history.as_ref().is_some_and(ScopeResult::available);
    let partial = history
        .as_ref()
        .is_some_and(|history| history.available() != graph.available());
    let result = ScopedResults {
        schema_version: 1,
        scope,
        limit_per_scope: limit,
        partial,
        history,
        graph,
    };
    crate::output::with_stdout_writer(|out| -> Result<()> {
        if json {
            serde_json::to_writer(&mut *out, &result)?;
            writeln!(out)?;
        } else {
            if let Some(history) = &result.history {
                render_scope(out, "History", history, verbose, ui.stdout_context())?;
            }
            render_scope(out, "Graph", &result.graph, verbose, ui.stdout_context())?;
            if partial {
                writeln!(out, "Partial results: one search scope is unavailable.")?;
            }
        }
        Ok(())
    })?;
    if !available {
        bail!("no requested search scope is available");
    }
    Ok(())
}

fn graph_query(query: &str, limit: usize, explicit: Option<&Path>) -> Result<Value> {
    use ctx_graph::graf::{model::QueryOptions, query::SearchOptions, store::Store};
    let db = ctx_graph::discover_database(explicit)?;
    let store = Store::open_read_only(&db)?;
    let options = SearchOptions {
        graph: QueryOptions {
            limit,
            ..QueryOptions::default()
        },
        ..SearchOptions::default()
    };
    Ok(serde_json::to_value(
        store.query_extended(query, &options)?,
    )?)
}

fn history_query(args: SearchArgs, data_root: Option<PathBuf>) -> Result<Value> {
    let root = data_root
        .map(Ok)
        .unwrap_or_else(ctx_history_platform::default_data_root)?;
    let config = ctx_app_config::AppConfig::load_read_only(&root)?;
    let request = ctx_history_cli::SearchRequest::from(crate::commands::search::adapt(args)).into();
    let result = ctx_history_cli::cli_snapshot_search(
        request,
        &root,
        ctx_history_cli::HistoryCliConfig {
            daemon_enabled: false,
            semantic_search_enabled: false,
            semantic_executor: config.semantic_embedding_executor().clone(),
            local_usage_enabled: false,
            automatic_provider_discovery: config.automatic_source_discovery_enabled(),
            provider_roots: config.provider_root_definitions(),
        },
    );
    result
        .map(|(value, _, _, _)| value)
        .map_err(|failure| failure.into_parts().0.into())
}

fn render_scope(
    mut out: &mut dyn std::io::Write,
    name: &str,
    result: &ScopeResult,
    verbose: bool,
    context: &crate::ui::RenderContext,
) -> Result<()> {
    writeln!(out, "{name}")?;
    match result {
        ScopeResult::Ok { result } => {
            // Reuse each engine's result presentation; unrelated relevance
            // scores are never mixed into a fabricated overall ranking.
            if name == "History" {
                let document = ctx_history_cli::render_snapshot_search(result, verbose, context);
                write!(out, "{}", document.render(context))?;
            } else {
                let search: ctx_graph::graf::query::SearchResult =
                    serde_json::from_value(result.clone())?;
                ctx_graph::write_search(&mut out, &search)?;
            }
        }
        ScopeResult::Unavailable { error, next_action } => {
            writeln!(
                out,
                "Unavailable: {}",
                crate::ui::sanitize_untrusted_history_body_for_terminal(error)
            )?;
            writeln!(out, "{next_action}")?;
        }
    }
    Ok(())
}
