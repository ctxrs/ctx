use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{SourceKey, StableEntityId, StableEntityKind, TypedKey};

mod repository;
mod validation;

pub use repository::{
    GitObjectFormat, GitObjectId, RepositoryAbstention, RepositoryAbstentionReason,
    RepositoryAlias, RepositoryAliasKind, RepositoryBinding, RepositoryCandidate,
    RepositoryCandidateEvidence, RepositoryCandidateKind, RepositoryEvidence,
    RepositoryEvidenceConfidence, RepositoryEvidenceKind, RepositoryFileInvocationEvidence,
    RepositoryFileInvocationKind, RepositoryFileInvocationTextRange, RepositoryFileObservation,
    RepositoryFileObservationKind, RepositoryLocalRootAuthorization, RepositoryObjectReplacement,
    RepositoryOutcomeKind, RepositoryOutcomeLinkage, RepositoryOutcomeObservation,
    RepositoryPullRequestAssociationObservation, RepositoryPullRequestIdentity,
    RepositoryVcsObservation, RepositoryVcsObservationKind,
};
use validation::{
    validate_count, validate_json_map, validate_optional_text, validate_owned_identity,
    validate_related_session_identity, validate_size, validate_text,
};

pub const CORE_RECORD_VERSION: u32 = 1;
pub const CORE_NORMALIZATION_REVISION: u32 = 1;
pub const CORE_CONTENT_POLICY_REVISION: u32 = 2;
pub const CORE_MCP_TOOL_CALL_ATTRIBUTION_REVISION: u32 = 1;
/// Frozen domain for the exact canonical Core-record leaf algorithm.
pub const CORE_RECORD_LEAF_DOMAIN: &[u8] = b"ctx-core-record-leaf-v1\0";
/// Frozen identity of the per-source Core-record accumulator algorithm.
///
/// This identity is part of the Core record contract fingerprint so a change
/// to the accumulator cannot be interpreted under older generation semantics.
pub const CORE_RECORD_ACCUMULATOR_IDENTITY: &[u8] = b"ctx-core-record-event-binding-v1\0";
pub const CORE_REPOSITORY_CONTRACT_REVISION: u32 = 8;
pub const CORE_REPOSITORY_OBSERVATION_REVISION: u32 = 3;
pub const CORE_BOUNDED_SHELL_SUBSET_REVISION: u32 = 2;
pub const CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION: u32 = 6;
pub const CORE_REPOSITORY_PULL_REQUEST_ASSOCIATION_CAPTURE_REVISION: u32 = 3;
pub const CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION: u32 = 3;
pub const CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION: u32 = 1;
pub const CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_DOMAIN: &[u8] =
    b"ctx.core.repository-local-root-fingerprint.v1\0";
const CORE_REPOSITORY_REUSE_INPUT_DOMAIN: &[u8] = b"ctx.core.repository-reuse-input.v1\0";
pub const CORE_MISSING_ACTIVITY_TIME_UNIX_MS: i64 = i64::MIN;

/// Maximum decoded size of either complete representation of policy-selected
/// content admitted to one Core record.
pub const MAX_CORE_CONTENT_BYTES: usize = 16 * 1024 * 1024;
/// JSON escaping can expand content beyond its decoded size. This is a decode
/// and storage bound, not a preview or truncation policy.
pub const MAX_ENCODED_CORE_RECORD_BYTES: usize = 64 * 1024 * 1024;
/// Maximum decoded UTF-8 size of each MCP tool-call attribution component.
pub const MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES: usize = 64 * 1024;

const MAX_TEXT_METADATA_BYTES: usize = 64 * 1024;
const MAX_METADATA_BYTES: usize = 1024 * 1024;
const MAX_REPOSITORY_ITEMS: usize = 256;
const MAX_REPOSITORY_OBSERVATIONS: usize = 4_096;
const MAX_REPOSITORY_ALIASES: usize = 64;
const MAX_REPOSITORY_EVIDENCE: usize = 64;
const MAX_REPOSITORY_NAMESPACE_PARTS: usize = 32;
const MAX_REPOSITORY_RELATIVE_PATH_BYTES: usize = 16 * 1024;
const MAX_GIT_REF_BYTES: usize = 4 * 1024;
const MAX_OUTCOME_LINKAGE_ITEMS: usize = 64;

