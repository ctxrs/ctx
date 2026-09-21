//! Canonical Blame requests, results, evidence and presentation data.
//!
//! This crate validates semantic data without opening history, repositories or indexes.

pub use ctx_history_core::{CoreRecord, SourceKey, StableEntityId};
use serde::{Deserialize, Serialize};

pub const MAX_BLAME_RESULTS: u32 = 100;
pub const MAX_BLAME_CURSOR_BYTES: usize = 256;
pub const MAX_BLAME_EVIDENCE: usize = 3_200;
pub const MAX_BLAME_ATTRIBUTIONS_PER_MATCH: usize = 100;
pub const MAX_BLAME_DIAGNOSTIC_CANDIDATES: usize = 5;
pub const MAX_CITATIONS_PER_FACT: usize = 32;
pub const MAX_BLAME_TARGET_BYTES: usize = 8 * 1024;
pub const MAX_COMMIT_LINEAGE_RETURNED_EVENTS: u32 = 100;
pub const MAX_COMMIT_LINEAGE_EXAMINED_EVENTS: u32 = 1_000;
/// Frozen per-operation mapping bound for repository commit lineage.
pub const MAX_REPOSITORY_COMMIT_OPERATION_MAPPINGS: usize = 32;
/// Frozen repository contract revision carried by Core materialization metadata.
pub const CORE_REPOSITORY_CONTRACT_REVISION: u32 = 11;
/// Frozen repository outcome-capture revision persisted with derived repository facts.
pub const CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION: u32 = 5;

mod error;
pub use error::{
    BlameDiagnosticCandidate, BlameDiagnosticDetails, BlameDiagnosticReason, ErrorClass,
    ProtocolError,
};
mod query;
pub use query::{
    AgentAttribution, BlameAttribution, BlameContinuation, BlameCoverage, BlameCoverageUnit,
    BlameMatch, BlameOutcome, BlameRequest, BlameResult, BlameTarget, CommitBlameMatch,
    CommitFactType, CommitPredicate, ContinuationReason, FactConfidence, FactState, FileBlameMatch,
    GitSnapshot, LineRange, NumberedEvidence, ProductionRelationship, PullRequestAction,
    PullRequestActivity, PullRequestBlameMatch, PullRequestBlameRelationship, PullRequestCommit,
    PullRequestCommitRelationship, QuerySnapshotExpectation, ResolvedBlameTarget, WorktreeStatus,
    canonical_logical_repository_id,
};
mod query_lineage;
pub use query_lineage::{
    CommitLineage, CommitLineageBounds, CommitLineageEdge, CommitLineageOmission,
    CommitLineageOperationKind, CommitLineageProofClass, CommitLineageRelationClass,
    CommitLineageState, CommitLineageTruncationReason, CommitLineageYield, ExactCommitRef,
    GitObjectFormat, ScopedCommitEndpoint,
};

mod generation;
pub use generation::{
    CORE_MATERIALIZATION_CONTRACT_VERSION, CoreGenerationHead, CoreMaterializationReceipt,
    CoreMaterializationReceiptIdentity, CoreRecordDigests, CoreSourceState,
    MAX_CORE_CONTROL_WIRE_BYTES, MAX_CORE_MATERIALIZER_REVISION_BYTES, MAX_CORE_SOURCE_STATES,
    core_record_digests, core_record_digests_from_encoded, core_record_leaf_sha256,
    core_record_sha256, core_source_snapshot_sha256,
};
mod coverage;
pub use coverage::{CoreProjectionCurrentness, MaterializedCoverage, RepositoryCoverage};
mod presentation;
pub use presentation::{BlameResultFreshness, HostedBlameResult};
pub mod diagnostic;
pub use diagnostic::{
    BlameDiagnostic, BlameDiagnosticFreshness, BlameFreshnessState, BlameNextAction,
    BlameNextActionKind,
};
pub mod evidence_preview;
pub use evidence_preview::{
    EvidencePreview, EvidencePreviewModel, MAX_EVIDENCE_PREVIEW_CITATIONS,
    MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES, RepositoryFileInvocationKind,
};

/// Persisted relationship between a session and its lineage parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRelationshipKind {
    Root,
    Delegated,
    Forked,
    ResumedFrom,
    WorkflowChild,
    RelatedUnknown,
}

impl SessionRelationshipKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Delegated => "delegated",
            Self::Forked => "forked",
            Self::ResumedFrom => "resumed_from",
            Self::WorkflowChild => "workflow_child",
            Self::RelatedUnknown => "related_unknown",
        }
    }

    #[must_use]
    pub const fn is_primary(self) -> bool {
        !matches!(self, Self::Delegated | Self::WorkflowChild)
    }
}

/// Exact structural proof admitted for a persisted copied-event edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventCopyProofKind {
    NativeEventIdentity,
    NativeCopiedFromField,
    NativeCallResultIdentity,
    CertifiedOrderedPrefix,
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
mod tests;
