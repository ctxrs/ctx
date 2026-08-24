use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::{
    analytics::LocateTelemetry, local_usage::CliUsage, output::JsonOutputFormat,
    provider_args::ProviderArg, ui::Ui,
};

#[derive(Debug, Args)]
pub struct LocateArgs {
    #[command(subcommand)]
    pub target: LocateTarget,
}

#[derive(Debug, Subcommand)]
pub enum LocateTarget {
    #[command(about = "Locate Core source identity for a session")]
    Session(LocateSessionArgs),
    #[command(about = "Locate Core source identity for an event")]
    Event(LocateEventArgs),
}

#[derive(Debug, Args)]
pub struct LocateSessionArgs {
    #[arg(help = "ctx session id or unambiguous id prefix")]
    pub id: Option<String>,
    #[arg(long, value_parser = crate::parse_provider_arg, hide_possible_values = true)]
    pub provider: Option<ProviderArg>,
    #[arg(long = "provider-session")]
    pub provider_session: Option<String>,
    #[arg(long, requires_all = ["source_id", "provider_session"])]
    pub provider_key: Option<String>,
    #[arg(long, requires_all = ["provider_key", "provider_session"])]
    pub source_id: Option<String>,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub format: JsonOutputFormat,
}

#[derive(Debug, Args)]
pub struct LocateEventArgs {
    #[arg(help = "ctx event id or unambiguous id prefix")]
    pub id: String,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub format: JsonOutputFormat,
}

pub fn run_locate(
    args: LocateArgs,
    data_root: PathBuf,
    _telemetry: &mut LocateTelemetry,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
) -> Result<()> {
    let target = match args.target {
        LocateTarget::Session(args) => {
            ctx_history_cli::LocateTarget::Session(ctx_history_cli::LocateSessionArgs {
                id: args.id,
                provider: args.provider.map(history_provider),
                provider_session: args.provider_session,
                provider_key: args.provider_key,
                source_id: args.source_id,
                format: history_format(args.format),
            })
        }
        LocateTarget::Event(args) => {
            ctx_history_cli::LocateTarget::Event(ctx_history_cli::LocateEventArgs {
                id: args.id,
                format: history_format(args.format),
            })
        }
    };
    ctx_history_cli::run_locate(
        ctx_history_cli::LocateArgs { target },
        data_root,
        local_usage,
        ui,
    )
}

fn history_provider(provider: ProviderArg) -> ctx_history_cli::ProviderArg {
    ctx_history_cli::ProviderArg(ctx_history_cli::HistoryProvider::from(
        provider.capture_provider(),
    ))
}

fn history_format(format: JsonOutputFormat) -> ctx_history_cli::JsonOutputFormat {
    match format {
        JsonOutputFormat::Text => ctx_history_cli::JsonOutputFormat::Text,
        JsonOutputFormat::Json => ctx_history_cli::JsonOutputFormat::Json,
    }
}