pub type CoreRecordResult<T> = Result<T, CoreRecordError>;

/// Fingerprint of the versioned shared Core/repository contract.
///
/// Any logical shape or validation change must bump at least one bound
/// revision below, which changes both this value and generation identity.
pub fn core_record_contract_fingerprint() -> String {
    core_record_contract_fingerprint_for(CoreContractRevisions::current())
}

#[derive(Debug, Clone, Copy)]
struct CoreContractRevisions {
    record: u32,
    normalization: u32,
    content_policy: u32,
    mcp_tool_call_attribution: u32,
    accumulator_identity: &'static [u8],
    repository_contract: u32,
    repository_observation: u32,
    bounded_shell_subset: u32,
    repository_association_policy: u32,
    repository_pull_request_association_capture: u32,
    repository_outcome_capture: u32,
    repository_local_root_authorization_fingerprint: u32,
}

impl CoreContractRevisions {
    const fn current() -> Self {
        Self {
            record: CORE_RECORD_VERSION,
            normalization: CORE_NORMALIZATION_REVISION,
            content_policy: CORE_CONTENT_POLICY_REVISION,
            mcp_tool_call_attribution: CORE_MCP_TOOL_CALL_ATTRIBUTION_REVISION,
            accumulator_identity: CORE_RECORD_ACCUMULATOR_IDENTITY,
            repository_contract: CORE_REPOSITORY_CONTRACT_REVISION,
            repository_observation: CORE_REPOSITORY_OBSERVATION_REVISION,
            bounded_shell_subset: CORE_BOUNDED_SHELL_SUBSET_REVISION,
            repository_association_policy: CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
            repository_pull_request_association_capture:
                CORE_REPOSITORY_PULL_REQUEST_ASSOCIATION_CAPTURE_REVISION,
            repository_outcome_capture: CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
            repository_local_root_authorization_fingerprint:
                CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION,
        }
    }
}

fn core_record_contract_fingerprint_for(revisions: CoreContractRevisions) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx.core-record-contract\0");
    digest.update(revisions.record.to_be_bytes());
    digest.update(revisions.normalization.to_be_bytes());
    digest.update(revisions.content_policy.to_be_bytes());
    digest.update(revisions.mcp_tool_call_attribution.to_be_bytes());
    digest.update(revisions.repository_contract.to_be_bytes());
    digest.update(revisions.repository_observation.to_be_bytes());
    digest.update(revisions.bounded_shell_subset.to_be_bytes());
    digest.update(revisions.repository_association_policy.to_be_bytes());
    digest.update(
        revisions
            .repository_pull_request_association_capture
            .to_be_bytes(),
    );
    digest.update(revisions.repository_outcome_capture.to_be_bytes());
    digest.update(
        revisions
            .repository_local_root_authorization_fingerprint
            .to_be_bytes(),
    );
    digest.update(revisions.accumulator_identity);
    lowercase_sha256(&digest.finalize().into())
}

/// Computes the frozen leaf over an already-canonical stored Core record.
///
/// The exact input is `domain || canonical_event_id ||
/// u64_be(encoded_core_record.len) || encoded_core_record`.
pub fn core_record_leaf_digest(
    event_id: StableEntityId,
    encoded_core_record: &[u8],
) -> CoreRecordResult<[u8; 32]> {
    let canonical_event_id = event_id.encode_canonical()?;
    let encoded_len = u64::try_from(encoded_core_record.len())
        .map_err(|_| CoreRecordError::EncodedLengthOverflow)?;
    let mut digest = Sha256::new();
    digest.update(CORE_RECORD_LEAF_DOMAIN);
    digest.update(canonical_event_id);
    digest.update(encoded_len.to_be_bytes());
    digest.update(encoded_core_record);
    Ok(digest.finalize().into())
}

