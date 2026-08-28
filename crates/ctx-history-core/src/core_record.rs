use std::io::{self, Write};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{SourceKey, StableEntityId, StableEntityKind, TypedKey};

mod activity;
mod validation;

pub use activity::{
    admit_optional_metadata_text, admit_optional_provider_call_id, admit_provider_declared_fact,
    ActivityInvocation, ActivityJsonCapture, ActivityResult, ActivityTextCapture, CoreActivity,
    LiteralFactKind, ProviderDeclaredFact, CORE_ACTIVITY_REVISION, MAX_PROVIDER_DECLARED_FACTS,
};
use validation::{
    validate_optional_text, validate_owned_identity, validate_related_session_identity,
    validate_size, validate_text,
};

pub const CORE_RECORD_VERSION: u32 = 3;
pub const CORE_NORMALIZATION_REVISION: u32 = 1;
pub const CORE_CONTENT_POLICY_REVISION: u32 = 4;
/// Relationship claims are optional provider-native facts in revision 3.
pub const CORE_RELATIONSHIP_CONTRACT_REVISION: u32 = 3;
/// Frozen domain for the exact canonical Core-record leaf algorithm.
pub const CORE_RECORD_LEAF_DOMAIN: &[u8] = b"ctx-core-record-leaf-v1\0";
/// Frozen identity of the per-source Core-record accumulator algorithm.
///
/// This identity is part of the Core record contract fingerprint so a change
/// to the accumulator cannot be interpreted under older generation semantics.
pub const CORE_RECORD_ACCUMULATOR_IDENTITY: &[u8] = b"ctx-core-record-event-binding-v1\0";

/// Maximum decoded size of policy-selected content admitted to one Core record.
pub const MAX_CORE_CONTENT_BYTES: usize = 16 * 1024 * 1024;
/// JSON escaping can expand content beyond its decoded size. This is a decode
/// and storage bound, not a preview or truncation policy.
pub const MAX_ENCODED_CORE_RECORD_BYTES: usize = 64 * 1024 * 1024;

const MAX_TEXT_METADATA_BYTES: usize = 64 * 1024;
pub type CoreRecordResult<T> = Result<T, CoreRecordError>;

/// Fingerprint of the versioned repository-neutral Core record contract.
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
    activity: u32,
    relationship: u32,
    accumulator_identity: &'static [u8],
}

impl CoreContractRevisions {
    const fn current() -> Self {
        Self {
            record: CORE_RECORD_VERSION,
            normalization: CORE_NORMALIZATION_REVISION,
            content_policy: CORE_CONTENT_POLICY_REVISION,
            activity: CORE_ACTIVITY_REVISION,
            relationship: CORE_RELATIONSHIP_CONTRACT_REVISION,
            accumulator_identity: CORE_RECORD_ACCUMULATOR_IDENTITY,
        }
    }
}

fn core_record_contract_fingerprint_for(revisions: CoreContractRevisions) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx.core-record-contract\0");
    digest.update(revisions.record.to_be_bytes());
    digest.update(revisions.normalization.to_be_bytes());
    digest.update(revisions.content_policy.to_be_bytes());
    digest.update(revisions.activity.to_be_bytes());
    digest.update(revisions.relationship.to_be_bytes());
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
    #[error("Core record provider-native session relationship claim is malformed")]
    InvalidSessionRelationship,
    #[error("Core record provider-native event copy claim is malformed or self-referential")]
    InvalidEventCopy,
    #[error("Core record content does not match its policy status")]
    InvalidContentPolicyState,
    #[error("Core record provider activity has an invalid shape or linkage")]
    InvalidActivity,
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
    pub structured_content: Option<serde_json::Value>,
    /// Optional positive proof that every body contribution is derived from a
    /// ctx history-retrieval invocation. Absence means unproven, not ordinary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_exclusion: Option<CoreDiscoveryExclusion>,
    /// Exact repository-neutral provider activity and literal source facts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<CoreActivity>,
}

/// Provider-neutral reason a complete Core record is excluded from ranked
/// discovery while remaining available to deterministic enumeration and show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreDiscoveryExclusion {
    CtxRetrievalDerived,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreContentPolicyStatus {
    Selected,
    Redacted { reason: String },
    Omitted { reason: String },
}

