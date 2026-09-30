//! Content-free terminals for explicitly invoked hosted workflows.

use std::time::Duration;

use serde_json::{json, Map, Value};

use super::{duration_bucket, DurationBucket, Outcome, OutputKind, PublicEventV1};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedOperationV1 {
    ArchiveExport,
    ArchiveRestore,
    RemoteConnect,
    RemoteShare,
    RemoteSync,
    ServerInit,
    ServerInvite,
    ServerGrant,
    ServerRevoke,
    ServerWithdraw,
    ServerBackup,
    ServerRestore,
}

impl HostedOperationV1 {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::ArchiveExport => "archive_export",
            Self::ArchiveRestore => "archive_restore",
            Self::RemoteConnect => "remote_connect",
            Self::RemoteShare => "remote_share",
            Self::RemoteSync => "remote_sync",
            Self::ServerInit => "server_init",
            Self::ServerInvite => "server_invite",
            Self::ServerGrant => "server_grant",
            Self::ServerRevoke => "server_revoke",
            Self::ServerWithdraw => "server_withdraw",
            Self::ServerBackup => "server_backup",
            Self::ServerRestore => "server_restore",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedFailureTypeV1 {
    InvalidRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    Credentials,
    PolicyDenied,
    Unavailable,
    Capacity,
    Io,
    InvalidArchive,
    Other,
}

impl HostedFailureTypeV1 {
    const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::Credentials => "credentials",
            Self::PolicyDenied => "policy_denied",
            Self::Unavailable => "unavailable",
            Self::Capacity => "capacity",
            Self::Io => "io",
            Self::InvalidArchive => "invalid_archive",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedFailureV1 {
    Operation(HostedFailureTypeV1),
    /// The command succeeded but its final output could not be delivered.
    Output,
}

#[derive(Debug)]
pub struct HostedOperationCompletedV1 {
    pub(super) operation: HostedOperationV1,
    pub(super) duration: DurationBucket,
    output: OutputKind,
    result: Result<(), HostedFailureV1>,
}

/// Record once after command execution and final output delivery. The caller
/// owns consent and delivery; no raw errors or command arguments enter here.
pub fn hosted_operation_completed(
    operation: HostedOperationV1,
    output: OutputKind,
    result: Result<(), HostedFailureV1>,
    duration: Duration,
) -> PublicEventV1 {
    PublicEventV1::HostedOperationCompleted(HostedOperationCompletedV1 {
        operation,
        duration: duration_bucket(duration),
        output,
        result,
    })
}

impl HostedOperationCompletedV1 {
    pub(super) const fn outcome(&self) -> Outcome {
        match self.result {
            Ok(()) => Outcome::Success,
            Err(_) => Outcome::Failure,
        }
    }

    pub(super) fn properties(&self) -> Map<String, Value> {
        let mut properties = Map::new();
        properties.insert("output".to_owned(), json!(self.output.as_str()));
        if let Err(failure) = self.result {
            let (stage, kind) = match failure {
                HostedFailureV1::Operation(kind) => ("operation", kind),
                HostedFailureV1::Output => ("output", HostedFailureTypeV1::Io),
            };
            properties.insert("hosted_failure_stage".to_owned(), json!(stage));
            properties.insert("failure_type".to_owned(), json!(kind.as_str()));
        }
        properties
    }
}

#[cfg(test)]
mod tests;