/// Computes the frozen per-record addend for a source accumulator.
///
/// The exact input is `accumulator_identity ||
/// u64_be(canonical_event_id.len) || canonical_event_id || core_record_leaf`.
pub fn core_record_accumulator_leaf_digest(
    event_id: StableEntityId,
    core_record_leaf: &[u8; 32],
) -> CoreRecordResult<[u8; 32]> {
    let canonical_event_id = event_id.encode_canonical()?;
    let encoded_len = u64::try_from(canonical_event_id.len())
        .map_err(|_| CoreRecordError::EncodedLengthOverflow)?;
    let mut digest = Sha256::new();
    digest.update(CORE_RECORD_ACCUMULATOR_IDENTITY);
    digest.update(encoded_len.to_be_bytes());
    digest.update(canonical_event_id);
    digest.update(core_record_leaf);
    Ok(digest.finalize().into())
}

/// Returns the lowercase leaf digest for one exact canonical `CoreRecord`.
pub fn core_record_leaf_sha256(record: &CoreRecord) -> CoreRecordResult<String> {
    let encoded = record.encode_stored()?;
    Ok(lowercase_sha256(&core_record_leaf_digest(
        record.event_id,
        &encoded,
    )?))
}

fn lowercase_sha256(digest: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let mut encoded = String::with_capacity(64);
    for byte in digest {
        encoded.push(HEX[usize::from(byte >> 4)] as char);
        encoded.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    encoded
}

#[derive(Debug, Error)]
pub enum CoreRecordError {
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Projection(#[from] crate::ProjectionContractError),
    #[error("encoded Core record length cannot be represented as u64")]
    EncodedLengthOverflow,
    #[error("unsupported Core record version {0}")]
    UnsupportedVersion(u32),
    #[error("Core record field {field} is empty")]
    EmptyField { field: &'static str },
    #[error("Core record field {field} is too large: {actual} bytes, maximum {maximum}")]
    FieldTooLarge {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("Core record collection {field} has too many items: {actual}, maximum {maximum}")]
    TooManyItems {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("Core record contains an invalid stable identity relationship")]
    InvalidIdentityRelationship,
    #[error("Core record content does not match its policy status")]
    InvalidContentPolicyState,
    #[error("Core record metadata must be a bounded JSON value")]
    InvalidMetadata,
    #[error("Core record repository identity {field} is duplicated: {value}")]
    DuplicateRepositoryIdentity { field: &'static str, value: String },
    #[error("Core record repository evidence names unknown binding {0}")]
    UnknownRepositoryBinding(String),
    #[error("repository path is not canonical repository-relative data: {0}")]
    InvalidRepositoryRelativePath(String),
    #[error("repository alias contains credential-bearing or non-canonical host data")]
    InvalidRepositoryAlias,
    #[error("Git object ID does not match its declared object format")]
    InvalidGitObjectId,
    #[error("Core record repository revisions do not match the active contract")]
    InvalidRepositoryRevisions,
    #[error("repository outcome does not match its declared operation or linkage")]
    InvalidRepositoryOutcome,
    #[error("repository candidate evidence is not strictly sorted and unique")]
    NonCanonicalRepositoryCandidateEvidence,
    #[error("repository file invocation evidence is not strictly sorted and unique")]
    NonCanonicalRepositoryFileInvocationEvidence,
    #[error("repository file invocation evidence has an invalid shape or text range")]
    InvalidRepositoryFileInvocationEvidence,
    #[error("pull-request association observation is not exact or locally certified")]
    InvalidRepositoryPullRequestAssociation,
}

/// Complete normalized content retained under one explicit product policy.
///
/// Presentation previews are derived from this value and are never durable
/// fields in Core.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreContent {
    pub policy_revision: u32,
    pub policy_status: CoreContentPolicyStatus,
    /// Complete normalized text representation of the selected event.
    pub normalized_body: Option<String>,
    /// Optional complete structured representation of the same selected event.
    /// Providers may intentionally repeat arguments present in
    /// `normalized_body`; admission charges the larger representation once.
    pub structured_content: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreContentPolicyStatus {
    Selected,
    Redacted { reason: String },
    Omitted { reason: String },
}

/// Provider-neutral attribution for a confirmed MCP tool call.
///
/// Provider adapters are responsible for deciding whether their native
/// evidence proves this shape. Core stores only the exact decoded components
/// and rejects malformed persisted values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpToolCallAttribution {
    pub server: String,
    pub tool: String,
}

/// One complete, generation-owned normalized history event.
///
/// Provider read-time locators are intentionally absent. `source` identifies
/// ownership and parser lineage; it is not an address for reading content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreRecord {
    pub record_version: u32,
    pub event_id: StableEntityId,
    pub session_id: StableEntityId,
    pub parent_session_id: Option<StableEntityId>,
    pub root_session_id: StableEntityId,
    pub source: SourceKey,
    pub provider_session_id: Option<String>,
    pub native_event_id: Option<TypedKey>,
    pub event_sequence: u64,
    pub occurred_at_unix_ms: Option<i64>,
    pub event_type: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_mcp_tool_call"
    )]
    /// Durable attribution is admitted only for policy-selected Core content.
    /// Presentation-time content suppression does not change this field.
    pub mcp_tool_call: Option<McpToolCallAttribution>,
    pub role: Option<String>,
    pub agent_type: String,
    pub is_primary: bool,
    pub workspace: Option<String>,
    pub branch: Option<String>,
    pub cwd: Option<String>,
    pub parser_revision: String,
    pub normalization_revision: u32,
    pub content: CoreContent,
    pub metadata: BTreeMap<String, serde_json::Value>,
    pub repository_candidate_evidence: RepositoryCandidateEvidence,
    pub repository_bindings: Vec<RepositoryBinding>,
    pub repository_abstentions: Vec<RepositoryAbstention>,
    /// Certified provider-native request intent, distinct from file effects.
    #[serde(default)]
    pub repository_file_invocation_evidence: Vec<RepositoryFileInvocationEvidence>,
    pub repository_file_observations: Vec<RepositoryFileObservation>,
    pub repository_vcs_observations: Vec<RepositoryVcsObservation>,
}

