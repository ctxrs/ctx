use std::process::ExitCode;

use ctx_history_core::CaptureProvider;

mod analytics;
mod cli;
mod commands;
mod complete_content;
mod config;
mod deprecated_controls;
mod dispatch;
mod docs;
mod execution_capabilities;
mod history_source_plugins;
mod identity;
mod install_marker;
mod integrations;
mod local_usage;
mod mcp;
mod net;
mod output;
mod pro;
mod progress;
mod provider_args;
mod provider_projection;
mod provider_sources;
mod release_build_identity;
mod search_filters;
mod search_render;
mod semantic;
mod skill;
mod store_util;
mod transcript;
mod upgrade;
mod value_parsers;

#[cfg(test)]
mod parser_prop_tests;

pub(crate) use cli::{
    Cli, DaemonArgs, DaemonCommand, DaemonRunArgs, DaemonStartModeArg, DaemonTriggerCommandArg,
    DoctorArgs, FormatArgs, ImportArgs, LocateArgs, LocateTarget, SearchArgs, SearchBackendArg,
    SetupArgs, ShowArgs, ShowTarget, SourcesArgs, SqlArgs, StatusArgs, UsageStatusMode,
    MAX_EVENT_WINDOW, MAX_SEARCH_LIMIT,
};
pub(crate) use commands::search::{RefreshArg, SearchRefreshReport};
pub(crate) use commands::sql::raw_sql_result_json;
pub(crate) use output::{compact_json, OutputFormat};
pub(crate) use provider_args::{cli_supported_provider, parse_provider_arg, ProviderArg};
pub(crate) use provider_sources::{discovered_plugin_sources_json, sources_json};
pub(crate) use search_filters::{
    search_filters, search_has_intent, SearchFilterInput, SearchIntentInput,
    SourceIdentityFilterArgs,
};
pub(crate) use search_render::SearchDto;
pub(crate) use transcript::{
    event_window, event_window_json, session_transcript_json, TranscriptMode,
};
pub(crate) use value_parsers::parse_event_window_limit;

const WAL_TRUNCATE_MIN_BYTES: u64 = 64 * 1024 * 1024;
const LARGE_IMPORT_SOURCE_FILES_WARNING: usize = 10_000;
const LARGE_IMPORT_SOURCE_BYTES_WARNING: u64 = 1024 * 1024 * 1024;
const MAX_HISTORY_SOURCE_PLUGIN_JSONL_LINE_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_VISIBLE_SOURCE_PROVIDERS: &[CaptureProvider] = &[
    CaptureProvider::Claude,
    CaptureProvider::Codex,
    CaptureProvider::Cursor,
    CaptureProvider::Pi,
    CaptureProvider::CopilotCli,
    CaptureProvider::OpenCode,
];

fn main() -> ExitCode {
    if release_build_identity::print_if_requested() {
        return ExitCode::SUCCESS;
    }
    dispatch::run()
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