/// Exact provider-native proof admitted for a copied event claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderNativeCopyProof {
    NativeEventIdentity,
    NativeCopiedFromField,
    NativeCallResultIdentity,
}

/// Optional provider-native claim that this event copies one ancestor event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderNativeEventCopy {
    pub ancestor_session_id: StableEntityId,
    pub ancestor_event_id: StableEntityId,
    pub proof: ProviderNativeCopyProof,
}

/// Closed provider-native session relationship claim.
///
/// The claim is optional. Core never fills in `Root`, computes transitive
/// closure, or substitutes a value for an absent/unknown provider claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderNativeSessionRelationship {
    Root,
    Delegated,
    Forked,
    ResumedFrom,
    WorkflowChild,
}

impl ProviderNativeSessionRelationship {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Delegated => "delegated",
            Self::Forked => "forked",
            Self::ResumedFrom => "resumed_from",
            Self::WorkflowChild => "workflow_child",
        }
    }
}

/// Optional search-only agent scope explicitly declared by a provider.
/// Unknown scope is represented by absence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentScope {
    Primary,
    Subagent,
}

impl AgentScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Subagent => "subagent",
        }
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<StableEntityId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_session_id: Option<StableEntityId>,
    /// Closed provider-native relationship claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_relationship: Option<ProviderNativeSessionRelationship>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_copy: Option<ProviderNativeEventCopy>,
    pub source: SourceKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_event_id: Option<TypedKey>,
    pub event_sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_at_unix_ms: Option<i64>,
    pub event_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Search-only scope label, when one is explicitly stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_scope: Option<AgentScope>,
    pub parser_revision: String,
    pub normalization_revision: u32,
    pub content: CoreContent,
}

/// Provider-owned additions applied while constructing a complete Core record.
///
/// This shape deliberately has no generic metadata map. Exact source content
/// belongs in `structured_content`; admitted provider-declared literals belong
/// in the closed `CoreActivity::facts` vocabulary.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CoreRecordAnnotation {
    pub activity: Option<CoreActivity>,
    pub structured_content: Option<serde_json::Value>,
}

impl CoreRecord {
    /// Constructs the common policy-selected Core shape while keeping the
    /// provider parser revision explicit.
    #[allow(clippy::too_many_arguments)]
    pub fn new_selected(
        event_id: StableEntityId,
        session_id: StableEntityId,
        source: SourceKey,
        event_sequence: u64,
        event_type: impl Into<String>,
        parser_revision: impl Into<String>,
        normalized_body: impl Into<String>,
    ) -> CoreRecordResult<Self> {
        let record = Self {
            record_version: CORE_RECORD_VERSION,
            event_id,
            session_id,
            parent_session_id: None,
            root_session_id: None,
            session_relationship: None,
            event_copy: None,
            source,
            provider_session_id: None,
            native_event_id: None,
            event_sequence,
            occurred_at_unix_ms: None,
            event_type: event_type.into(),
            role: None,
            agent_scope: None,
            parser_revision: parser_revision.into(),
            normalization_revision: CORE_NORMALIZATION_REVISION,
            content: CoreContent {
                policy_revision: CORE_CONTENT_POLICY_REVISION,
                policy_status: CoreContentPolicyStatus::Selected,
                normalized_body: Some(normalized_body.into()),
                structured_content: None,
                discovery_exclusion: None,
                activity: None,
            },
        };
        record.validate_contract()?;
        Ok(record)
    }

    pub fn validate_contract(&self) -> CoreRecordResult<()> {
        self.validate_contract_and_content_bytes().map(|_| ())
    }

