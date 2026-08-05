//! Exact, bounded Protocol V2 between the OSS `ctx` host and a local Pro helper.
//!
//! The public crate is the only wire authority. Private products consume this
//! crate and its generated inventory at one exact source revision.

use serde::{Deserialize, Serialize};

pub use ctx_history_core::{CoreRecord, SourceKey, StableEntityId};

pub const FRAME_MAGIC: &[u8; 6] = b"CTXPRO";
pub const PROTOCOL_VERSION: u16 = 2;
include!("protocol_fingerprint.rs");
pub const FRAME_HEADER_BYTES: usize = FRAME_MAGIC.len() + 2 + 4;
pub const MAX_FRAME_PAYLOAD_BYTES: usize = 80 * 1024 * 1024;
pub const MAX_BLAME_RESULTS: u32 = 100;
pub const MAX_BLAME_CURSOR_BYTES: usize = 256;
pub const MAX_BLAME_EVIDENCE: usize = 3_200;
pub const MAX_BLAME_ATTRIBUTIONS_PER_MATCH: usize = 100;
pub const MAX_BLAME_DIAGNOSTIC_CANDIDATES: usize = 5;
pub const MAX_CITATIONS_PER_FACT: usize = 32;
pub const MAX_BLAME_TARGET_BYTES: usize = 8 * 1024;
/// Canonical generated Protocol V2 inventory shipped by this exact crate revision.
pub const PROTOCOL_INVENTORY_JSON: &str = include_str!("../testdata/v2/inventory.json");
/// Canonical entitlement vectors shipped by this exact crate revision.
pub const ENTITLEMENT_GOLDEN_JSON: &str = include_str!("../testdata/entitlement/v1/golden.json");

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
pub use error::{
    BlameDiagnosticCandidate, BlameDiagnosticDetails, BlameDiagnosticReason, ErrorClass,
    ProtocolError,
};
mod frame;
pub use frame::{read_frame, write_frame, FrameError};
mod layout;
pub use layout::{
    is_pro_graph_artifact_file_name, pro_clock_record_id, pro_graph_record_id,
    valid_pro_installation_id, ProFilesystemLayout, CTX_PRO_DATA_ROOT_ENV,
    CTX_PRO_INSTALLATION_ID_ENV, PRO_BIN_DIRECTORY_NAME, PRO_CLOCK_RECORD_ID_DOMAIN,
    PRO_DOWNLOADS_DIRECTORY_NAME, PRO_GRAPH_DIRECTORY_NAME, PRO_GRAPH_RECORD_ID_DOMAIN,
    PRO_HELPER_FILE_NAME, PRO_INSTALLATION_ID_FILE_NAME, PRO_LIFECYCLE_LOCK_FILE_NAME,
    PRO_PREVIOUS_HELPER_FILE_NAME, PRO_PREVIOUS_MARKER_FILE_NAME, PRO_PUBLISH_HELPER_FILE_NAME,
    PRO_PUBLISH_MARKER_FILE_NAME, PRO_ROLLBACK_HELPER_FILE_NAME, PRO_ROLLBACK_MARKER_FILE_NAME,
    PRO_ROOT_DIRECTORY_NAME, PRO_TRANSACTION_HELPER_FILE_NAME, PRO_TRANSACTION_JOURNAL_FILE_NAME,
    PRO_TRANSACTION_JOURNAL_NEXT_FILE_NAME, PRO_TRANSACTION_MARKER_FILE_NAME,
};
mod lifecycle;
pub use lifecycle::{
    ConfirmGraphKeyDeletionRequest, GraphKeyDeleted, GraphKeyDeletionPrepared,
    PrepareGraphKeyDeletionRequest, GRAPH_KEY_DELETION_CHALLENGE_BYTES,
    GRAPH_KEY_DELETION_CHALLENGE_TTL_SECONDS,
};
mod message;
pub use message::{
    apply_core_source_delta_page_request_frame_wire_bytes,
    core_source_delta_page_applied_frame_wire_bytes, Capability, CoreProjectionCurrentness,
    HelloRequest, HelloResult, HelperEnvelope, HelperMessage, HostEnvelope, HostMessage,
    JournalFinishActivity, MaterializedCoverage, ProAccessState, ProAccessStatus, ProOperation,
    ProStorageEvidence, RepositoryCoverage, StatusRequest, StatusResult,
    MAX_JOURNAL_FINISH_WORKERS,
};
mod query;
pub use query::{
    canonical_logical_repository_id, AgentAttribution, BlameAttribution, BlameContinuation,
    BlameCoverage, BlameCoverageUnit, BlameMatch, BlameOutcome, BlameRequest, BlameResult,
    BlameTarget, CommitBlameMatch, CommitFactType, CommitPredicate, ContinuationReason,
    FactConfidence, FactState, FileBlameMatch, GitSnapshot, LineRange, NumberedEvidence,
    ProductionRelationship, PullRequestAction, PullRequestActivity, PullRequestBlameMatch,
    PullRequestBlameRelationship, PullRequestCommit, PullRequestCommitRelationship,
    QuerySnapshotExpectation, ResolvedBlameTarget, WorktreeStatus,
};
mod core_materialization;
pub use core_materialization::{
    core_materialization_id, core_record_digests, core_record_digests_from_encoded,
    core_record_leaf_sha256, core_record_sha256, core_source_snapshot_sha256,
    ApplyCoreEventDeltaPageRequest, ApplyCoreEventDeltaPagesRequest,
    ApplyCoreSourceDeltaPageRequest, BeginCoreMaterializationRequest, CoreEventDelta,
    CoreEventDeltaPage, CoreEventDeltaPageAcknowledgementIdentity, CoreEventDeltaPageApplied,
    CoreEventDeltaPageBuilder, CoreEventDeltaPagesAcknowledgementIdentity,
    CoreEventDeltaPagesApplied, CoreEventReplacement, CoreEventState, CoreEventStatePage,
    CoreEventStatePageRequest, CoreEventTombstone, CoreGenerationHead, CoreMaterializationBegan,
    CoreMaterializationBeginAcknowledgementIdentity, CoreMaterializationFinished,
    CoreMaterializationReceipt, CoreMaterializationReceiptIdentity, CoreRecordDigests,
    CoreSourceDelta, CoreSourceDeltaPage, CoreSourceDeltaPageAcknowledgementIdentity,
    CoreSourceDeltaPageApplied, CoreSourceReconciliation, CoreSourceRemoval, CoreSourceState,
    FinishCoreMaterializationRequest, CORE_MATERIALIZATION_CONTRACT_VERSION,
    MAX_CORE_CONTROL_WIRE_BYTES, MAX_CORE_EVENT_DELTA_PAGES,
    MAX_CORE_EVENT_DELTA_PAGES_PREPARED_OUTPUT_BYTES,
    MAX_CORE_EVENT_DELTA_PAGES_REQUEST_WIRE_BYTES, MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES,
    MAX_CORE_EVENT_DELTA_PAGE_ITEMS, MAX_CORE_EVENT_DELTA_PAGE_WIRE_BYTES,
    MAX_CORE_EVENT_STATE_PAGE_ITEMS, MAX_CORE_EVENT_STATE_PAGE_WIRE_BYTES,
    MAX_CORE_MATERIALIZER_REVISION_BYTES, MAX_CORE_SOURCE_DELTA_PAGE_ITEMS,
    MAX_CORE_SOURCE_DELTA_PAGE_WIRE_BYTES, MAX_CORE_SOURCE_STATES,
};
mod fake;
pub use fake::{FakeBlameFailure, FakeHelper};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ByteRange {
    pub start: u64,
    pub end_exclusive: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceCitation {
    pub core_generation_id: String,
    pub source: SourceKey,
    pub session_id: StableEntityId,
    pub event_id: StableEntityId,
    pub event_sequence: u64,
    pub byte_range: Option<ByteRange>,
    pub evidence_sha256: Option<String>,
}

impl EvidenceCitation {
    #[must_use]
    pub fn is_usable(&self) -> bool {
        use ctx_history_core::StableEntityKind;

        self.core_generation_id.len() == 64
            && is_lower_sha256(&self.core_generation_id)
            && self.source.validate_contract().is_ok()
            && self.session_id.validate_contract().is_ok()
            && self.event_id.validate_contract().is_ok()
            && self.session_id.entity_kind() == StableEntityKind::Session
            && self.event_id.entity_kind() == StableEntityKind::Event
            && self.event_id.source_digest() == self.source.identity().digest()
            && self.event_id.source_descriptor_digest() == self.source.exact_descriptor_digest()
            && self
                .byte_range
                .as_ref()
                .is_none_or(|range| range.start <= range.end_exclusive)
            && self.evidence_sha256.as_deref().is_none_or(is_lower_sha256)
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