/// Provider-owned additions applied while constructing a complete Core record.
///
/// This keeps provider normalization and repository attribution out of the
/// index writer while sharing one bounded annotation shape across adapters.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CoreRecordAnnotation {
    pub mcp_tool_call: Option<McpToolCallAttribution>,
    pub structured_content: Option<serde_json::Value>,
    pub metadata: BTreeMap<String, serde_json::Value>,
    pub repository_candidate_evidence: RepositoryCandidateEvidence,
    pub repository_bindings: Vec<RepositoryBinding>,
    pub repository_abstentions: Vec<RepositoryAbstention>,
    pub repository_file_invocation_evidence: Vec<RepositoryFileInvocationEvidence>,
    pub repository_file_observations: Vec<RepositoryFileObservation>,
    pub repository_vcs_observations: Vec<RepositoryVcsObservation>,
}

#[derive(Serialize)]
struct RepositoryReuseInput<'a> {
    record_version: u32,
    event_id: StableEntityId,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    source: &'a SourceKey,
    provider_session_id: &'a Option<String>,
    native_event_id: &'a Option<TypedKey>,
    event_sequence: u64,
    occurred_at_unix_ms: Option<i64>,
    event_type: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    mcp_tool_call: &'a Option<McpToolCallAttribution>,
    role: &'a Option<String>,
    agent_type: &'a str,
    is_primary: bool,
    workspace: &'a Option<String>,
    branch: &'a Option<String>,
    cwd: &'a Option<String>,
    parser_revision: &'a str,
    normalization_revision: u32,
    content: &'a CoreContent,
    metadata: &'a BTreeMap<String, serde_json::Value>,
    repository_candidate_evidence: &'a RepositoryCandidateEvidence,
}