    /// Validates the complete Core contract and returns the exact encoded size
    /// of its policy-governed content without materializing a second payload.
    pub fn validate_contract_and_content_bytes(&self) -> CoreRecordResult<usize> {
        if self.record_version != CORE_RECORD_VERSION {
            return Err(CoreRecordError::UnsupportedVersion(self.record_version));
        }
        self.source
            .validate_contract()
            .map_err(|_| CoreRecordError::InvalidIdentityRelationship)?;
        validate_owned_identity(self.event_id, StableEntityKind::Event, &self.source)?;
        validate_owned_identity(self.session_id, StableEntityKind::Session, &self.source)?;
        self.validate_session_relationship_claims()?;
        self.validate_event_copy()?;
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
        validate_optional_text("role", self.role.as_deref(), MAX_TEXT_METADATA_BYTES)?;
        validate_text(
            "parser_revision",
            &self.parser_revision,
            MAX_TEXT_METADATA_BYTES,
        )?;
        if self.normalization_revision == 0 || self.content.policy_revision == 0 {
            return Err(CoreRecordError::InvalidContentPolicyState);
        }
        let content_bytes = self.content.validate_contract()?;
        Ok(content_bytes)
    }

    fn validate_session_relationship_claims(&self) -> CoreRecordResult<()> {
        if let Some(parent) = self.parent_session_id {
            validate_related_session_identity(parent)
                .map_err(|_| CoreRecordError::InvalidSessionRelationship)?;
            if parent == self.session_id {
                return Err(CoreRecordError::InvalidSessionRelationship);
            }
        }
        if let Some(root) = self.root_session_id {
            validate_related_session_identity(root)
                .map_err(|_| CoreRecordError::InvalidSessionRelationship)?;
        }
        Ok(())
    }

