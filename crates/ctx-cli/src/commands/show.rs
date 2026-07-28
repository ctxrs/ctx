use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use ctx_history_core::database_path;

use crate::analytics::{count_bucket, ShowTelemetry};
use crate::complete_content::{
    resolve_event_contents, ContentPolicy, CLI_COMPLETE_CONTENT_MAX_OUTPUT_BYTES,
};
use crate::output::OutputFormat;
use crate::provider_args::ProviderArg;
use crate::store_util::open_existing_store_read_only;
use crate::transcript::{
    event_window, resolve_event, resolve_session, selected_transcript_events,
    write_rendered_events, write_rendered_session, TranscriptMode,
};
use crate::{parse_event_window_limit, parse_provider_arg, ShowArgs, ShowTarget};

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
) -> Result<()> {
    if crate::commands::source_index::index_is_available(&data_root) {
        return crate::commands::source_index::run_show(args, data_root, telemetry);
    }
    match args.target {
        ShowTarget::Session(args) => {
            let store = open_existing_store_read_only(&database_path(data_root), "ctx show")?;
            let session = resolve_session(
                &store,
                args.id,
                args.provider.map(ProviderArg::capture_provider),
                args.provider_session.as_deref(),
            )?;
            let events = store.events_for_session(session.id)?;
            telemetry.events_returned = Some(count_bucket(events.len() as u64));
            let selected = selected_transcript_events(&events, args.mode);
            let content = resolve_event_contents(
                &store,
                &selected,
                args.content,
                CLI_COMPLETE_CONTENT_MAX_OUTPUT_BYTES,
            )?;
            write_rendered_session(
                &store,
                &session,
                &events,
                args.mode,
                args.format,
                args.out,
                &content,
            )?;
        }
        ShowTarget::Event(args) => {
            let store = open_existing_store_read_only(&database_path(data_root), "ctx show")?;
            let event = resolve_event(&store, &args.id)?;
            let events = event_window(&store, &event, args.before, args.after, args.window)?;
            telemetry.events_returned = Some(count_bucket(events.len() as u64));
            let selected = events.iter().collect::<Vec<_>>();
            let content = resolve_event_contents(
                &store,
                &selected,
                args.content,
                CLI_COMPLETE_CONTENT_MAX_OUTPUT_BYTES,
            )?;
            write_rendered_events(&store, &event, &events, args.format, None, &content)?;
        }
    }
    Ok(())
}
