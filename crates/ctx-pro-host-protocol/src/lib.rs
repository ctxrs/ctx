//! Exact, bounded Protocol V1 between the OSS `ctx` host and a local Pro helper.
//!
//! The public crate is the only wire authority. Private implementations mirror its
//! generated inventory and fingerprint; they do not define a compatible range.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub use ctx_history_core::ContentRef;

pub const FRAME_MAGIC: &[u8; 6] = b"CTXPRO";
pub const PROTOCOL_VERSION: u16 = 1;
/// Lowercase SHA-256 of `testdata/v1/inventory.json`'s canonical inventory.
pub const PROTOCOL_FINGERPRINT: &str =
    "f9c77c0df491f276dd3d8c2cdb7f6c95daf8ebb9a216b2ca9a158ff0be1024c9";
pub const PROJECTION_CONTRACT_VERSION: u32 = 1;
pub const FRAME_HEADER_BYTES: usize = FRAME_MAGIC.len() + 2 + 4;
pub const MAX_FRAME_PAYLOAD_BYTES: usize = 10 * 1024 * 1024;
pub const MAX_QUERY_RESULTS: u32 = 500;
pub const MAX_QUERY_CURSOR_BYTES: usize = 4 * 1024;
pub const MAX_CITATIONS_PER_FACT: usize = 32;
pub const MAX_FACTS_PER_QUERY_RECORD: usize = 128;
pub const MAX_RESOURCE_SELECTOR_BYTES: usize = 8 * 1024;

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
    journal_sync_envelope_bytes, sha256_hex, JournalCheckpoint, JournalEntityKind,
    JournalEvidenceIdentity, JournalOperation, JournalPosition, JournalProvenanceIdentity,
    JournalRecord, JournalSyncMode, JournalSyncRequest, JournalSyncResult, ResultContentSidecar,
    MAX_AUTHORIZED_REPOSITORY_ROOTS, MAX_AUTHORIZED_REPOSITORY_ROOTS_TOTAL_BYTES,
    MAX_AUTHORIZED_REPOSITORY_ROOT_BYTES, MAX_JOURNAL_EVIDENCE_PER_RECORD,
    MAX_JOURNAL_IDENTITY_BYTES, MAX_JOURNAL_PAYLOAD_BYTES, MAX_JOURNAL_RECORDS_PER_BATCH,
    MAX_JOURNAL_SYNC_ENVELOPE_BYTES, MAX_RESULT_CONTENT_BYTES_PER_ITEM,
    MAX_RESULT_CONTENT_ITEMS_PER_REQUEST, MAX_RESULT_CONTENT_TOTAL_BYTES,
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
    HostMessage, StatusRequest, StatusResult,
};
mod query;
pub use query::{QueryRequest, QuerySnapshotExpectation};
mod fake;
pub use fake::{FakeHelper, FakeQueryFailure};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationKind {
    Event,
    FileTouch,
    VcsChange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ByteRange {
    pub start: u64,
    pub end_exclusive: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}

impl EvidenceCitation {
    #[must_use]
    pub fn is_usable(&self) -> bool {
        let coordinate_is_complete = self.observation_id.is_some()
            == (self.observation_seq.is_some() && self.observation_kind.is_some());
        let fields_are_valid = coordinate_is_complete
            && self.observation_id.is_none_or(|id| !id.is_nil())
            && self.session_id.is_none_or(|id| !id.is_nil())
            && self.event_id.is_none_or(|id| !id.is_nil())
            && self.source_path.as_deref().is_none_or(|path| {
                !path.trim().is_empty() && path.len() <= MAX_RESOURCE_SELECTOR_BYTES
            })
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryKind {
    Show,
    Locate,
    Blame,
    Timeline,
    Related,
    Facts,
}

impl QueryKind {
    pub const fn required_capability(self) -> Capability {
        Capability::Query
    }
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
pub struct ResourceSelector {
    pub kind: ResourceKind,
    pub value: String,
    pub repository: Option<String>,
    pub line: Option<u32>,
}

impl ResourceSelector {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.value.trim().is_empty() || self.value.len() > MAX_RESOURCE_SELECTOR_BYTES {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "resource selector value is empty or exceeds its byte bound",
            ));
        }
        if self.repository.as_deref().is_some_and(|repository| {
            repository.trim().is_empty() || repository.len() > MAX_RESOURCE_SELECTOR_BYTES
        }) {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "resource selector repository is empty or exceeds its byte bound",
            ));
        }
        if self.line == Some(0) {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "resource selector line must be positive",
            ));
        }
        if self.line.is_some() && self.kind != ResourceKind::File {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "resource selector line is valid only for file targets",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceRef {
    pub id: String,
    pub kind: ResourceKind,
    pub display: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QueryResult {
    pub records: Vec<QueryRecord>,
    pub next_cursor: Option<String>,
    pub truncated: bool,
    pub stale: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryResultWire {
    records: Vec<QueryRecord>,
    next_cursor: Option<String>,
    truncated: bool,
    stale: bool,
}

impl<'de> Deserialize<'de> for QueryResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = QueryResultWire::deserialize(deserializer)?;
        if wire.records.len() > MAX_QUERY_RESULTS as usize
            || wire.next_cursor.as_deref().is_some_and(|cursor| {
                cursor.is_empty() || cursor.len() > MAX_QUERY_CURSOR_BYTES || !cursor.is_ascii()
            })
        {
            return Err(serde::de::Error::custom(
                "query result exceeds Protocol V1 bounds",
            ));
        }
        Ok(Self {
            records: wire.records,
            next_cursor: wire.next_cursor,
            truncated: wire.truncated,
            stale: wire.stale,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QueryRecord {
    pub resource: ResourceRef,
    pub summary: Option<String>,
    pub occurred_at_ms: Option<i64>,
    pub facts: Vec<FactRecord>,
    pub citations: Vec<EvidenceCitation>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryRecordWire {
    resource: ResourceRef,
    summary: Option<String>,
    occurred_at_ms: Option<i64>,
    facts: Vec<FactRecord>,
    citations: Vec<EvidenceCitation>,
}

impl<'de> Deserialize<'de> for QueryRecord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = QueryRecordWire::deserialize(deserializer)?;
        let record = Self {
            resource: wire.resource,
            summary: wire.summary,
            occurred_at_ms: wire.occurred_at_ms,
            facts: wire.facts,
            citations: wire.citations,
        };
        record
            .validate()
            .map_err(|error| serde::de::Error::custom(error.message))?;
        Ok(record)
    }
}

impl QueryRecord {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.citations.len() > MAX_CITATIONS_PER_FACT
            || self.facts.len() > MAX_FACTS_PER_QUERY_RECORD
        {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "query record exceeds fact or citation bounds",
            ));
        }
        if self.citations.iter().any(|citation| !citation.is_usable()) {
            return Err(ProtocolError::new(
                ErrorClass::Corrupt,
                "query record has an unusable citation",
            ));
        }
        for fact in &self.facts {
            fact.validate()?;
        }
        if self.citations.is_empty() && self.facts.is_empty() {
            return Err(ProtocolError::new(
                ErrorClass::Corrupt,
                "query record has no cited evidence leaf",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactConfidence {
    Explicit,
    High,
    Medium,
    Low,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactState {
    Asserted,
    Ambiguous,
    Contradicted,
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum FactValue {
    Resource(ResourceRef),
    Text(String),
    Integer(i64),
    Boolean(bool),
    Json(Value),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FactRecord {
    pub id: String,
    pub fact_type: String,
    pub subject: ResourceRef,
    pub predicate: String,
    pub object: FactValue,
    pub confidence: FactConfidence,
    pub state: FactState,
    pub detector_version: String,
    pub owning_root_session_id: Option<Uuid>,
    pub direct_actor_session_id: Option<Uuid>,
    pub citations: Vec<EvidenceCitation>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FactRecordWire {
    id: String,
    fact_type: String,
    subject: ResourceRef,
    predicate: String,
    object: FactValue,
    confidence: FactConfidence,
    state: FactState,
    detector_version: String,
    owning_root_session_id: Option<Uuid>,
    direct_actor_session_id: Option<Uuid>,
    citations: Vec<EvidenceCitation>,
}

impl<'de> Deserialize<'de> for FactRecord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = FactRecordWire::deserialize(deserializer)?;
        let fact = Self {
            id: wire.id,
            fact_type: wire.fact_type,
            subject: wire.subject,
            predicate: wire.predicate,
            object: wire.object,
            confidence: wire.confidence,
            state: wire.state,
            detector_version: wire.detector_version,
            owning_root_session_id: wire.owning_root_session_id,
            direct_actor_session_id: wire.direct_actor_session_id,
            citations: wire.citations,
        };
        fact.validate()
            .map_err(|error| serde::de::Error::custom(error.message))?;
        Ok(fact)
    }
}

impl FactRecord {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.citations.is_empty() {
            return Err(ProtocolError::new(
                ErrorClass::Corrupt,
                "fact has no usable supporting citation",
            ));
        }
        if self.citations.len() > MAX_CITATIONS_PER_FACT {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "fact exceeds its citation bound",
            ));
        }
        if self.citations.iter().any(|citation| !citation.is_usable()) {
            return Err(ProtocolError::new(
                ErrorClass::Corrupt,
                "fact has an unusable supporting citation",
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
