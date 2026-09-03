//! Core Clap adapters, command presentation, and terminal-facing workflows.
//!
//! The final `ctx` binary owns process startup, persisted configuration,
//! installation identity, daemon composition, and release provenance.
//! This crate receives those authorities through explicit per-call values and
//! ports; it never depends on the final binary.

pub mod docs;
pub mod integrations;
pub mod mcp_text;
pub mod analytics {
    pub use ctx_client_observability::analytics::*;
}
pub mod commands;
pub mod local_usage {
    pub use ctx_client_observability::local_usage::*;
}
pub mod output;
pub mod progress;
pub mod provider_args;
pub mod skill;
pub mod transcript;
pub mod upgrade;
pub mod value_parsers;
pub mod ui {
    pub use ctx_terminal::ui::*;
}

#[cfg(test)]
mod test_support;

pub use output::{JsonOutputFormat, OutputFormat};
pub use progress::ProgressArg;
pub use provider_args::{
    cli_supported_provider, parse_native_provider_arg, parse_provider_arg, ImportFormatArg,
    NativeProviderArg, ProviderArg,
};
pub use transcript::TranscriptMode;
pub use value_parsers::{parse_daemon_interval_seconds, parse_event_window_limit};

/// Marks a command failure whose exact command-specific output was emitted.
///
/// This is the shared lower CLI contract also recognized by final dispatch.
pub use ctx_history_cli::RenderedCliError;

pub fn rendered_cli_error() -> anyhow::Error {
    anyhow::Error::new(RenderedCliError)
}
