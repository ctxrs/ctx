//! Explicit remote reads. This adapter never initializes local history storage.

mod backend;
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

pub(crate) fn run(
    mut command: CommandRoot,
    data_root: Option<PathBuf>,
    name: &str,
    color: ColorMode,
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
    let root = data_root
        .map(Ok)
        .unwrap_or_else(ctx_history_platform::default_data_root)?;
    let client = crate::hosted::remote_store(&root, name)?.remote_client()?;
    let mut ui = Ui::stdio(color);
    match command {
        CommandRoot::Search(args) => {
            let response = client.search(args.query.as_deref().unwrap_or_default(), args.limit)?;
            if args.format.is_json() {
                render::json(&mut ui, &response)?;
            } else {
                render::search(&mut ui, name, &response)?;
            }
        }
        CommandRoot::Show(args) => show(&client, name, args, &mut ui)?,
        CommandRoot::Mcp(args) => return crate::mcp::run_remote(args, RemoteBackend::new(client)),
        _ => unreachable!("remote operation was validated"),
    }
    ui.flush()?;
    Ok(())
}

fn show(client: &RemoteClient, name: &str, args: ShowArgs, ui: &mut Ui) -> Result<()> {
    match args.target {
        ShowTarget::Event(args) => {
            validation::citation(
                &args.id,
                CitationKind::Event,
                &client.connection().collection,
            )?;
            let event = client.event(&args.id)?;
            if matches!(args.format, OutputFormat::Json | OutputFormat::Jsonl) {
                render::json(ui, &event)?;
            } else {
                render::event(ui, name, &event)?;
            }
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
                let page = client.session(citation, cursor.as_deref(), remaining)?;
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
