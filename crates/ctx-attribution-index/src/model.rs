use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use ctx_attribution_model::{
    CORE_REPOSITORY_CONTRACT_REVISION, CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION, ResourceKind,
};

mod citation;
mod resource;
#[cfg(test)]
mod tests;

pub use citation::ServingCitation;
pub use resource::{ServingResource, logical_repository_graph_id};

pub const MAX_IDENTIFIER_BYTES: usize = 4 * 1024;
/// Exact public Core bound for opaque logical repository identities.
pub const MAX_REPOSITORY_ID_BYTES: usize = 64 * 1024;
pub const MAX_FACT_FAMILY_BYTES: usize = 128;
pub const MAX_INDEX_TERM_BYTES: usize = 4 * 1024;
pub const MAX_INDEX_TERMS_PER_RECORD: usize = 32;
pub const MAX_CITATIONS_PER_RECORD: usize = 64;
pub const MAX_ATTRIBUTES_PER_RECORD: usize = 128;
pub const MAX_ATTRIBUTE_KEY_BYTES: usize = 256;
pub const MAX_CORE_IDENTITY_BYTES: usize = 64 * 1024;

pub const PREDICATE_ATTRIBUTE: &str = "_ctx.predicate";

pub const REPOSITORY_LIVE_ACCESS: &str = "_ctx.repository.live_access";
pub const GIT_REMOTE_ALIAS: &str = "git.remote.alias";
pub const FILE_TOUCHED: &str = "file.touched";
pub const FILE_MENTIONED: &str = "file.mentioned";
pub const GIT_COMMIT_PRODUCED: &str = "git.commit.produced";
pub const GIT_COMMIT_REPLACED: &str = "git.commit.replaced";
pub const GIT_COMMIT_REFERENCED: &str = "git.commit.referenced";
pub const GIT_COMMIT_INSPECTED: &str = "git.commit.inspected";
pub const PULL_REQUEST_PRODUCED: &str = "pull_request.produced";
pub const PULL_REQUEST_REFERENCED: &str = "pull_request.referenced";
pub const FORGE_PULL_REQUEST_PRODUCED: &str = "forge.pull_request.produced";
pub const FORGE_PULL_REQUEST_REFERENCED: &str = "forge.pull_request.referenced";

pub const GIT_COMMIT_AMBIGUOUS: &str = "git.commit.ambiguous";
pub const GIT_COMMIT_AMENDED: &str = "git.commit.amended";
pub const GIT_COMMIT_CHERRY_PICKED: &str = "git.commit.cherry_picked";
pub const GIT_COMMIT_REVERTED: &str = "git.commit.reverted";
pub const GIT_COMMIT_PUSHED: &str = "git.commit.pushed";
pub const FORGE_PULL_REQUEST_MERGED_AS: &str = "forge.pull_request.merged_as";
pub const FORGE_PULL_REQUEST_CONTAINS_COMMIT: &str = "forge.pull_request.contains_commit";
pub const FORGE_CREATE: &str = "forge.create";
pub const FORGE_REVIEW: &str = "forge.review";
pub const FORGE_COMMENT: &str = "forge.comment";
pub const FORGE_MERGE: &str = "forge.merge";
pub const FORGE_EDIT: &str = "forge.edit";
pub const FORGE_CLOSE: &str = "forge.close";
pub const FORGE_REOPEN: &str = "forge.reopen";

/// Complete fact closure consumed by the shipping blame service.
///
/// This is deliberately the union of the two SQL blame allowlists plus the
/// repository/file resolution, attribution, and commit-rewrite traversal facts
/// used around those lists.
pub const SCHEMA_CRITICAL_FACT_FAMILIES: [&str; 26] = [
    REPOSITORY_LIVE_ACCESS,
    GIT_REMOTE_ALIAS,
    FILE_TOUCHED,
    FILE_MENTIONED,
    GIT_COMMIT_PRODUCED,
    GIT_COMMIT_REPLACED,
    GIT_COMMIT_AMBIGUOUS,
    GIT_COMMIT_AMENDED,
    GIT_COMMIT_CHERRY_PICKED,
    GIT_COMMIT_REVERTED,
    GIT_COMMIT_PUSHED,
    GIT_COMMIT_REFERENCED,
    GIT_COMMIT_INSPECTED,
    FORGE_PULL_REQUEST_MERGED_AS,
    FORGE_PULL_REQUEST_CONTAINS_COMMIT,
    FORGE_PULL_REQUEST_REFERENCED,
    FORGE_CREATE,
    FORGE_REVIEW,
    FORGE_COMMENT,
    FORGE_MERGE,
    FORGE_EDIT,
    FORGE_CLOSE,
    FORGE_REOPEN,
    PULL_REQUEST_PRODUCED,
    PULL_REQUEST_REFERENCED,
    FORGE_PULL_REQUEST_PRODUCED,
];

