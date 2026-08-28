use std::process::ExitCode;

// Keep every direct CLI write on the same measured stdout/stderr seam as the
// structured terminal UI. These preserve the standard print macro behavior;
// `output` only adds content-free byte accounting while a command is active.
#[allow(unused_macros)]
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
mod analytics_outbox;
mod cli;
mod commands;
mod companion;
mod config;
mod core_capability;
mod deprecated_controls;
mod dispatch;
mod docs;
mod execution_capabilities;
mod history_config;
mod history_source_plugins;
mod identity;
mod integrations;
mod local_usage;
mod mcp;
mod net;
mod observability_composition;
mod observability_product;
mod operation_descriptor;
mod output;
mod presentation_limit {
    pub(crate) use ctx_terminal::presentation_limit::*;
}
mod process_environment;
mod progress;
mod provider_args;
mod provider_sources;
mod release_build_identity;
#[cfg(test)]
mod search_filters;
mod semantic;
mod tool_backend;
mod transcript;
#[allow(dead_code, unused_imports)]
mod ui;
mod upgrade;
mod value_parsers;

#[cfg(test)]
mod parser_prop_tests;

pub(crate) use cli::{
    Cli, DaemonArgs, DaemonCommand, DaemonStartModeArg, DaemonTriggerCommandArg, ImportArgs,
};
pub(crate) use commands::locate::LocateTarget;
pub(crate) use ctx_cli_presentation::commands::{
    DoctorArgs, SetupArgs, StatusArgs, UsageStatusMode,
};
pub(crate) use ctx_history_read_application::SearchBackend as SearchBackendArg;
pub(crate) use provider_args::ProviderArg;
pub(crate) use provider_sources::{discovered_plugin_sources_json, sources_json};
pub(crate) use transcript::TranscriptMode;
#[cfg(test)]
pub(crate) use value_parsers::parse_event_window_limit;

fn main() -> ExitCode {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    if let Some(exit) = core_capability::intercept(&arguments) {
        return exit;
    }
    if let Some(exit) = companion::forward_paid_cli_if_selected(arguments.clone()) {
        return exit;
    }
    ui::bootstrap_color_choice(arguments);
    if release_build_identity::print_if_requested() {
        return ExitCode::SUCCESS;
    }
    dispatch::run()
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
