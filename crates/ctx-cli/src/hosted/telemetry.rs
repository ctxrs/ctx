//! Best-effort completion metrics using the existing consent and durable outbox.
//! This path never starts a daemon or sends a telemetry request.
use std::{path::Path, time::Duration};

use ctx_client_observability::analytics::{
    hosted_operation_completed, HostedFailureTypeV1 as FailureType, HostedFailureV1 as Failure,
    OutputKind,
};

use super::HostedCommand;

#[derive(Debug)]
pub(super) struct OutputFailure;

impl std::fmt::Display for OutputFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("could not write command output")
    }
}

impl std::error::Error for OutputFailure {}

pub(super) fn operation(command: &HostedCommand) -> Option<super::HostedOperation> {
    use super::HostedOperation as Operation;
    match command {
        HostedCommand::Archive(args) => Some(
            args.telemetry_operation()
                .map(Operation::Existing)
                .unwrap_or(Operation::ArchiveVerify),
        ),
        HostedCommand::Server(args) => args.observed_operation(),
        HostedCommand::Remote(args) => Some(args.observed_operation()),
    }
}

pub(super) fn record(
    command: &HostedCommand,
    root: Option<&Path>,
    result: &anyhow::Result<()>,
    duration: Duration,
) {
    let operation = match command {
        HostedCommand::Archive(args) => args.telemetry_operation(),
        HostedCommand::Server(args) => args.telemetry_operation(),
        HostedCommand::Remote(args) => args.telemetry_operation(),
    };
    let Some(operation) = operation else { return };
    let Ok(root) = super::data_root(root) else {
        return;
    };
    let output = if super::json_output(command) {
        OutputKind::Json
    } else {
        OutputKind::Human
    };
    let event = hosted_operation_completed(
        operation,
        output,
        result.as_ref().copied().map_err(classify),
        duration,
    );
    // Optional diagnostics cannot change command success or emit delivery errors.
    let _ = crate::observability_composition::append_analytics_batch(&root, &[event]);
}

pub(super) fn classify(error: &anyhow::Error) -> Failure {
    if error.is::<OutputFailure>() {
        return Failure::Output;
    }
    if error.is::<std::io::Error>() {
        return Failure::Operation(FailureType::Io);
    }
    let failure = match super::error_code(error) {
        "invalid_request" => FailureType::InvalidRequest,
        "unauthorized" => FailureType::Unauthorized,
        "forbidden" => FailureType::Forbidden,
        "not_found" => FailureType::NotFound,
        "conflict" | "destination_changed" | "busy" => FailureType::Conflict,
        "credentials" | "not_connected" => FailureType::Credentials,
        "policy_denied" => FailureType::PolicyDenied,
        "unavailable" | "staging_expired" | "history_unavailable" => FailureType::Unavailable,
        "capacity" | "too_large" | "rate_limited" => FailureType::Capacity,
        "archive_io" => FailureType::Io,
        "invalid_archive" => FailureType::InvalidArchive,
        _ => FailureType::Other,
    };
    Failure::Operation(failure)
}

#[cfg(test)]
mod tests;
