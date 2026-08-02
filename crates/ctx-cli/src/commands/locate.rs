use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::{
    analytics::LocateTelemetry, local_usage::CliUsage, output::JsonOutputFormat,
    provider_args::ProviderArg, ui::Ui,
};

#[derive(Debug, Args)]
pub(crate) struct LocateArgs {
    #[command(subcommand)]
    pub(crate) target: LocateTarget,
}

#[derive(Debug, Subcommand)]
pub(crate) enum LocateTarget {
    #[command(about = "Locate Core source identity for a session")]
    Session(LocateSessionArgs),
    #[command(about = "Locate Core source identity for an event")]
    Event(LocateEventArgs),
}

#[derive(Debug, Args)]
pub(crate) struct LocateSessionArgs {
    #[arg(help = "ctx session id or unambiguous id prefix")]
    pub(crate) id: Option<String>,
    #[arg(long, value_parser = crate::parse_provider_arg, hide_possible_values = true)]
    pub(crate) provider: Option<ProviderArg>,
    #[arg(long = "provider-session")]
    pub(crate) provider_session: Option<String>,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub(crate) format: JsonOutputFormat,
}

#[derive(Debug, Args)]
pub(crate) struct LocateEventArgs {
    #[arg(help = "ctx event id or unambiguous id prefix")]
    pub(crate) id: String,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub(crate) format: JsonOutputFormat,
}

pub(crate) fn run_locate(
    args: LocateArgs,
    data_root: PathBuf,
    _telemetry: &mut LocateTelemetry,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
) -> Result<()> {
    crate::commands::source_index::run_locate(args, data_root, local_usage, ui)
}
