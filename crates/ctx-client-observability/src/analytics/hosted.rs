//! Content-free terminals for explicitly invoked hosted workflows.

use std::time::Duration;

use serde_json::{json, Map, Value};

use super::{duration_bucket, DurationBucket, Outcome, OutputKind, PublicEventV1};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedOperationV1 {
    ArchiveExport,
    ArchiveRestore,
    ArchiveVerify,
    RemoteConnect,
    RemoteShare,
    RemoteSync,
    RemotePause,
    RemoteResume,
    RemoteStatus,
    RemoteRemove,
    ServerInit,
    ServerInvite,
    ServerGrant,
    ServerRevoke,
    ServerWithdraw,
    ServerBackup,
    ServerCollectionCreate,
    ServerUserList,
    ServerUserCredentials,
    ServerUserCreate,
    ServerUserCredential,
    ServerPublications,
    ServerStatus,
    ServerRestore,
}

impl HostedOperationV1 {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::ArchiveExport => "archive_export",
            Self::ArchiveRestore => "archive_restore",
            Self::ArchiveVerify => "archive_verify",
            Self::RemoteConnect => "remote_connect",
            Self::RemoteShare => "remote_share",
            Self::RemoteSync => "remote_sync",
            Self::RemotePause => "remote_pause",
            Self::RemoteResume => "remote_resume",
            Self::RemoteStatus => "remote_status",
            Self::RemoteRemove => "remote_remove",
            Self::ServerInit => "server_init",
            Self::ServerInvite => "server_invite",
            Self::ServerGrant => "server_grant",
            Self::ServerRevoke => "server_revoke",
            Self::ServerWithdraw => "server_withdraw",
            Self::ServerBackup => "server_backup",
            Self::ServerCollectionCreate => "server_collection_create",
            Self::ServerUserList => "server_user_list",
            Self::ServerUserCredentials => "server_user_credentials",
            Self::ServerUserCreate => "server_user_create",
            Self::ServerUserCredential => "server_user_credential",
            Self::ServerPublications => "server_publications",
            Self::ServerStatus => "server_status",
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
    measurements: Option<HostedMeasurements>,
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
        measurements: None,
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
        if let Some(facts) = self.measurements {
            super::engines::insert_hosted_measurements(&mut properties, facts);
        }
        properties
    }
}

/// Optional complete measured sidecar; old hosted terminals remain valid without it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostedMeasurements {
    pub elapsed: Duration,
    pub timings: super::ProductTimings,
    pub delivery: super::DeliveryEvidence,
    pub result: Option<super::ProductResultFacts>,
}
/// The legacy failure tuple remains authoritative. Contradictory delivery evidence
/// is omitted as a whole sidecar instead of changing the command outcome.
pub fn hosted_operation_completed_with_measurements(
    operation: HostedOperationV1,
    output: OutputKind,
    result: Result<(), HostedFailureV1>,
    duration: Duration,
    facts: HostedMeasurements,
) -> PublicEventV1 {
    let output_failed = matches!(result, Err(HostedFailureV1::Output));
    let consistent = (facts.delivery == super::DeliveryEvidence::Failed) == output_failed;
    PublicEventV1::HostedOperationCompleted(HostedOperationCompletedV1 {
        operation,
        duration: duration_bucket(duration),
        output,
        result,
        measurements: consistent.then_some(facts),
    })
}

#[cfg(test)]
mod tests;
