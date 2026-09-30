//! Explicit archive, hosted administration, and remote publication adapters.

mod archive;
#[cfg(test)]
mod completion_tests;
mod credentials;
mod observation;
mod remote;
mod server;
mod telemetry;
pub(crate) use observation::{HostedCompletion, HostedObservers, HostedOperation};

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
    /// Run or administer an opt-in beta history server.
    Server(server::ServerArgs),
    /// Connect to a history server and explicitly select history to share.
    Remote(remote::RemoteArgs),
}

impl HostedCommand {
    pub(crate) fn needs_live_observers(&self) -> bool {
        match self {
            Self::Archive(_) => false,
            Self::Server(args) => args.needs_live_observers(),
            Self::Remote(args) => args.needs_live_observers(),
        }
    }

    /// Only used to decide whether optional telemetry may create identity after
    /// a failed restore. The restore implementation still owns all validation.
    pub(crate) fn restore_ownership_marker(
        &self,
        root: Option<&Path>,
    ) -> Option<std::path::PathBuf> {
        use ctx_client_observability::analytics::HostedOperationV1;
        match self {
            Self::Archive(args)
                if args.telemetry_operation() == Some(HostedOperationV1::ArchiveRestore) =>
            {
                data_root(root)
                    .ok()
                    .map(|root| root.join("archive-root.json"))
            }
            Self::Server(args) => args.restore_ownership_marker(root),
            _ => None,
        }
    }
}

pub(crate) fn json_output(command: &HostedCommand) -> bool {
    match command {
        HostedCommand::Archive(args) => args.format.is_json(),
        HostedCommand::Server(args) => args.format.is_json(),
        HostedCommand::Remote(args) => args.format.is_json(),
    }
}

/// Hosted operations never initialize the local index or daemon. Selected
/// completed operations may append consent-controlled, content-free analytics.
pub(crate) fn run_with_observers(
    command: &HostedCommand,
    data_root: Option<&Path>,
    color: ColorMode,
    observers: HostedObservers,
) -> Result<()> {
    let mut ui = Ui::stdio(color);
    run_with_ui(command, data_root, &mut ui, &observers)
}

fn run_with_ui(
    command: &HostedCommand,
    data_root: Option<&Path>,
    ui: &mut Ui,
    observers: &HostedObservers,
) -> Result<()> {
    let started = std::time::Instant::now();
    let mut result = (match command {
        HostedCommand::Archive(args) => archive::run(args, data_root, ui),
        HostedCommand::Server(args) => server::run(args, data_root, ui, observers),
        HostedCommand::Remote(args) => remote::run(args, data_root, ui, observers.sharing.clone()),
    })
    .and_then(|()| {
        ui.flush()
            .map_err(|error| anyhow::Error::new(error).context(telemetry::OutputFailure))
    });
    let mut rendered_error = false;
    if json_output(command) {
        if let Err(error) = &result {
            let output = writeln!(
                ui.stderr_writer(),
                "{}",
                json!({"schema_version": 1, "error": {
                    "code": error_code(error), "message": error.to_string()
                }})
            )
            .and_then(|()| ui.flush());
            match output {
                Ok(()) => rendered_error = true,
                Err(error) => {
                    result = Err(anyhow::Error::new(error).context(telemetry::OutputFailure));
                }
            }
        }
    }
    if let Some(completion) = &observers.completion {
        if let Some(operation) = telemetry::operation(command) {
            completion(HostedCompletion {
                operation,
                output: if json_output(command) {
                    crate::analytics::OutputKind::Json
                } else {
                    crate::analytics::OutputKind::Human
                },
                duration: started.elapsed(),
                result: result.as_ref().copied().map_err(telemetry::classify),
            });
        }
    } else {
        telemetry::record(command, data_root, &result, started.elapsed());
    }
    if rendered_error {
        // The terminal above retains the operation/output failure. This marker
        // only tells the caller that its JSON error was already written.
        Err(crate::dispatch::rendered_cli_error())
    } else {
        result
    }
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
    let result = (|| {
        if format.is_json() {
            ui.write_stdout_bytes(&serde_json::to_vec_pretty(&value)?)?;
            ui.write_stdout_bytes(b"\n")?;
            Ok(())
        } else {
            for line in text.as_ref().lines() {
                ui.write_stdout(&Document::from_line(Line::text(line)))?;
            }
            Ok(())
        }
    })();
    result.map_err(|error: anyhow::Error| error.context(telemetry::OutputFailure))
}
