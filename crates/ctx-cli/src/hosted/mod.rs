//! Explicit archive, hosted administration, and remote publication adapters.

mod archive;
mod credentials;
mod remote;
mod server;

pub(crate) use remote::store as remote_store;

use std::path::Path;

use anyhow::Result;
use clap::Subcommand;
use serde_json::{json, Value};

use crate::{
    output::JsonOutputFormat,
    ui::{ColorMode, Document, Line, Ui},
};

#[derive(Debug, Subcommand)]
pub(crate) enum HostedCommand {
    /// Export, verify, and restore portable retained history.
    Archive(archive::ArchiveArgs),
    /// Run or administer an explicitly selected history server.
    Server(server::ServerArgs),
    /// Connect to a history server and explicitly select history to share.
    Remote(remote::RemoteArgs),
}

pub(crate) fn json_output(command: &HostedCommand) -> bool {
    match command {
        HostedCommand::Archive(args) => args.format.is_json(),
        HostedCommand::Server(args) => args.format.is_json(),
        HostedCommand::Remote(args) => args.format.is_json(),
    }
}

/// Dispatch before local configuration, telemetry, or daemon initialization.
pub(crate) fn run(
    command: &HostedCommand,
    data_root: Option<&Path>,
    color: ColorMode,
) -> Result<()> {
    let mut ui = Ui::stdio(color);
    let result = match command {
        HostedCommand::Archive(args) => archive::run(args, data_root, &mut ui),
        HostedCommand::Server(args) => server::run(args, data_root, &mut ui),
        HostedCommand::Remote(args) => remote::run(args, data_root, &mut ui),
    };
    if let Err(error) = result {
        if json_output(command) {
            writeln!(
                ui.stderr_writer(),
                "{}",
                json!({"schema_version": 1, "error": {
                    "code": error_code(&error), "message": error.to_string()
                }})
            )?;
            ui.flush()?;
            return Err(crate::dispatch::rendered_cli_error());
        }
        return Err(error);
    }
    ui.flush()?;
    Ok(())
}

fn error_code(error: &anyhow::Error) -> &'static str {
    use ctx_history_archive::ArchiveError;
    use ctx_history_server::Error as Server;
    use ctx_history_sharing::Error as Sharing;

    if let Some(error) = error.downcast_ref::<ArchiveError>() {
        return match error {
            ArchiveError::Conflict { .. } => "conflict",
            ArchiveError::Io(_) => "archive_io",
            _ => "invalid_archive",
        };
    }

    if let Some(error) = error.downcast_ref::<Server>() {
        return match error {
            Server::Unauthorized => "unauthorized",
            Server::Forbidden => "forbidden",
            Server::NotFound => "not_found",
            Server::Conflict => "conflict",
            Server::RecoveryClosed => "recovery_closed",
            Server::Unavailable => "unavailable",
            Server::Capacity => "capacity",
            Server::Expired => "staging_expired",
            Server::Invalid(_) => "invalid_request",
            _ => "server_error",
        };
    }
    if let Some(error) = error.downcast_ref::<Sharing>() {
        return match error {
            Sharing::Unauthorized => "unauthorized",
            Sharing::Forbidden => "forbidden",
            Sharing::NotFound => "not_found",
            Sharing::Conflict | Sharing::PolicyConflict => "conflict",
            Sharing::NotConnected => "not_connected",
            Sharing::Credentials | Sharing::MissingCredential => "credentials",
            Sharing::PolicyDenied => "policy_denied",
            Sharing::Unavailable => "unavailable",
            Sharing::StagingExpired => "staging_expired",
            Sharing::InvalidEndpoint | Sharing::InvalidConfig => "invalid_request",
            Sharing::DestinationChanged => "destination_changed",
            Sharing::Busy => "busy",
            Sharing::TooLarge => "too_large",
            Sharing::RateLimited => "rate_limited",
            Sharing::Archive => "history_unavailable",
            _ => "sharing_error",
        };
    }
    "hosted_error"
}

fn data_root(explicit: Option<&Path>) -> Result<std::path::PathBuf> {
    Ok(match explicit {
        Some(path) => path.to_path_buf(),
        None => ctx_history_platform::default_data_root()?,
    })
}

fn print_result(
    format: JsonOutputFormat,
    value: Value,
    text: impl AsRef<str>,
    ui: &mut Ui,
) -> Result<()> {
    if format.is_json() {
        ctx_terminal::print_json(value)
    } else {
        for line in text.as_ref().lines() {
            ui.write_stdout(&Document::from_line(Line::text(line)))?;
        }
        Ok(())
    }
}
