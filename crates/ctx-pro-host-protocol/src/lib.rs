//! Exact, bounded Protocol V1 between the OSS `ctx` host and a local Pro helper.
//!
//! The public crate is the only wire authority. Private implementations mirror its
//! generated inventory and fingerprint; they do not define a compatible range.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use ctx_history_core::ContentRef;

pub const FRAME_MAGIC: &[u8; 6] = b"CTXPRO";
pub const PROTOCOL_VERSION: u16 = 1;
include!("protocol_fingerprint.rs");
pub const PROJECTION_CONTRACT_VERSION: u32 = 1;
pub const FRAME_HEADER_BYTES: usize = FRAME_MAGIC.len() + 2 + 4;
pub const MAX_FRAME_PAYLOAD_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_BLAME_RESULTS: u32 = 100;
pub const MAX_BLAME_CURSOR_BYTES: usize = 256;
pub const MAX_BLAME_EVIDENCE: usize = 3_200;
pub const MAX_BLAME_ATTRIBUTIONS_PER_MATCH: usize = 100;
pub const MAX_CITATIONS_PER_FACT: usize = 32;
pub const MAX_BLAME_TARGET_BYTES: usize = 8 * 1024;

mod entitlement;
pub use entitlement::{
    base64url, canonical_grant_bytes, decode_base64url, installation_key_thumbprint,
    installation_proof_bytes, AuthorizationRequest, AuthorizationResult, EntitlementAccessKind,
    EntitlementAccessState, EntitlementCapability, EntitlementGrant, SignedEntitlement,
    AUTHORIZATION_CHALLENGE_BYTES, ED25519_SIGNATURE_BYTES, ENTITLEMENT_CLOCK_SKEW_SECONDS,
    ENTITLEMENT_GRANT_SECONDS, ENTITLEMENT_MAX_GRACE_SECONDS,
    ENTITLEMENT_REFRESH_REMAINING_SECONDS, ENTITLEMENT_SCHEMA_VERSION,
    INSTALLATION_PUBLIC_KEY_BYTES,
};
mod error;
pub use error::{ErrorClass, ProtocolError};
mod frame;
pub use frame::{read_frame, write_frame, FrameError};
mod journal;
pub use journal::{
    canonical_payload_bytes, initial_journal_digest, journal_record_digest,
    journal_sync_envelope_bytes, sha256_hex, JournalCheckpoint, JournalContextWindow,
    JournalEntityKind, JournalEvidenceIdentity, JournalOperation, JournalPosition,
    JournalProvenanceIdentity, JournalRecord, JournalSyncMode, JournalSyncRequest,
    JournalSyncResult, MAX_AUTHORIZED_REPOSITORY_ROOTS,
    MAX_AUTHORIZED_REPOSITORY_ROOTS_TOTAL_BYTES, MAX_AUTHORIZED_REPOSITORY_ROOT_BYTES,
    MAX_JOURNAL_CONTEXT_BYTES, MAX_JOURNAL_CONTEXT_RECORDS, MAX_JOURNAL_EVIDENCE_PER_RECORD,
    MAX_JOURNAL_IDENTITY_BYTES, MAX_JOURNAL_PAYLOAD_BYTES, MAX_JOURNAL_RECORDS_PER_BATCH,
    MAX_JOURNAL_SYNC_ENVELOPE_BYTES,
};
mod layout;
pub use layout::{
    pro_clock_record_id, pro_graph_record_id, valid_pro_installation_id, ProFilesystemLayout,
    CTX_PRO_DATA_ROOT_ENV, CTX_PRO_INSTALLATION_ID_ENV, PRO_BIN_DIRECTORY_NAME,
    PRO_CLOCK_RECORD_ID_DOMAIN, PRO_DOWNLOADS_DIRECTORY_NAME, PRO_GRAPH_FILE_NAME,
    PRO_GRAPH_RECORD_ID_DOMAIN, PRO_HELPER_FILE_NAME, PRO_INSTALLATION_ID_FILE_NAME,
    PRO_LIFECYCLE_LOCK_FILE_NAME, PRO_PREVIOUS_HELPER_FILE_NAME, PRO_PREVIOUS_MARKER_FILE_NAME,
    PRO_PUBLISH_HELPER_FILE_NAME, PRO_PUBLISH_MARKER_FILE_NAME, PRO_ROLLBACK_HELPER_FILE_NAME,
    PRO_ROLLBACK_MARKER_FILE_NAME, PRO_ROOT_DIRECTORY_NAME, PRO_TRANSACTION_HELPER_FILE_NAME,
    PRO_TRANSACTION_JOURNAL_FILE_NAME, PRO_TRANSACTION_JOURNAL_NEXT_FILE_NAME,
    PRO_TRANSACTION_MARKER_FILE_NAME,
};
mod lifecycle;
pub use lifecycle::{
    ConfirmGraphKeyDeletionRequest, GraphKeyDeleted, GraphKeyDeletionPrepared,
    PrepareGraphKeyDeletionRequest, GRAPH_KEY_DELETION_CHALLENGE_BYTES,
    GRAPH_KEY_DELETION_CHALLENGE_TTL_SECONDS,
};
mod message;
pub use message::{
    Capability, GraphState, HelloRequest, HelloResult, HelperEnvelope, HelperMessage, HostEnvelope,
    HostMessage, MaterializationAuthority, StatusRequest, StatusResult,
};
mod output;
pub use output::{
    BeginOutputInventoryRequest, FinishOutputInventoryRequest, ObserveOutputSourceRequest,
    OutputAssociations, OutputCommandContext, OutputInventoryBegan, OutputInventoryFinished,
    OutputNativeCoordinate, OutputNativeCursor, OutputObservationKind, OutputOutcome,
    OutputOutcomeMetadata, OutputPageMaterialized, OutputProgressRequest, OutputProgressResult,
    OutputRepositoryContext, OutputSourceAvailability, OutputSourceDisposition,
    OutputSourceIdentity, OutputSourceLocator, OutputSourceObserved, OutputSourceProgress,
    ProOutputMaterializationPage, ProOutputObservation, ProviderOutputEvidence,
    TransientOutputContent, MAX_OUTPUT_COMMAND_BYTES, MAX_OUTPUT_CONTENT_BYTES,
    MAX_OUTPUT_CONTENT_BYTES_PER_PAGE, MAX_OUTPUT_CURSOR_BYTES, MAX_OUTPUT_IDENTITY_BYTES,
    MAX_OUTPUT_LOCATOR_BYTES, MAX_OUTPUT_OBSERVATIONS_PER_PAGE, MAX_OUTPUT_PROGRESS_SOURCES,
    OUTPUT_MATERIALIZATION_CONTRACT_VERSION,
};
mod query;
pub use query::{
    AgentAttribution, BlameContinuation, BlameMatch, BlameRequest, BlameResult, BlameTarget,
    CommitBlameMatch, CommitFactType, CommitPredicate, ContinuationReason, FactConfidence,
    FactState, FileBlameMatch, GitSnapshot, LineRange, NumberedEvidence, ProductionRelationship,
    PullRequestAction, PullRequestActivity, PullRequestBlameMatch, PullRequestBlameRelationship,
    PullRequestCommit, PullRequestCommitRelationship, QuerySnapshotExpectation,
    ResolvedBlameTarget, WorktreeStatus,
};
mod source_materialization;
pub use source_materialization::{
    certified_source_revision_sha256, legacy_source_manifest_sha256,
    source_manifest_receipt_sha256, AdmitSourceManifestPageRequest,
    BeginSourceManifestAdmissionRequest, BeginSourceManifestRequest, DeleteSourceRequest,
    FinishAdmittedSourceManifestRequest, FinishSourceManifestAdmissionRequest,
    FinishSourceManifestRequest, MaterializeSourcePageRequest, PrepareSourceRequest,
    SourceCommandFact, SourceDeleted, SourceDisposition, SourceManifest,
    SourceManifestAdmissionBegan, SourceManifestAdmissionCursor, SourceManifestAdmissionReceipt,
    SourceManifestAdmitted, SourceManifestBegan, SourceManifestFinished, SourceManifestHeader,
    SourceManifestPage, SourceManifestPageAdmitted, SourceManifestPageEntries,
    SourceManifestReceipt, SourceManifestReceiptIdentity, SourceMessageFact, SourceOutcome,
    SourcePageMaterialized, SourcePrepared, SourceProgress, SourceRecord, SourceRecordMetadata,
    SourceRemoval, SourceRepositoryContext, SourceResultFact, SourceSessionRelationships,
    TransientSourceContent, TransientSourceFact, MAX_SOURCE_CONTENT_BYTES,
    MAX_SOURCE_CONTENT_BYTES_PER_PAGE, MAX_SOURCE_CONTROL_WIRE_BYTES, MAX_SOURCE_FACTS_PER_RECORD,
    MAX_SOURCE_IDENTITY_BYTES, MAX_SOURCE_INVENTORY_SOURCES, MAX_SOURCE_MANIFEST_PAGE_ITEMS,
    MAX_SOURCE_MANIFEST_PAGE_WIRE_BYTES, MAX_SOURCE_MANIFEST_REMOVALS, MAX_SOURCE_MANIFEST_SOURCES,
    MAX_SOURCE_MANIFEST_WIRE_BYTES, MAX_SOURCE_PAGE_WIRE_BYTES, MAX_SOURCE_PATH_BYTES,
    MAX_SOURCE_PROGRESS_SOURCES, MAX_SOURCE_RECORDS_PER_PAGE, MAX_SOURCE_TOUCHED_FILES_PER_RECORD,
    SOURCE_MATERIALIZATION_CONTRACT_VERSION,
};
mod fake;
pub use fake::{FakeBlameFailure, FakeHelper};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationKind {
    Event,
    FileTouch,
    VcsChange,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ByteRange {
    pub start: u64,
    pub end_exclusive: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceCitation {
    pub observation_id: Option<Uuid>,
    pub observation_seq: Option<u64>,
    pub observation_kind: Option<ObservationKind>,
    pub session_id: Option<Uuid>,
    pub event_id: Option<Uuid>,
    pub event_seq: Option<u64>,
    pub source_path: Option<String>,
    pub fixture_line: Option<u64>,
    pub source_record_ordinal: Option<u64>,
    pub source_record_subrecord_index: Option<u32>,
    pub byte_range: Option<ByteRange>,
    pub source_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_output: Option<ProviderOutputEvidence>,
}

impl EvidenceCitation {
    #[must_use]
    pub fn is_usable(&self) -> bool {
        if let Some(provider_output) = &self.provider_output {
            return self.observation_id.is_none()
                && self.observation_seq.is_none()
                && self.observation_kind.is_none()
                && self.session_id.is_none()
                && self.event_id.is_none()
                && self.event_seq.is_none()
                && self.source_path.is_none()
                && self.fixture_line.is_none()
                && self.source_record_ordinal.is_none()
                && self.source_record_subrecord_index.is_none()
                && self.byte_range.is_none()
                && self.source_sha256.is_none()
                && provider_output.is_usable();
        }
        let coordinate_is_complete = self.observation_id.is_some()
            == (self.observation_seq.is_some() && self.observation_kind.is_some());
        let fields_are_valid = coordinate_is_complete
            && self.observation_id.is_none_or(|id| !id.is_nil())
            && self.session_id.is_none_or(|id| !id.is_nil())
            && self.event_id.is_none_or(|id| !id.is_nil())
            && self
                .source_path
                .as_deref()
                .is_none_or(|path| !path.trim().is_empty() && path.len() <= MAX_BLAME_TARGET_BYTES)
            && self.fixture_line.is_none_or(|line| line > 0)
            && self
                .byte_range
                .as_ref()
                .is_none_or(|range| range.start <= range.end_exclusive)
            && self.source_sha256.as_deref().is_none_or(is_lower_sha256)
            && (self.source_record_subrecord_index.is_none()
                || self.source_record_ordinal.is_some());
        if !fields_are_valid {
            return false;
        }
        let canonical =
            self.observation_id.is_some() || self.event_id.is_some() || self.session_id.is_some();
        let source = self.source_path.is_some()
            && (self.fixture_line.is_some()
                || self.source_record_ordinal.is_some()
                || self.byte_range.is_some()
                || self.source_sha256.is_some());
        canonical || source
    }
}

fn is_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Repository,
    Checkout,
    Worktree,
    Branch,
    Commit,
    File,
    PullRequest,
    Issue,
    Remote,
    Release,
    Command,
    Check,
    Session,
    Agent,
    Run,
}

impl ResourceKind {
    pub const ALL: [Self; 15] = [
        Self::Repository,
        Self::Checkout,
        Self::Worktree,
        Self::Branch,
        Self::Commit,
        Self::File,
        Self::PullRequest,
        Self::Issue,
        Self::Remote,
        Self::Release,
        Self::Command,
        Self::Check,
        Self::Session,
        Self::Agent,
        Self::Run,
    ];

    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Repository => "repository",
            Self::Checkout => "checkout",
            Self::Worktree => "worktree",
            Self::Branch => "branch",
            Self::Commit => "commit",
            Self::File => "file",
            Self::PullRequest => "pull_request",
            Self::Issue => "issue",
            Self::Remote => "remote",
            Self::Release => "release",
            Self::Command => "command",
            Self::Check => "check",
            Self::Session => "session",
            Self::Agent => "agent",
            Self::Run => "run",
        }
    }

    #[must_use]
    pub fn from_wire_name(value: &str) -> Option<Self> {
        match value {
            "repository" => Some(Self::Repository),
            "checkout" => Some(Self::Checkout),
            "worktree" => Some(Self::Worktree),
            "branch" => Some(Self::Branch),
            "commit" => Some(Self::Commit),
            "file" => Some(Self::File),
            "pull_request" => Some(Self::PullRequest),
            "issue" => Some(Self::Issue),
            "remote" => Some(Self::Remote),
            "release" => Some(Self::Release),
            "command" => Some(Self::Command),
            "check" => Some(Self::Check),
            "session" => Some(Self::Session),
            "agent" => Some(Self::Agent),
            "run" => Some(Self::Run),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceRef {
    pub id: String,
    pub kind: ResourceKind,
    pub display: String,
}

impl ResourceRef {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.id.trim().is_empty()
            || self.id.len() > MAX_BLAME_TARGET_BYTES
            || self.id.chars().any(char::is_control)
            || self.display.trim().is_empty()
            || self.display.len() > MAX_BLAME_TARGET_BYTES
            || self.display.chars().any(char::is_control)
        {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "resource reference is empty, unsafe, or exceeds its byte bound",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[cfg(test)]
mod conformance;
