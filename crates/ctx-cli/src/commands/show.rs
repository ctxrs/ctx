pub(crate) mod events;

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::analytics::ShowTelemetry;
use crate::local_usage::CliUsage;
use crate::output::OutputFormat;
use crate::provider_args::ProviderArg;
use crate::transcript::TranscriptMode;
use crate::ui::Ui;
use crate::{parse_event_window_limit, parse_provider_arg, ShowArgs};

pub(crate) use events::{EventQueryFormat, ShowEventsArgs};

#[derive(Debug, Args)]
pub(crate) struct ShowSessionArgs {
    #[arg(help = "ctx session id or unambiguous id prefix")]
    pub(crate) id: Option<String>,
    #[arg(long, value_parser = parse_provider_arg, hide_possible_values = true)]
    pub(crate) provider: Option<ProviderArg>,
    #[arg(long = "provider-session")]
    pub(crate) provider_session: Option<String>,
    #[arg(long, value_enum, default_value_t = TranscriptMode::Lite)]
    pub(crate) mode: TranscriptMode,
    #[arg(long, help = "Return at most this many selected transcript events")]
    pub(crate) max_events: Option<usize>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub(crate) format: OutputFormat,
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(crate) struct ShowEventArgs {
    #[arg(help = "ctx event id or unambiguous id prefix")]
    pub(crate) id: String,
    #[arg(
        long,
        default_value_t = 0,
        value_parser = parse_event_window_limit,
        help = "Number of preceding events to include (0..50)"
    )]
    pub(crate) before: usize,
    #[arg(
        long,
        default_value_t = 0,
        value_parser = parse_event_window_limit,
        help = "Number of following events to include (0..50)"
    )]
    pub(crate) after: usize,
    #[arg(
        long,
        value_parser = parse_event_window_limit,
        help = "Use this many events on both sides of the selected event (0..50)"
    )]
    pub(crate) window: Option<usize>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub(crate) format: OutputFormat,
}

pub(crate) fn run_show(
    args: ShowArgs,
    data_root: PathBuf,
    telemetry: &mut ShowTelemetry,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
) -> Result<()> {
    match args.target {
        crate::ShowTarget::Events(args) => events::run(args, data_root, telemetry, local_usage, ui),
        target => crate::commands::source_index::run_show(
            ShowArgs { target },
            data_root,
            telemetry,
            local_usage,
            ui,
        ),
    }
}