    fn validate_event_copy(&self) -> CoreRecordResult<()> {
        let Some(copy) = &self.event_copy else {
            return Ok(());
        };
        validate_related_session_identity(copy.ancestor_session_id)
            .map_err(|_| CoreRecordError::InvalidEventCopy)?;
        copy.ancestor_event_id
            .validate_contract()
            .map_err(|_| CoreRecordError::InvalidEventCopy)?;
        if copy.ancestor_event_id.entity_kind() != StableEntityKind::Event
            || copy.ancestor_session_id == self.session_id
            || copy.ancestor_event_id == self.event_id
        {
            return Err(CoreRecordError::InvalidEventCopy);
        }
        Ok(())
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

    /// Returns the exact canonical JSON size without materializing the encoded
    /// record. The storage and publication layers apply their own, narrower
    /// admission limits to this measurement.
    pub fn encoded_json_len(&self) -> CoreRecordResult<usize> {
        self.validate_contract()?;
        count_encoded_json_bytes(self)
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
}

impl CoreContent {
    /// Whether this complete record may contribute to ranked discovery.
    ///
    /// Enumeration and direct show APIs intentionally do not use this policy.
    pub const fn is_discovery_eligible(&self) -> bool {
        self.discovery_exclusion.is_none()
    }

    pub fn meaningful_text(&self) -> &str {
        self.normalized_body.as_deref().unwrap_or("")
    }

    pub fn encoded_content_bytes(&self) -> CoreRecordResult<usize> {
        Ok(self.encoded_content_byte_counts()?.total)
    }

    /// Omits trailing provider-declared facts when optional activity metadata
    /// alone would exceed the selected-content budget.
    pub fn omit_provider_declared_facts_if_aggregate_exceeds_limit(
        &mut self,
    ) -> CoreRecordResult<usize> {
        let mut excess = self
            .encoded_content_byte_counts()?
            .total
            .saturating_sub(MAX_CORE_CONTENT_BYTES);
        let mut omitted = 0;
        while excess > 0 {
            let Some(fact) = self
                .activity
                .as_mut()
                .and_then(|activity| activity.facts.pop())
            else {
                break;
            };
            excess = excess.saturating_sub(count_encoded_json_bytes(&fact)?);
            omitted += 1;
        }
        if self.activity.as_ref().is_some_and(|activity| {
            activity.invocation.is_none() && activity.result.is_none() && activity.facts.is_empty()
        }) {
            self.activity = None;
        }
        Ok(omitted)
    }

    /// Omits a projector-declared redundant structured representation when it
    /// is the only reason the aggregate selected content exceeds Core's limit.
    pub fn omit_structured_content_if_aggregate_exceeds_limit(&mut self) -> CoreRecordResult<bool> {
        if self.structured_content.is_none() {
            return Ok(false);
        }
        let counts = self.encoded_content_byte_counts()?;
        if counts.total <= MAX_CORE_CONTENT_BYTES {
            return Ok(false);
        }
        let retained_bytes = counts
            .total
            .checked_sub(counts.structured)
            .ok_or(CoreRecordError::EncodedLengthOverflow)?;
        if retained_bytes > MAX_CORE_CONTENT_BYTES {
            return Ok(false);
        }
        self.structured_content = None;
        Ok(true)
    }

    fn validate_contract(&self) -> CoreRecordResult<usize> {
        if self.policy_revision != CORE_CONTENT_POLICY_REVISION {
            return Err(CoreRecordError::InvalidContentPolicyState);
        }
        let counts = self.encoded_content_byte_counts()?;
        validate_size("normalized_body", counts.body, MAX_CORE_CONTENT_BYTES)?;
        validate_size(
            "structured_content",
            counts.structured,
            MAX_CORE_CONTENT_BYTES,
        )?;
        if let Some(activity) = &self.activity {
            if !matches!(self.policy_status, CoreContentPolicyStatus::Selected) {
                return Err(CoreRecordError::InvalidContentPolicyState);
            }
            activity.validate_contract(self.normalized_body.as_deref())?;
        }
        if self.discovery_exclusion.is_some()
            && !matches!(self.policy_status, CoreContentPolicyStatus::Selected)
        {
            return Err(CoreRecordError::InvalidContentPolicyState);
        }
        validate_size("selected_content", counts.total, MAX_CORE_CONTENT_BYTES)?;
        let has_selected_content = self
            .normalized_body
            .as_ref()
            .is_some_and(|body| !body.is_empty())
            || self.structured_content.is_some()
            || self.activity.is_some();
        match &self.policy_status {
            CoreContentPolicyStatus::Selected if !has_selected_content => {
                Err(CoreRecordError::InvalidContentPolicyState)
            }
            CoreContentPolicyStatus::Redacted { reason } => {
                validate_text("redaction_reason", reason, MAX_TEXT_METADATA_BYTES)?;
                if !has_selected_content {
                    return Err(CoreRecordError::InvalidContentPolicyState);
                }
                Ok(counts.total)
            }
            CoreContentPolicyStatus::Omitted { reason } => {
                validate_text("omission_reason", reason, MAX_TEXT_METADATA_BYTES)?;
                if self.normalized_body.is_some()
                    || self.structured_content.is_some()
                    || self.activity.is_some()
                {
                    return Err(CoreRecordError::InvalidContentPolicyState);
                }
                Ok(counts.total)
            }
            CoreContentPolicyStatus::Selected => Ok(counts.total),
        }
    }

    fn encoded_content_byte_counts(&self) -> CoreRecordResult<EncodedContentByteCounts> {
        let body = self.normalized_body.as_ref().map_or(0, String::len);
        let structured = self
            .structured_content
            .as_ref()
            .map(count_encoded_json_bytes)
            .transpose()?
            .unwrap_or(0);
        let activity = self
            .activity
            .as_ref()
            .map(count_encoded_json_bytes)
            .transpose()?
            .unwrap_or(0);
        let total = body
            .checked_add(structured)
            .and_then(|bytes| bytes.checked_add(activity))
            .ok_or(CoreRecordError::EncodedLengthOverflow)?;
        Ok(EncodedContentByteCounts {
            body,
            structured,
            total,
        })
    }
}

struct EncodedContentByteCounts {
    body: usize,
    structured: usize,
    total: usize,
}

#[derive(Default)]
struct EncodedJsonByteCounter {
    bytes: usize,
    overflowed: bool,
}

impl Write for EncodedJsonByteCounter {
    fn write(&mut self, encoded: &[u8]) -> io::Result<usize> {
        if let Some(bytes) = self.bytes.checked_add(encoded.len()) {
            self.bytes = bytes;
        } else {
            self.bytes = usize::MAX;
            self.overflowed = true;
        }
        Ok(encoded.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn count_encoded_json_bytes<T>(value: &T) -> CoreRecordResult<usize>
where
    T: Serialize + ?Sized,
{
    let mut counter = EncodedJsonByteCounter::default();
    serde_json::to_writer(&mut counter, value)?;
    if counter.overflowed {
        return Err(CoreRecordError::EncodedLengthOverflow);
    }
    Ok(counter.bytes)
}

#[cfg(test)]
mod tests;
