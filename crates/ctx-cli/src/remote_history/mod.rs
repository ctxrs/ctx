//! Explicit remote reads. This adapter never initializes local history storage.

mod backend;
mod observation;
#[cfg(test)]
mod protocol_tests;
mod render;
#[cfg(test)]
mod tests;
mod validation;

use std::path::PathBuf;

use anyhow::{bail, ensure, Result};
use clap::CommandFactory;
use ctx_history_server::CitationKind;
use ctx_history_sharing::RemoteClient;

use crate::{
    cli::CommandRoot,
    commands::show::{ShowArgs, ShowTarget},
    output::OutputFormat,
    ui::{ColorMode, Ui},
};

pub(crate) use backend::RemoteBackend;
use observation::Observation;
#[cfg(test)]
pub(crate) use observation::RemoteReadFacts;
pub(crate) use observation::{
    RemoteCompletion, RemoteFailure, RemoteObserver, RemoteOperation, RemoteStage,
};

pub(crate) fn run_with_observer(
    command: CommandRoot,
    data_root: Option<PathBuf>,
    name: &str,
    color: ColorMode,
    observer: Option<RemoteObserver>,
) -> Result<()> {
    let operation = match &command {
        CommandRoot::Search(_) => RemoteOperation::Search,
        CommandRoot::Show(ShowArgs {
            target: ShowTarget::Event(_),
        }) => RemoteOperation::Event,
        CommandRoot::Show(ShowArgs {
            target: ShowTarget::Session(_),
        }) => RemoteOperation::Session,
        _ => RemoteOperation::Unsupported,
    };
    // The MCP carrier owns tool terminals and its final writer outcome.
    let emit = !matches!(&command, CommandRoot::Mcp(_));
    let mut observed = Observation::new(operation);
    let result = run_observed(command, data_root, name, color, &mut observed);
    if emit {
        if let Some(observer) = observer {
            observer(observed.completion(result.as_ref().err()));
        }
    }
    result
}

fn run_observed(
    mut command: CommandRoot,
    data_root: Option<PathBuf>,
    name: &str,
    color: ColorMode,
    observed: &mut Observation,
) -> Result<()> {
    if let CommandRoot::Show(args) = &mut command {
        if matches!(args.target, ShowTarget::Session(_)) {
            let matches = crate::Cli::command().try_get_matches_from(std::env::args_os())?;
            validation::select_cli_mode(args, &matches);
        }
    }
    match &command {
        CommandRoot::Search(args) => validation::cli_search(args)?,
        CommandRoot::Show(args) => validation::cli_show(args)?,
        CommandRoot::Mcp(_) => {}
        _ => bail!("--server is supported by search, show, and mcp serve"),
    }
    observed.stage = RemoteStage::Setup;
    let root = data_root
        .map(Ok)
        .unwrap_or_else(ctx_history_platform::default_data_root)?;
    let client = crate::hosted::remote_store(&root, name)?.remote_client()?;
    let mut ui = Ui::stdio(color);
    match command {
        CommandRoot::Search(args) => {
            let response = observed
                .request(|| client.search(args.query.as_deref().unwrap_or_default(), args.limit))?;
            observed.search(&response, args.limit);
            observed.stage = RemoteStage::Render;
            if args.format.is_json() {
                render::json(&mut ui, &response)?;
            } else {
                render::search(&mut ui, name, &response)?;
            }
            observed.rendered(response.results.len() as u64);
        }
        CommandRoot::Show(args) => show_observed(&client, name, args, &mut ui, observed)?,
        CommandRoot::Mcp(args) => {
            return crate::mcp::run_remote(args, RemoteBackend::new(client), root);
        }
        _ => unreachable!("remote operation was validated"),
    }
    finish_output(&mut ui, observed)
}

fn finish_output(ui: &mut Ui, observed: &mut Observation) -> Result<()> {
    observed.stage = RemoteStage::Flush;
    let result = ui.flush();
    observed.facts.output_flushed = Some(result.is_ok());
    result?;
    observed.stage = RemoteStage::Complete;
    Ok(())
}

fn show_observed(
    client: &RemoteClient,
    name: &str,
    args: ShowArgs,
    ui: &mut Ui,
    observed: &mut Observation,
) -> Result<()> {
    observed.stage = RemoteStage::Validation;
    match args.target {
        ShowTarget::Event(args) => {
            validation::citation(
                &args.id,
                CitationKind::Event,
                &client.connection().collection,
            )?;
            let event = observed.request(|| client.event(&args.id))?;
            observed.facts.returned = Some(1);
            observed.stage = RemoteStage::Render;
            if matches!(args.format, OutputFormat::Json | OutputFormat::Jsonl) {
                render::json(ui, &event)?;
            } else {
                render::event(ui, name, &event)?;
            }
            observed.rendered(1);
        }
        ShowTarget::Session(args) => {
            let citation = args
                .id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("shared session citation is required"))?;
            validation::citation(
                citation,
                CitationKind::Session,
                &client.connection().collection,
            )?;
            observed.facts.limit = args.max_events.map(|limit| limit as u64);
            observed.stage = RemoteStage::Render;
            let mut cursor = None;
            let mut returned = 0_usize;
            let mut first = true;
            // Stream pages through the ordinary CLI instead of retaining a
            // whole transcript in memory. Every page rechecks server authority.
            if args.format == OutputFormat::Json {
                ui.write_stdout_bytes(
                    b"{\"schema_version\":1,\"scope\":\"shared\",\"mode\":\"log\",\"events\":[",
                )?;
            } else if args.format == OutputFormat::Text {
                render::notice(
                    ui,
                    "Shared session log: all retained events, including tool activity.",
                )?;
            }
            loop {
                let remaining = args
                    .max_events
                    .map_or(100, |n| n.saturating_sub(returned).min(100));
                if remaining == 0 {
                    break;
                }
                let page =
                    observed.request(|| client.session(citation, cursor.as_deref(), remaining))?;
                observed.page(&page, cursor.is_some());
                observed.stage = RemoteStage::Render;
                let next = page.next_cursor;
                ensure!(
                    next.is_none() || next != cursor,
                    "remote history returned a non-advancing cursor"
                );
                for event in page.events {
                    match args.format {
                        OutputFormat::Json => {
                            if !first {
                                ui.write_stdout_bytes(b",")?;
                            }
                            ui.write_stdout_bytes(&serde_json::to_vec(&event)?)?;
                            first = false;
                        }
                        OutputFormat::Jsonl => render::json(ui, &event)?,
                        OutputFormat::Text => render::event(ui, name, &event)?,
                        OutputFormat::Markdown => unreachable!("validated output format"),
                    }
                    returned += 1;
                    observed.rendered(1);
                }
                cursor = next;
                if cursor.is_none() {
                    break;
                }
            }
            if args.format == OutputFormat::Json {
                ui.write_stdout_bytes(b"],\"next_cursor\":")?;
                ui.write_stdout_bytes(&serde_json::to_vec(&cursor)?)?;
                ui.write_stdout_bytes(b"}\n")?;
            } else if cursor.is_some() {
                render::notice(
                    ui,
                    "Shared transcript limited by --max-events; omit it to read all pages.",
                )?;
            }
        }
    }
    Ok(())
}
