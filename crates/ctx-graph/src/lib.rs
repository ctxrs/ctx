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
    let argv = std::iter::once(OsString::from("ctx graph")).chain(args);
    let parsed = command()
        .try_get_matches_from(argv)
        .and_then(|matches| cli::Cli::from_arg_matches(&matches));
    let cli = match parsed {
        Ok(cli) => cli,
        Err(error) => {
            if error.use_stderr() {
                eprintln!("{}", human(error.render().ansi().to_string().trim_end()));
            } else {
                let _ = error.print();
            }
            return error.exit_code();
        }
    };
    match run_parsed(cli.graph) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("ctx graph: {}", human(&format!("{error:#}")));
            1
        }
    }
}

/// Search an existing saved graph without refreshing sources or migrating it.
pub fn search(
    db: &Path,
    text: &str,
    options: &ctx_graph_core::query::SearchOptions,
) -> Result<ctx_graph_core::query::SearchResult> {
    Store::open_read_only(db)?.query_extended(text, options)
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