impl<'a> From<&'a CoreRecord> for RepositoryReuseInput<'a> {
    fn from(record: &'a CoreRecord) -> Self {
        Self {
            record_version: record.record_version,
            event_id: record.event_id,
            session_id: record.session_id,
            parent_session_id: record.parent_session_id,
            root_session_id: record.root_session_id,
            source: &record.source,
            provider_session_id: &record.provider_session_id,
            native_event_id: &record.native_event_id,
            event_sequence: record.event_sequence,
            occurred_at_unix_ms: record.occurred_at_unix_ms,
            event_type: &record.event_type,
            mcp_tool_call: &record.mcp_tool_call,
            role: &record.role,
            agent_type: &record.agent_type,
            is_primary: record.is_primary,
            workspace: &record.workspace,
            branch: &record.branch,
            cwd: &record.cwd,
            parser_revision: &record.parser_revision,
            normalization_revision: record.normalization_revision,
            content: &record.content,
            metadata: &record.metadata,
            repository_candidate_evidence: &record.repository_candidate_evidence,
        }
    }
}

impl CoreRecord {
    /// Constructs the common policy-selected Core shape while keeping the
    /// provider parser revision explicit.
    ///
    /// Provider adapters can set the public optional/native/repository fields
    /// they actually observed after construction. The generation writer
    /// validates the completed record again before indexing it.
    #[allow(clippy::too_many_arguments)]
    pub fn new_selected(
        event_id: StableEntityId,
        session_id: StableEntityId,
        root_session_id: StableEntityId,
        source: SourceKey,
        event_sequence: u64,
        event_type: impl Into<String>,
        agent_type: impl Into<String>,
        is_primary: bool,
        parser_revision: impl Into<String>,
        normalized_body: impl Into<String>,
    ) -> CoreRecordResult<Self> {
        let record = Self {
            record_version: CORE_RECORD_VERSION,
            event_id,
            session_id,
            parent_session_id: None,
            root_session_id,
            source,
            provider_session_id: None,
            native_event_id: None,
            event_sequence,
            occurred_at_unix_ms: None,
            event_type: event_type.into(),
            mcp_tool_call: None,
            role: None,
            agent_type: agent_type.into(),
            is_primary,
            workspace: None,
            branch: None,
            cwd: None,
            parser_revision: parser_revision.into(),
            normalization_revision: CORE_NORMALIZATION_REVISION,
            content: CoreContent {
                policy_revision: CORE_CONTENT_POLICY_REVISION,
                policy_status: CoreContentPolicyStatus::Selected,
                normalized_body: Some(normalized_body.into()),
                structured_content: None,
            },
            metadata: BTreeMap::new(),
            repository_candidate_evidence: RepositoryCandidateEvidence::default(),
            repository_bindings: Vec::new(),
            repository_abstentions: Vec::new(),
            repository_file_invocation_evidence: Vec::new(),
            repository_file_observations: Vec::new(),
            repository_vcs_observations: Vec::new(),
        };
        record.validate_contract()?;
        Ok(record)
    }

