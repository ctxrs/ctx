use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::analytics::ShowTelemetry;
use crate::complete_content::ContentPolicy;
use crate::local_usage::CliUsage;
use crate::output::OutputFormat;
use crate::provider_args::ProviderArg;
use crate::transcript::TranscriptMode;
use crate::ui::Ui;
use crate::{parse_event_window_limit, parse_provider_arg, ShowArgs};

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
    #[arg(
        long,
        value_enum,
        default_value_t = ContentPolicy::Indexed,
        help = "Message content fidelity; complete may read verified local provider sources and caps final serialized output at 64 MiB"
    )]
    pub(crate) content: ContentPolicy,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub(crate) format: OutputFormat,
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(crate) struct ShowEventArgs {
    #[arg(help = "ctx event id or unambiguous id prefix")]
    pub(crate) id: String,
    #[arg(long, default_value_t = 0, value_parser = parse_event_window_limit)]
    pub(crate) before: usize,
    #[arg(long, default_value_t = 0, value_parser = parse_event_window_limit)]
    pub(crate) after: usize,
    #[arg(long, value_parser = parse_event_window_limit)]
    pub(crate) window: Option<usize>,
    #[arg(
        long,
        value_enum,
        default_value_t = ContentPolicy::Indexed,
        help = "Message content fidelity; complete may read verified local provider sources and caps final serialized output at 64 MiB"
    )]
    pub(crate) content: ContentPolicy,
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
    crate::commands::source_index::run_show(args, data_root, telemetry, local_usage, ui)
}