#[must_use]
pub fn fact_type_may_confer_producer_authority(value: &str) -> bool {
    matches!(
        value,
        GIT_COMMIT_PRODUCED
            | GIT_COMMIT_REPLACED
            | GIT_COMMIT_AMBIGUOUS
            | GIT_COMMIT_CHERRY_PICKED
            | PULL_REQUEST_PRODUCED
            | FORGE_PULL_REQUEST_PRODUCED
            | FORGE_CREATE
            | FORGE_MERGE
    )
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ServingModelError {
    #[error("{field} is empty, unsafe, or exceeds its byte bound")]
    InvalidText { field: &'static str },
    #[error("repository identity is not a credential-free logical identity")]
    InvalidRepository,
    #[error("record resource repository scope does not match its serving scope")]
    RepositoryScopeMismatch,
    #[error("record line range is invalid")]
    InvalidLineRange,
    #[error("record origin is invalid")]
    InvalidOrigin,
    #[error("record digest is not a lowercase SHA-256 value")]
    InvalidDigest,
    #[error("record exceeds a collection bound")]
    CollectionBound,
    #[error("index terms must be strictly sorted and unique")]
    NonCanonicalIndexTerms,
    #[error("projected record is not indexed by its canonical record ID")]
    MissingRecordIdIndexTerm,
    #[error("asserted authority is not backed by direct verified evidence")]
    IneligibleAssertedAuthority,
    #[error("projected resource does not carry an exact typed identity")]
    InvalidResourceIdentity,
    #[error("projected citation is not an exact usable Core citation")]
    InvalidCoreCitation,
    #[error("answer record has no usable exact Core citation")]
    MissingCitation,
    #[error("projected record and its Core event owner disagree")]
    EventOwnerMismatch,
    #[error("citation chronology is not canonical")]
    NonCanonicalCitationChronology,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FactFamily(String);

impl FactFamily {
    pub fn new(value: impl Into<String>) -> Result<Self, ServingModelError> {
        let value = value.into();
        validate_token("fact family", &value, MAX_FACT_FAMILY_BYTES)?;
        if !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_')
        }) {
            return Err(ServingModelError::InvalidText {
                field: "fact family",
            });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn authority(&self) -> FactAuthority {
        match self.as_str() {
            REPOSITORY_LIVE_ACCESS => FactAuthority::AccessAuthorization,
            GIT_REMOTE_ALIAS => FactAuthority::Alias,
            FILE_TOUCHED => FactAuthority::DirectObservation,
            FILE_MENTIONED => FactAuthority::LowerAuthorityMention,
            GIT_COMMIT_INSPECTED => FactAuthority::Inspection,
            GIT_COMMIT_PRODUCED
            | GIT_COMMIT_REPLACED
            | PULL_REQUEST_PRODUCED
            | FORGE_PULL_REQUEST_PRODUCED
            | FORGE_CREATE
            | FORGE_MERGE => FactAuthority::OwnedOutcome,
            value if value.ends_with(".referenced") => FactAuthority::Reference,
            _ => FactAuthority::Unspecified,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FactAuthority {
    OwnedOutcome,
    DirectObservation,
    AccessAuthorization,
    Alias,
    Reference,
    Inspection,
    LowerAuthorityMention,
    Unspecified,
}

impl FactAuthority {
    #[must_use]
    pub const fn boundary(self) -> Option<AuthorityBoundary> {
        match self {
            Self::OwnedOutcome => Some(AuthorityBoundary::Ownership),
            Self::DirectObservation => Some(AuthorityBoundary::FileIdentity),
            Self::AccessAuthorization => Some(AuthorityBoundary::LiveRepositoryAccess),
            Self::Alias => Some(AuthorityBoundary::RepositoryAlias),
            Self::Reference
            | Self::Inspection
            | Self::LowerAuthorityMention
            | Self::Unspecified => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AuthorityBoundary {
    Ownership,
    FileIdentity,
    LiveRepositoryAccess,
    RepositoryAlias,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServingConfidence {
    Verified,
    High,
    Medium,
    Ambiguous,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServingFactState {
    Asserted,
    Ambiguous,
    Contradicted,
    Superseded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceRelationship {
    Supports,
    Contradicts,
    Context,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum AttributeValue {
    String(String),
    Integer(i64),
    Boolean(bool),
    Strings(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventOwner {
    pub source_id: String,
    pub event_id: String,
    pub direct_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_session_id: Option<String>,
    pub event_sequence: u64,
}

impl EventOwner {
    pub fn validate(&self) -> Result<(), ServingModelError> {
        validate_text("source id", &self.source_id, MAX_IDENTIFIER_BYTES)?;
        validate_text("event id", &self.event_id, MAX_IDENTIFIER_BYTES)?;
        validate_text(
            "direct session id",
            &self.direct_session_id,
            MAX_IDENTIFIER_BYTES,
        )?;
        if let Some(root_session_id) = &self.root_session_id {
            validate_text("root session id", root_session_id, MAX_IDENTIFIER_BYTES)?;
        }
        Ok(())
    }

    #[must_use]
    pub fn key(&self) -> EventOwnerKey {
        EventOwnerKey {
            source_id: self.source_id.clone(),
            event_id: self.event_id.clone(),
        }
    }

    #[must_use]
    pub fn tombstone(&self) -> EventTombstone {
        EventTombstone {
            source_id: self.source_id.clone(),
            event_id: self.event_id.clone(),
            event_sequence: self.event_sequence,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct EventOwnerKey {
    pub source_id: String,
    pub event_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ObservationOrigin {
    Direct,
    Copied {
        origin_event_id: String,
        origin_session_id: String,
    },
    Later {
        origin_event_sequence: u64,
    },
    Unspecified,
}

impl ObservationOrigin {
    #[must_use]
    pub const fn rank(&self) -> u8 {
        match self {
            Self::Direct => 0,
            Self::Copied { .. } => 1,
            Self::Later { .. } => 2,
            Self::Unspecified => 3,
        }
    }

    fn validate(&self) -> Result<(), ServingModelError> {
        match self {
            Self::Direct | Self::Later { .. } | Self::Unspecified => Ok(()),
            Self::Copied {
                origin_event_id,
                origin_session_id,
            } => {
                validate_text("origin event id", origin_event_id, MAX_IDENTIFIER_BYTES)?;
                validate_text("origin session id", origin_session_id, MAX_IDENTIFIER_BYTES)
                    .map_err(|_| ServingModelError::InvalidOrigin)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineRange {
    pub start_line: u32,
    pub end_line_inclusive: u32,
}

impl LineRange {
    fn validate(self) -> Result<(), ServingModelError> {
        if self.start_line == 0 || self.end_line_inclusive < self.start_line {
            Err(ServingModelError::InvalidLineRange)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServingRecord {
    /// Authoritative serialized Core logical fact identity.
    pub record_id: String,
    pub event_owner: EventOwner,
    pub repository_id: String,
    pub fact_family: FactFamily,
    pub subject: ServingResource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<ServingResource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<ServingResource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct_actor: Option<ServingResource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_at_unix_ms: Option<i64>,
    pub confidence: ServingConfidence,
    pub state: ServingFactState,
    pub detector_id: String,
    pub detector_revision: String,
    pub origin: ObservationOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_range: Option<LineRange>,
    pub index_terms: Vec<String>,
    pub attributes: BTreeMap<String, AttributeValue>,
    pub citations: Vec<ServingCitation>,
}

impl ServingRecord {
    pub fn validate(&self) -> Result<(), ServingModelError> {
        validate_text("record id", &self.record_id, MAX_IDENTIFIER_BYTES)?;
        self.event_owner.validate()?;
        validate_repository_id(&self.repository_id)?;
        FactFamily::new(self.fact_family.as_str())?;
        self.subject.validate(&self.repository_id)?;
        for resource in [
            self.object.as_ref(),
            self.scope.as_ref(),
            self.direct_actor.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            resource.validate(&self.repository_id)?;
        }
        validate_text("detector id", &self.detector_id, MAX_IDENTIFIER_BYTES)?;
        validate_text(
            "detector revision",
            &self.detector_revision,
            MAX_IDENTIFIER_BYTES,
        )?;
        self.origin.validate()?;
        if let Some(line_range) = self.line_range {
            line_range.validate()?;
        }
        if self.index_terms.is_empty()
            || self.index_terms.len() > MAX_INDEX_TERMS_PER_RECORD
            || self.citations.is_empty()
            || self.citations.len() > MAX_CITATIONS_PER_RECORD
            || self.attributes.len() > MAX_ATTRIBUTES_PER_RECORD
        {
            return Err(ServingModelError::CollectionBound);
        }
        for term in &self.index_terms {
            validate_text("index term", term, MAX_INDEX_TERM_BYTES)?;
        }
        if self
            .index_terms
            .windows(2)
            .any(|pair| pair[0].as_bytes() >= pair[1].as_bytes())
        {
            return Err(ServingModelError::NonCanonicalIndexTerms);
        }
        for (key, value) in &self.attributes {
            validate_text("attribute key", key, MAX_ATTRIBUTE_KEY_BYTES)?;
            validate_attribute(value)?;
        }
        for citation in &self.citations {
            citation.validate()?;
        }
        if self.state == ServingFactState::Asserted
            && self.fact_family.authority().boundary().is_some()
            && !self.grants_authority_for_family()
        {
            return Err(ServingModelError::IneligibleAssertedAuthority);
        }
        Ok(())
    }

    /// Enforces the stronger contract required for production projection.
    ///
    /// Flat's focused codec tests may use synthetic structural records, but a
    /// projected answer record must pass this method before it can be written.
    pub fn validate_projected(&self) -> Result<(), ServingModelError> {
        self.validate()?;
        for resource in [
            Some(&self.subject),
            self.object.as_ref(),
            self.scope.as_ref(),
            self.direct_actor.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            resource.validate_projected()?;
        }
        let predicate = self
            .attributes
            .get(PREDICATE_ATTRIBUTE)
            .and_then(|value| match value {
                AttributeValue::String(value) => Some(value.as_str()),
                _ => None,
            })
            .ok_or(ServingModelError::InvalidText {
                field: "fact predicate",
            })?;
        validate_text("fact predicate", predicate, MAX_IDENTIFIER_BYTES)?;
        if self
            .index_terms
            .binary_search_by(|term| term.as_bytes().cmp(self.record_id.as_bytes()))
            .is_err()
        {
            return Err(ServingModelError::MissingRecordIdIndexTerm);
        }

        let mut usable = 0_usize;
        for citation in &self.citations {
            citation.validate_exact_core()?;
            let exact = citation.exact_core_citation()?;
            if crate::core_source_storage_id(&exact.source) != self.event_owner.source_id
                || exact.event_id.to_string() != self.event_owner.event_id
                || exact.session_id.to_string() != self.event_owner.direct_session_id
                || exact.event_sequence != self.event_owner.event_sequence
            {
                return Err(ServingModelError::EventOwnerMismatch);
            }
            usable = usable.saturating_add(1);
        }
        if usable == 0 {
            return Err(ServingModelError::MissingCitation);
        }
        if self.citations.windows(2).any(|pair| {
            (
                pair[0].event_sequence,
                pair[0].event_id.as_str(),
                pair[0].citation_id.as_str(),
            ) > (
                pair[1].event_sequence,
                pair[1].event_id.as_str(),
                pair[1].citation_id.as_str(),
            )
        }) {
            return Err(ServingModelError::NonCanonicalCitationChronology);
        }
        Ok(())
    }

    #[must_use]
    pub fn grants_authority(&self, boundary: AuthorityBoundary) -> bool {
        authority_eligible(
            self.fact_family.authority(),
            boundary,
            &self.origin,
            self.state,
            self.confidence,
        ) || self.grants_verified_commit_operation_authority(boundary)
    }

    fn grants_verified_commit_operation_authority(&self, boundary: AuthorityBoundary) -> bool {
        matches!(
            self.fact_family.as_str(),
            GIT_COMMIT_PRODUCED | GIT_COMMIT_REPLACED | GIT_COMMIT_CHERRY_PICKED
        ) && boundary == AuthorityBoundary::Ownership
            && self.state == ServingFactState::Asserted
            && self.is_verified_later_repository_outcome()
            && self.subject.typed_kind().ok() == Some(ResourceKind::Commit)
            && self.verified_commit_operation_shape()
            && matches!(
                self.attributes.get("outcome_kind"),
                Some(AttributeValue::String(kind)) if kind == "commit"
            )
            && matches!(
                self.attributes.get("operation_id"),
                Some(AttributeValue::String(digest)) if is_lower_sha256(digest)
            )
            && matches!(
                self.attributes.get("receipt_id"),
                Some(AttributeValue::String(digest)) if is_lower_sha256(digest)
            )
            && matches!(
                self.attributes.get("proof_class"),
                Some(AttributeValue::String(proof)) if proof == "repository_verified"
            )
            && matches!(
                self.attributes.get("operation_state"),
                Some(AttributeValue::String(state)) if state == "asserted"
            )
            && self.commit_resources_match_declared_format()
    }

    fn is_verified_later_repository_outcome(&self) -> bool {
        let ObservationOrigin::Later {
            origin_event_sequence,
        } = self.origin
        else {
            return false;
        };
        self.confidence == ServingConfidence::Verified
            && self.detector_id == "core.repository"
            && self.detector_revision == CORE_REPOSITORY_CONTRACT_REVISION.to_string()
            && matches!(
                self.attributes.get("outcome_capture_revision"),
                Some(AttributeValue::String(revision))
                    if revision.parse::<u32>().ok()
                        == Some(CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION)
            )
            && matches!(
                self.attributes.get("origin_event_sequence"),
                Some(AttributeValue::Integer(sequence))
                    if u64::try_from(*sequence).ok() == Some(origin_event_sequence)
            )
            && matches!(
                self.attributes.get("result_record_sha256"),
                Some(AttributeValue::String(digest))
                    if digest.len() == 64
                        && digest.bytes().all(|byte| {
                            byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
                        })
            )
    }

    #[must_use]
    pub fn verified_commit_operation_id(&self) -> Option<&str> {
        if !self.grants_verified_commit_operation_authority(AuthorityBoundary::Ownership) {
            return None;
        }
        match self.attributes.get("operation_id") {
            Some(AttributeValue::String(operation_id)) => Some(operation_id),
            _ => None,
        }
    }

    fn verified_commit_operation_shape(&self) -> bool {
        let Some(AttributeValue::String(kind)) = self.attributes.get("operation_kind") else {
            return false;
        };
        let Some(AttributeValue::String(relation)) = self.attributes.get("relation_class") else {
            return false;
        };
        let Some(AttributeValue::String(predicate)) = self.attributes.get(PREDICATE_ATTRIBUTE)
        else {
            return false;
        };
        match self.fact_family.as_str() {
            GIT_COMMIT_PRODUCED => {
                predicate == "produced_by"
                    && self.object.as_ref().is_some_and(|object| {
                        object.typed_kind().ok() == Some(ResourceKind::Session)
                    })
                    && operation_kind_matches_relation(kind, relation)
            }
            GIT_COMMIT_REPLACED => {
                predicate == "replaces"
                    && self.object.as_ref().is_some_and(|object| {
                        object.typed_kind().ok() == Some(ResourceKind::Commit)
                    })
                    && matches!(kind.as_str(), "amend" | "rebase")
                    && relation == "replacement"
            }
            GIT_COMMIT_CHERRY_PICKED => {
                predicate == "cherry_picked_from"
                    && self.object.as_ref().is_some_and(|object| {
                        object.typed_kind().ok() == Some(ResourceKind::Commit)
                    })
                    && kind == "cherry_pick"
                    && relation == "derivation"
            }
            _ => false,
        }
    }

    fn commit_resources_match_declared_format(&self) -> bool {
        let Some(AttributeValue::String(format)) = self.attributes.get("object_format") else {
            return false;
        };
        if !commit_resource_matches_format(&self.subject, format) {
            return false;
        }
        match self.fact_family.as_str() {
            GIT_COMMIT_REPLACED | GIT_COMMIT_CHERRY_PICKED => self
                .object
                .as_ref()
                .is_some_and(|object| commit_resource_matches_format(object, format)),
            GIT_COMMIT_PRODUCED => true,
            _ => false,
        }
    }

    #[must_use]
    pub fn is_possible_commit_production_evidence(&self) -> bool {
        self.fact_family.as_str() == GIT_COMMIT_PRODUCED
            && self.state == ServingFactState::Ambiguous
            && self.is_verified_later_repository_outcome()
            && matches!(
                self.attributes.get("outcome_kind"),
                Some(AttributeValue::String(kind))
                    if kind == "commit"
            )
    }

    #[must_use]
    pub fn grants_file_identity_authority(&self) -> bool {
        self.grants_authority(AuthorityBoundary::FileIdentity)
    }

    #[must_use]
    pub fn grants_live_repository_access(&self) -> bool {
        self.grants_authority(AuthorityBoundary::LiveRepositoryAccess)
    }

    fn grants_authority_for_family(&self) -> bool {
        self.fact_family
            .authority()
            .boundary()
            .is_some_and(|boundary| self.grants_authority(boundary))
    }
}

fn operation_kind_matches_relation(kind: &str, relation: &str) -> bool {
    match kind {
        "amend" | "rebase" => relation == "replacement",
        "cherry_pick" => relation == "derivation",
        _ => false,
    }
}

fn commit_resource_matches_format(resource: &ServingResource, format: &str) -> bool {
    let expected = match format {
        "sha1" => 40,
        "sha256" => 64,
        _ => return false,
    };
    resource.display().is_ok_and(|oid| {
        oid.len() == expected
            && oid
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

#[must_use]
pub fn authority_eligible(
    authority: FactAuthority,
    boundary: AuthorityBoundary,
    origin: &ObservationOrigin,
    state: ServingFactState,
    confidence: ServingConfidence,
) -> bool {
    authority.boundary() == Some(boundary)
        && matches!(origin, ObservationOrigin::Direct)
        && matches!(state, ServingFactState::Asserted)
        && matches!(confidence, ServingConfidence::Verified)
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventTombstone {
    pub source_id: String,
    pub event_id: String,
    pub event_sequence: u64,
}

impl EventTombstone {
    pub fn validate(&self) -> Result<(), ServingModelError> {
        validate_text("tombstone source id", &self.source_id, MAX_IDENTIFIER_BYTES)?;
        validate_text("tombstone event id", &self.event_id, MAX_IDENTIFIER_BYTES)
    }

    #[must_use]
    pub fn key(&self) -> EventOwnerKey {
        EventOwnerKey {
            source_id: self.source_id.clone(),
            event_id: self.event_id.clone(),
        }
    }
}

fn validate_attribute(value: &AttributeValue) -> Result<(), ServingModelError> {
    match value {
        AttributeValue::String(value) => {
            validate_text("attribute value", value, MAX_IDENTIFIER_BYTES)
        }
        AttributeValue::Integer(_) | AttributeValue::Boolean(_) => Ok(()),
        AttributeValue::Strings(values) => {
            if values.len() > MAX_ATTRIBUTES_PER_RECORD {
                return Err(ServingModelError::CollectionBound);
            }
            for value in values {
                validate_text("attribute list value", value, MAX_IDENTIFIER_BYTES)?;
            }
            Ok(())
        }
    }
}

pub fn validate_repository_id(value: &str) -> Result<(), ServingModelError> {
    if value.is_empty() || value.len() > MAX_REPOSITORY_ID_BYTES {
        Err(ServingModelError::InvalidText {
            field: "repository id",
        })
    } else {
        Ok(())
    }
}

pub fn validate_token(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), ServingModelError> {
    validate_text(field, value, maximum)?;
    if value.bytes().any(|byte| byte.is_ascii_whitespace()) {
        Err(ServingModelError::InvalidText { field })
    } else {
        Ok(())
    }
}

pub fn validate_text(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), ServingModelError> {
    if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        Err(ServingModelError::InvalidText { field })
    } else {
        Ok(())
    }
}

#[must_use]
pub fn is_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}