    pub fn validate_contract(&self) -> CoreRecordResult<()> {
        if self.record_version != CORE_RECORD_VERSION {
            return Err(CoreRecordError::UnsupportedVersion(self.record_version));
        }
        self.source
            .validate_contract()
            .map_err(|_| CoreRecordError::InvalidIdentityRelationship)?;
        validate_owned_identity(self.event_id, StableEntityKind::Event, &self.source)?;
        validate_owned_identity(self.session_id, StableEntityKind::Session, &self.source)?;
        validate_related_session_identity(self.root_session_id)?;
        if let Some(parent) = self.parent_session_id {
            validate_related_session_identity(parent)?;
        }
        validate_optional_text(
            "provider_session_id",
            self.provider_session_id.as_deref(),
            MAX_TEXT_METADATA_BYTES,
        )?;
        if let Some(native_event_id) = &self.native_event_id {
            native_event_id
                .validate_contract()
                .map_err(|_| CoreRecordError::InvalidIdentityRelationship)?;
        }
        validate_text("event_type", &self.event_type, MAX_TEXT_METADATA_BYTES)?;
        if let Some(attribution) = &self.mcp_tool_call {
            if !matches!(
                &self.content.policy_status,
                CoreContentPolicyStatus::Selected
            ) {
                return Err(CoreRecordError::InvalidContentPolicyState);
            }
            attribution.validate_contract()?;
        }
        validate_optional_text("role", self.role.as_deref(), MAX_TEXT_METADATA_BYTES)?;
        validate_text("agent_type", &self.agent_type, MAX_TEXT_METADATA_BYTES)?;
        validate_optional_text(
            "workspace",
            self.workspace.as_deref(),
            MAX_TEXT_METADATA_BYTES,
        )?;
        validate_optional_text("branch", self.branch.as_deref(), MAX_TEXT_METADATA_BYTES)?;
        validate_optional_text("cwd", self.cwd.as_deref(), MAX_TEXT_METADATA_BYTES)?;
        validate_text(
            "parser_revision",
            &self.parser_revision,
            MAX_TEXT_METADATA_BYTES,
        )?;
        if self.normalization_revision == 0 || self.content.policy_revision == 0 {
            return Err(CoreRecordError::InvalidContentPolicyState);
        }
        self.content.validate_contract()?;
        validate_json_map(&self.metadata)?;
        self.repository_candidate_evidence.validate_contract()?;
        self.validate_repositories()
    }

    pub fn encode_stored(&self) -> CoreRecordResult<Vec<u8>> {
        self.validate_contract()?;
        let encoded = serde_json::to_vec(self)?;
        validate_size(
            "encoded_core_record",
            encoded.len(),
            MAX_ENCODED_CORE_RECORD_BYTES,
        )?;
        Ok(encoded)
    }

    pub fn decode_stored(encoded: &[u8]) -> CoreRecordResult<Self> {
        validate_size(
            "encoded_core_record",
            encoded.len(),
            MAX_ENCODED_CORE_RECORD_BYTES,
        )?;
        let record: Self = serde_json::from_slice(encoded)?;
        record.validate_contract()?;
        Ok(record)
    }

    pub fn needs_prior_repository_certificate(&self) -> bool {
        self.repository_bindings.is_empty()
            && self.repository_abstentions.iter().any(|abstention| {
                abstention.reason == RepositoryAbstentionReason::CandidateMissingBeforeCertification
            })
    }

    /// Retains a previously certified logical identity when the same event is
    /// rebuilt after its local candidate disappeared. The old local route is
    /// deliberately revoked; only immutable identity and scoped observations
    /// survive.
    pub fn reuse_prior_repository_certificate(&mut self, prior: &Self) -> bool {
        let missing_before_certification = self.needs_prior_repository_certificate();
        let same_event = self.event_id == prior.event_id
            && self.session_id == prior.session_id
            && self.source.exact_descriptor_eq(&prior.source);
        let same_reuse_input = self
            .repository_reuse_input_fingerprint()
            .is_some_and(|current| {
                prior
                    .repository_reuse_input_fingerprint()
                    .is_some_and(|previous| current == previous)
            });
        if !missing_before_certification
            || !same_event
            || !same_reuse_input
            || prior.repository_bindings.is_empty()
        {
            return false;
        }

        self.repository_bindings = prior
            .repository_bindings
            .iter()
            .cloned()
            .map(|mut binding| {
                binding.local_root_authorization = None;
                binding
            })
            .collect();
        self.repository_file_invocation_evidence =
            prior.repository_file_invocation_evidence.clone();
        self.repository_file_observations = prior.repository_file_observations.clone();
        self.repository_vcs_observations = prior.repository_vcs_observations.clone();
        let evidence_kind = self
            .repository_abstentions
            .iter()
            .find(|abstention| {
                abstention.reason == RepositoryAbstentionReason::CandidateMissingBeforeCertification
            })
            .map(|abstention| abstention.evidence_kind)
            .unwrap_or(RepositoryEvidenceKind::SessionCwd);
        self.repository_abstentions.retain(|abstention| {
            abstention.reason != RepositoryAbstentionReason::CandidateMissingBeforeCertification
        });
        self.repository_abstentions.push(RepositoryAbstention {
            evidence_kind,
            reason: RepositoryAbstentionReason::Unavailable,
            detail: Some("prior_certificate_reused_without_local_authorization".to_owned()),
            association_policy_revision: CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
        });
        true
    }

