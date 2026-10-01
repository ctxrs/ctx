//! Graph commands backed by the local graph engine.
use std::{
    ffi::OsString,
    io::{self, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use clap::{CommandFactory, FromArgMatches};
use ctx_graph_core::{hook_guard, import, index, model::*, store::Store};

pub use ctx_graph_core;

mod agent_setup;
mod cli;
mod commands;
mod connect;
mod dispatch;
mod extraction;
mod mcp;
mod observation;
mod output;
mod paths;
mod query_log;
mod read;
mod switch;
mod switch_config;
mod switch_files;

pub use cli::GraphArgs;
use cli::*;
pub use dispatch::run_parsed;
pub use dispatch::run_parsed_observed;
pub use observation::{GraphInvocation, GraphObservation, GraphObserver, GraphOperation};
pub use output::write_search;
use output::*;
use paths::database;
pub use paths::{discover_database, optional_database};
use query_log::append_query_log;
use read::*;

/// Exact schema for the `ctx graph` namespace.
pub fn command() -> clap::Command {
    cli::Cli::command()
}

/// Run arguments following `ctx graph`, without terminating the process.
pub fn run(args: impl IntoIterator<Item = OsString>) -> i32 {
    run_observed(args, None)
}

/// Run with an optional nonblocking, consent-resolved host observer.
pub fn run_observed(
    args: impl IntoIterator<Item = OsString>,
    observer: Option<GraphObserver>,
) -> i32 {
    let started = std::time::Instant::now();
    let argv = std::iter::once(OsString::from("ctx graph")).chain(args);
    let parsed = command()
        .try_get_matches_from(argv)
        .and_then(|matches| cli::Cli::from_arg_matches(&matches));
    let cli = match parsed {
        Ok(cli) => cli,
        Err(error) => {
            let mut facts = GraphObservation::new(GraphOperation::Parse, GraphInvocation::Cli);
            facts.phase = observation::GraphPhase::Parse;
            if error.use_stderr() {
                let mut stderr = io::stderr().lock();
                let delivered = writeln!(
                    stderr,
                    "{}",
                    human(error.render().ansi().to_string().trim_end())
                )
                .and_then(|()| stderr.flush());
                facts.output_served = Some(delivered.is_ok());
                facts.output_boundary = observation::GraphOutputBoundary::CliFlush;
                facts.output_failure = delivered
                    .err()
                    .map(|error| observation::failure_kind(&error.into()));
                facts.fail(observation::GraphFailureKind::InvalidInput);
                facts.duration = Some(started.elapsed());
                observation::emit(&observer, facts);
            } else {
                let _ = error.print();
            }
            return error.exit_code();
        }
    };
    run_parsed_exit_observed(cli.graph, observer)
}

/// Execute parsed graph arguments with the same runtime error output on every entry path.
pub fn run_parsed_exit(cli: GraphArgs) -> i32 {
    run_parsed_exit_observed(cli, None)
}

pub fn run_parsed_exit_observed(cli: GraphArgs, observer: Option<GraphObserver>) -> i32 {
    let json = cli.json;
    let started = std::time::Instant::now();
    let mut facts =
        GraphObservation::new(observation::operation(&cli.command), GraphInvocation::Cli);
    let result = run_parsed_observed(cli, &mut facts, observer.clone());
    let status = observation::finish_cli(
        &mut facts,
        &result,
        json,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    );
    facts.duration = Some(started.elapsed());
    observation::emit(&observer, facts);
    // Optional host hooks retain their fail-open exit contract on broken pipes.
    if facts.operation == GraphOperation::HookGuard && result.is_ok() {
        0
    } else {
        status
    }
}

/// Search an existing saved graph without refreshing sources or migrating it.
pub fn search(
    db: &Path,
    text: &str,
    options: &ctx_graph_core::query::SearchOptions,
) -> Result<ctx_graph_core::query::SearchResult> {
    search_observed(
        db,
        text,
        options,
        &mut GraphObservation::new(GraphOperation::Search, GraphInvocation::Library),
    )
}

/// Project facts from the same query; no additional source scans or output writes.
pub fn search_observed(
    db: &Path,
    text: &str,
    options: &ctx_graph_core::query::SearchOptions,
    facts: &mut GraphObservation,
) -> Result<ctx_graph_core::query::SearchResult> {
    let started = std::time::Instant::now();
    facts.phase = observation::GraphPhase::Open;
    let result = (|| {
        let store = Store::open_read_only(db)?;
        facts.phase = observation::GraphPhase::Query;
        store.query_extended(text, options)
    })();
    facts.query_duration = Some(started.elapsed());
    facts.duration = Some(started.elapsed());
    match &result {
        Ok(result) => {
            observation::search(facts, result);
            facts.execution_succeeded = Some(true);
        }
        Err(error) => observation::failed(facts, error),
    }
    result
}

fn hook_context(
    host: hook_guard::Host,
    project: Option<&Path>,
    db: Option<&Path>,
    input: impl std::io::Read,
) -> Option<serde_json::Value> {
    let mut value = hook_guard::run(host, project, db, input)?;
    for pointer in [
        "/additionalContext",
        "/hookSpecificOutput/additionalContext",
    ] {
        if let Some(context) = value.pointer_mut(pointer)
            && let Some(text) = context.as_str()
        {
            *context = serde_json::Value::String(
                text.replace("graf query", "ctx graph search")
                    .replace("graf show", "ctx graph show")
                    .replace("graf callers", "ctx graph callers")
                    .replace("graf impact", "ctx graph impact"),
            );
        }
    }
    Some(value)
}

#[cfg(test)]
mod tests;
