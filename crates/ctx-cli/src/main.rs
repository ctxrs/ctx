use std::process::ExitCode;

use ctx_history_core::CaptureProvider;

// Keep every direct CLI write on the same measured stdout/stderr seam as the
// structured terminal UI. These preserve the standard print macro behavior;
// `output` only adds content-free byte accounting while a command is active.
macro_rules! print {
    ($($arg:tt)*) => {{
        $crate::output::write_stdout(format_args!($($arg)*));
    }};
}

macro_rules! println {
    () => {{
        $crate::output::write_stdout_line(format_args!(""));
    }};
    ($($arg:tt)*) => {{
        $crate::output::write_stdout_line(format_args!($($arg)*));
    }};
}

macro_rules! eprintln {
    () => {{
        $crate::output::write_stderr_line(format_args!(""));
    }};
    ($($arg:tt)*) => {{
        $crate::output::write_stderr_line(format_args!($($arg)*));
    }};
}

mod analytics;
mod cli;
mod commands;
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
mod presentation_limit;
mod pro;
mod process_environment;
mod progress;
mod provider_args;
mod provider_sources;
mod release_build_identity;
mod search_filters;
mod semantic;
mod skill;
mod transcript;
#[allow(dead_code, unused_imports)]
mod ui;
mod upgrade;
mod value_parsers;

#[cfg(test)]
mod parser_prop_tests;

pub(crate) use cli::{
    Cli, DaemonArgs, DaemonCommand, DaemonDisableArgs, DaemonRunArgs, DaemonStartModeArg,
    DaemonTriggerCommandArg, DoctorArgs, FormatArgs, ImportArgs, SearchArgs, SearchBackendArg,
    SetupArgs, ShowArgs, ShowTarget, SourcesArgs, StatsArgs, StatusArgs, UsageStatusMode,
    MAX_EVENT_WINDOW, MAX_SEARCH_LIMIT,
};
pub(crate) use commands::locate::LocateTarget;
pub(crate) use commands::search::RefreshArg;
pub(crate) use output::compact_json;
pub(crate) use provider_args::{cli_supported_provider, parse_provider_arg, ProviderArg};
pub(crate) use provider_sources::{discovered_plugin_sources_json, sources_json};
pub(crate) use search_filters::{search_has_intent, SearchIntentInput, SourceIdentityFilterArgs};
pub(crate) use transcript::TranscriptMode;
pub(crate) use value_parsers::parse_event_window_limit;

const DEFAULT_VISIBLE_SOURCE_PROVIDERS: &[CaptureProvider] = &[
    CaptureProvider::Claude,
    CaptureProvider::Codex,
    CaptureProvider::Cursor,
    CaptureProvider::Pi,
    CaptureProvider::CopilotCli,
    CaptureProvider::OpenCode,
];

fn main() -> ExitCode {
    ui::bootstrap_color_choice(std::env::args_os());
    if release_build_identity::print_if_requested() {
        return ExitCode::SUCCESS;
    }
    dispatch::run()
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