    fn repository_reuse_input_fingerprint(&self) -> Option<[u8; 32]> {
        let encoded = serde_json::to_vec(&RepositoryReuseInput::from(self)).ok()?;
        let encoded_len = u64::try_from(encoded.len()).ok()?;
        let mut digest = Sha256::new();
        digest.update(CORE_REPOSITORY_REUSE_INPUT_DOMAIN);
        digest.update(CORE_REPOSITORY_CONTRACT_REVISION.to_be_bytes());
        digest.update(encoded_len.to_be_bytes());
        digest.update(encoded);
        Some(digest.finalize().into())
    }

    fn validate_repositories(&self) -> CoreRecordResult<()> {
        validate_count(
            "repository_bindings",
            self.repository_bindings.len(),
            MAX_REPOSITORY_ITEMS,
        )?;
        validate_count(
            "repository_abstentions",
            self.repository_abstentions.len(),
            MAX_REPOSITORY_ITEMS,
        )?;
        validate_count(
            "repository_file_invocation_evidence",
            self.repository_file_invocation_evidence.len(),
            MAX_REPOSITORY_OBSERVATIONS,
        )?;
        validate_count(
            "repository_file_observations",
            self.repository_file_observations.len(),
            MAX_REPOSITORY_OBSERVATIONS,
        )?;
        validate_count(
            "repository_vcs_observations",
            self.repository_vcs_observations.len(),
            MAX_REPOSITORY_OBSERVATIONS,
        )?;

        let mut binding_ids = HashSet::new();
        for binding in &self.repository_bindings {
            binding.validate_contract()?;
            if !binding_ids.insert(binding.binding_id.as_str()) {
                return Err(CoreRecordError::DuplicateRepositoryIdentity {
                    field: "repository_binding_id",
                    value: binding.binding_id.clone(),
                });
            }
        }
        for abstention in &self.repository_abstentions {
            abstention.validate_contract()?;
        }
        if self
            .repository_file_invocation_evidence
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        {
            return Err(CoreRecordError::NonCanonicalRepositoryFileInvocationEvidence);
        }
        for evidence in &self.repository_file_invocation_evidence {
            evidence.validate_contract(self.content.normalized_body.as_deref())?;
            if !binding_ids.contains(evidence.repository_binding_id.as_str()) {
                return Err(CoreRecordError::UnknownRepositoryBinding(
                    evidence.repository_binding_id.clone(),
                ));
            }
        }
        for observation in &self.repository_file_observations {
            observation.validate_contract()?;
            if !binding_ids.contains(observation.repository_binding_id.as_str()) {
                return Err(CoreRecordError::UnknownRepositoryBinding(
                    observation.repository_binding_id.clone(),
                ));
            }
        }
        for observation in &self.repository_vcs_observations {
            observation.validate_contract()?;
            let Some((binding, format)) = self
                .repository_bindings
                .iter()
                .find(|binding| binding.binding_id == observation.repository_binding_id)
                .map(|binding| (binding, binding.git_object_format))
            else {
                return Err(CoreRecordError::UnknownRepositoryBinding(
                    observation.repository_binding_id.clone(),
                ));
            };
            for object_id in observation
                .object_id
                .iter()
                .chain(observation.parent_object_ids.iter())
            {
                if format.is_none_or(|format| object_id.format != format) {
                    return Err(CoreRecordError::InvalidGitObjectId);
                }
            }
            match &observation.kind {
                RepositoryVcsObservationKind::Outcome(outcome) => {
                    if outcome
                        .pull_request
                        .as_ref()
                        .is_some_and(|pull_request| !binding.accepts_pull_request(pull_request))
                    {
                        return Err(CoreRecordError::InvalidRepositoryOutcome);
                    }
                    for object_id in outcome.object_ids() {
                        if format.is_none_or(|format| object_id.format != format) {
                            return Err(CoreRecordError::InvalidGitObjectId);
                        }
                    }
                }
                RepositoryVcsObservationKind::PullRequestAssociation(association)
                    if !binding.accepts_pull_request(&association.pull_request)
                        || format.is_none_or(|format| {
                            association.merged_as.format != format
                                || association
                                    .contains_commits
                                    .iter()
                                    .any(|object_id| object_id.format != format)
                        }) =>
                {
                    return Err(CoreRecordError::InvalidRepositoryPullRequestAssociation);
                }
                _ => {}
            }
        }
        Ok(())
    }
}

