use std::{
    collections::BTreeSet,
    io::Read,
    path::Path,
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_history_core::database_path;
use ctx_history_store::{
    JournalCheckpoint as StoreJournalCheckpoint, JournalEntityKind as StoreJournalEntityKind,
    JournalEvidenceIdentity as StoreJournalEvidenceIdentity,
    JournalOperation as StoreJournalOperation, JournalPosition as StoreJournalPosition,
    JournalProvenanceIdentity as StoreJournalProvenanceIdentity, ProjectionJournalRecord,
    ProjectionJournalSnapshot, Store,
};
#[cfg(test)]
use ctx_pro_host_protocol::MAX_JOURNAL_RECORDS_PER_BATCH;
use ctx_pro_host_protocol::{
    decode_base64url, initial_journal_digest, journal_sync_envelope_bytes, read_frame, write_frame,
    BlameResult, BlameTarget, Capability, ConfirmGraphKeyDeletionRequest, EntitlementAccessState,
    GraphKeyDeletionPrepared, GraphState, HelloRequest, HelperEnvelope, HelperMessage,
    HostEnvelope, HostMessage, JournalCheckpoint, JournalContextWindow, JournalEntityKind,
    JournalEvidenceIdentity, JournalOperation, JournalPosition, JournalProvenanceIdentity,
    JournalRecord, JournalSyncMode, JournalSyncRequest, JournalSyncResult,
    PrepareGraphKeyDeletionRequest, StatusRequest, StatusResult,
    GRAPH_KEY_DELETION_CHALLENGE_BYTES, MAX_JOURNAL_SYNC_ENVELOPE_BYTES, PROTOCOL_FINGERPRINT,
    PROTOCOL_VERSION,
};
use serde::Serialize;
use uuid::Uuid;

use crate::analytics::{
    pro_helper_connection_outcome, ProHelperConnectionOutcomeV1, ProMaterializationModeV1,
    ProMaterializationTelemetryV1,
};

#[path = "client_support.rs"]
mod support;
use super::authorization::{
    AuthorizationProvider, EntitlementSchedule, StoredAuthorizationProvider,
};
use super::credential_vault::CredentialVaultNamespace;
use super::helper_command;
use super::verified_executable::VerifiedHelperExecutable;
pub(crate) use support::default_helper_path;
use support::{git_executable, helper_executable};

#[path = "client_errors.rs"]
mod errors;
use errors::protocol_error;
pub(crate) use errors::stable_error_code;

#[path = "client_output.rs"]
mod output;
pub(crate) use output::ProOutputImport;

#[path = "client_status.rs"]
mod client_status;
#[cfg(all(test, unix))]
use client_status::smoke_helper_at_path_with_authorization;
#[cfg(ctx_pro_qualification)]
pub(crate) use client_status::smoke_qualification_helper;
#[cfg(test)]
use client_status::status_outcome;
#[cfg(test)]
pub(crate) use client_status::status_with_helper_resolver;
pub(crate) use client_status::{
    smoke_helper_at_path, status, HelperSmoke, ProSetupRepairability, ProStatus,
};

#[cfg(unix)]
use std::os::unix::process::CommandExt as _;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
const BLAME_TIMEOUT: Duration = Duration::from_secs(30);
const BATCH_TIMEOUT: Duration = Duration::from_secs(60);
const STDERR_MAX_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MaterializeReport {
    pub(crate) schema_version: u32,
    pub(crate) payload_type: &'static str,
    pub(crate) frontier: u64,
    pub(crate) batches: u64,
    pub(crate) observations: u64,
    pub(crate) replayed_batches: u64,
}

mod materialization;
mod operations;
mod transport;

pub(crate) use materialization::materialize;
use materialization::*;
pub(crate) use operations::blame;
pub(super) use operations::delete_graph_key;
#[cfg(test)]
use operations::*;
pub(crate) use transport::ProClient;
#[cfg(test)]
use transport::*;

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