impl McpToolCallAttribution {
    pub fn validate_contract(&self) -> CoreRecordResult<()> {
        validate_text(
            "mcp_tool_call.server",
            &self.server,
            MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES,
        )?;
        validate_text(
            "mcp_tool_call.tool",
            &self.tool,
            MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES,
        )
    }
}

fn deserialize_present_mcp_tool_call<'de, D>(
    deserializer: D,
) -> Result<Option<McpToolCallAttribution>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    McpToolCallAttribution::deserialize(deserializer).map(Some)
}

impl CoreContent {
    pub fn meaningful_text(&self) -> &str {
        self.normalized_body.as_deref().unwrap_or("")
    }

    fn validate_contract(&self) -> CoreRecordResult<()> {
        if self.policy_revision == 0 {
            return Err(CoreRecordError::InvalidContentPolicyState);
        }
        let body_bytes = self.normalized_body.as_ref().map_or(0, String::len);
        validate_size("normalized_body", body_bytes, MAX_CORE_CONTENT_BYTES)?;
        let structured_bytes = self
            .structured_content
            .as_ref()
            .map(serde_json::to_vec)
            .transpose()?
            .map_or(0, |encoded| encoded.len());
        validate_size(
            "structured_content",
            structured_bytes,
            MAX_CORE_CONTENT_BYTES,
        )?;
        // These fields are alternate complete representations of one selected
        // event, not independent payloads. Charging the larger decoded form
        // keeps the per-record ceiling while avoiding double-counting content
        // that providers intentionally retain in both forms.
        let selected_content_bytes = body_bytes.max(structured_bytes);
        validate_size(
            "selected_content",
            selected_content_bytes,
            MAX_CORE_CONTENT_BYTES,
        )?;
        match &self.policy_status {
            CoreContentPolicyStatus::Selected => {
                if self.normalized_body.is_none() && self.structured_content.is_none()
                    || self.meaningful_text().is_empty()
                {
                    return Err(CoreRecordError::InvalidContentPolicyState);
                }
            }
            CoreContentPolicyStatus::Redacted { reason } => {
                validate_text("redaction_reason", reason, MAX_TEXT_METADATA_BYTES)?;
                if self.normalized_body.is_none() && self.structured_content.is_none()
                    || self.meaningful_text().is_empty()
                {
                    return Err(CoreRecordError::InvalidContentPolicyState);
                }
            }
            CoreContentPolicyStatus::Omitted { reason } => {
                validate_text("omission_reason", reason, MAX_TEXT_METADATA_BYTES)?;
                if self.normalized_body.is_some() || self.structured_content.is_some() {
                    return Err(CoreRecordError::InvalidContentPolicyState);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
