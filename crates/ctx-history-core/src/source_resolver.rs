//! Source-backed content hydration contracts.
//!
//! Fresh ctx projections retain stable identity, metadata, and typed native
//! locators. Complete provider content remains in provider-owned sources.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::projection::{SourceKey, StableEntityId, StableEntityKind, TypedKey};

pub const NATIVE_LOCATOR_VERSION: u16 = 1;

const MAX_RELATION_BYTES: usize = 256;
const MAX_JSON_POINTER_BYTES: usize = 8 * 1024;
const MAX_LOCATORS_PER_REQUEST: usize = 100_000;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SourceResolverContractError {
    #[error("locator field {field} is empty")]
    EmptyField { field: &'static str },
    #[error("locator field {field} is too large: {actual}, maximum {maximum}")]
    FieldTooLarge {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("exact-revision locator is missing source revision evidence")]
    MissingSourceRevision,
    #[error("event identity does not belong to the locator source")]
    IdentitySourceMismatch,
    #[error("event identity has the wrong entity kind")]
    InvalidEventIdentity,
    #[error("hydration request has too many locators")]
    TooManyLocators,
    #[error("locator source identity is invalid")]
    InvalidSourceContract,
    #[error("unsupported native locator version {0}")]
    UnsupportedLocatorVersion(u16),
    #[error("hydration request identity is invalid")]
    InvalidIdentityContract,
}

pub type SourceResolverContractResult<T> = Result<T, SourceResolverContractError>;

/// Whether record evidence survives a benign append to the containing source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LocatorRevisionPolicy {
    /// The complete source revision must still equal the committed revision.
    ExactSourceRevision,
    /// The record's exact native coordinate and digest remain valid across an
    /// append, even though the containing source revision changed.
    StableRecordEvidence,
}

/// Path-independent provider-native coordinates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NativeRecordCoordinate {
    Jsonl {
        byte_offset: u64,
        byte_length: u64,
        physical_ordinal: u64,
        native_session_key: Option<TypedKey>,
        native_event_key: Option<TypedKey>,
    },
    ProviderSqlite {
        logical_relation: String,
        primary_key: TypedKey,
        row_version: Option<TypedKey>,
    },
    Document {
        object_key: TypedKey,
        json_pointer: Option<String>,
    },
    TreeRecord {
        relative_file_key: TypedKey,
        record_coordinate: TypedKey,
    },
    ProviderNative {
        namespace: String,
        coordinate: TypedKey,
    },
}

/// Locator and integrity evidence for one exact provider record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRecordLocator {
    locator_version: u16,
    source: SourceKey,
    coordinate: NativeRecordCoordinate,
    revision_policy: LocatorRevisionPolicy,
    certified_source_revision_digest: Option<[u8; 32]>,
    record_digest: [u8; 32],
}

impl SourceRecordLocator {
    pub fn new(
        source: SourceKey,
        coordinate: NativeRecordCoordinate,
        revision_policy: LocatorRevisionPolicy,
        certified_source_revision_digest: Option<[u8; 32]>,
        record_digest: [u8; 32],
    ) -> SourceResolverContractResult<Self> {
        let locator = Self {
            locator_version: NATIVE_LOCATOR_VERSION,
            source,
            coordinate,
            revision_policy,
            certified_source_revision_digest,
            record_digest,
        };
        locator.validate_contract()?;
        Ok(locator)
    }

    pub fn locator_version(&self) -> u16 {
        self.locator_version
    }

    pub fn source(&self) -> &SourceKey {
        &self.source
    }

    pub fn coordinate(&self) -> &NativeRecordCoordinate {
        &self.coordinate
    }

    pub fn revision_policy(&self) -> LocatorRevisionPolicy {
        self.revision_policy
    }

    pub fn certified_source_revision_digest(&self) -> Option<&[u8; 32]> {
        self.certified_source_revision_digest.as_ref()
    }

    pub fn record_digest(&self) -> &[u8; 32] {
        &self.record_digest
    }

    pub fn validate_contract(&self) -> SourceResolverContractResult<()> {
        if self.locator_version != NATIVE_LOCATOR_VERSION {
            return Err(SourceResolverContractError::UnsupportedLocatorVersion(
                self.locator_version,
            ));
        }
        self.source
            .validate_contract()
            .map_err(|_| SourceResolverContractError::InvalidSourceContract)?;
        validate_coordinate(&self.coordinate)?;
        if self.revision_policy == LocatorRevisionPolicy::ExactSourceRevision
            && self.certified_source_revision_digest.is_none()
        {
            return Err(SourceResolverContractError::MissingSourceRevision);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventHydrationRequest {
    event_id: StableEntityId,
    locator: SourceRecordLocator,
}

impl EventHydrationRequest {
    pub fn new(
        event_id: StableEntityId,
        locator: SourceRecordLocator,
    ) -> SourceResolverContractResult<Self> {
        event_id
            .validate_contract()
            .map_err(|_| SourceResolverContractError::InvalidIdentityContract)?;
        locator.validate_contract()?;
        if event_id.entity_kind() != StableEntityKind::Event {
            return Err(SourceResolverContractError::InvalidEventIdentity);
        }
        if event_id.source_digest() != locator.source.identity().digest()
            || event_id.source_descriptor_digest() != locator.source.exact_descriptor_digest()
        {
            return Err(SourceResolverContractError::IdentitySourceMismatch);
        }
        Ok(Self { event_id, locator })
    }

    pub fn event_id(&self) -> StableEntityId {
        self.event_id
    }

    pub fn locator(&self) -> &SourceRecordLocator {
        &self.locator
    }
}

/// Ordered locators are grouped by source by the resolver implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionHydrationRequest {
    session_id: StableEntityId,
    events: Vec<EventHydrationRequest>,
}

impl SessionHydrationRequest {
    pub fn new(
        session_id: StableEntityId,
        events: Vec<EventHydrationRequest>,
    ) -> SourceResolverContractResult<Self> {
        session_id
            .validate_contract()
            .map_err(|_| SourceResolverContractError::InvalidIdentityContract)?;
        if events.len() > MAX_LOCATORS_PER_REQUEST {
            return Err(SourceResolverContractError::TooManyLocators);
        }
        if session_id.entity_kind() != StableEntityKind::Session
            || events.iter().any(|event| {
                event.event_id.source_descriptor_digest()
                    != event.locator.source.exact_descriptor_digest()
                    || session_id.source_descriptor_digest()
                        != event.locator.source.exact_descriptor_digest()
            })
            || events
                .iter()
                .any(|event| event.event_id.source_digest() != session_id.source_digest())
        {
            return Err(SourceResolverContractError::IdentitySourceMismatch);
        }
        Ok(Self { session_id, events })
    }

    pub fn session_id(&self) -> StableEntityId {
        self.session_id
    }

    pub fn events(&self) -> &[EventHydrationRequest] {
        &self.events
    }
}

/// Exact provider bytes exist only for the duration of hydration/rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HydratedProviderRecord {
    pub event_id: StableEntityId,
    pub provider_bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HydrationFailureKind {
    TemporarilyUnavailable,
    ConfirmedDeleted,
    StaleSourceEvidence,
    StaleRecordEvidence,
    MissingRecord,
    UnsupportedParserRevision,
    InvalidLocator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HydrationFailure {
    pub kind: HydrationFailureKind,
    pub detail: String,
}

/// Implemented now only by the local provider-native resolver.
///
/// Future cloud, content-pack, or team resolvers may implement the same
/// boundary when those products define authority, privacy, and retention.
pub trait ContentSourceResolver {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure>;

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure>;
}

fn validate_coordinate(coordinate: &NativeRecordCoordinate) -> SourceResolverContractResult<()> {
    match coordinate {
        NativeRecordCoordinate::Jsonl { byte_length, .. } => {
            if *byte_length == 0 {
                return Err(SourceResolverContractError::EmptyField {
                    field: "jsonl_byte_length",
                });
            }
        }
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation, ..
        } => validate_text("logical_relation", logical_relation, MAX_RELATION_BYTES)?,
        NativeRecordCoordinate::Document { json_pointer, .. } => {
            if let Some(json_pointer) = json_pointer {
                validate_text("json_pointer", json_pointer, MAX_JSON_POINTER_BYTES)?;
            }
        }
        NativeRecordCoordinate::TreeRecord { .. } => {}
        NativeRecordCoordinate::ProviderNative { namespace, .. } => {
            validate_text("provider_namespace", namespace, MAX_RELATION_BYTES)?;
        }
    }
    Ok(())
}

fn validate_text(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> SourceResolverContractResult<()> {
    if value.is_empty() {
        return Err(SourceResolverContractError::EmptyField { field });
    }
    if value.len() > maximum {
        return Err(SourceResolverContractError::FieldTooLarge {
            field,
            actual: value.len(),
            maximum,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::projection::{
        derive_event_id, derive_session_id, EventIdentityInput, NativeItemKey, NativeSessionKey,
        SessionIdentityInput, SourceAnchor,
    };

    use super::*;

    fn source(lineage: u8) -> SourceKey {
        SourceKey::derive(
            "codex",
            "codex_session_jsonl",
            "session",
            1,
            SourceAnchor::CatalogLineage([lineage; 32]),
        )
        .unwrap()
    }

    fn event(source: &SourceKey) -> StableEntityId {
        let session_key =
            NativeSessionKey::native_id("session", TypedKey::utf8("session").unwrap()).unwrap();
        let session_id = derive_session_id(SessionIdentityInput {
            source,
            logical_session_kind: "thread",
            native_session_key: &session_key,
        })
        .unwrap();
        let item = NativeItemKey::native_id("message", TypedKey::utf8("event").unwrap()).unwrap();
        derive_event_id(EventIdentityInput {
            source,
            session_id,
            logical_item_kind: "message",
            native_item_key: &item,
            subrecord_selector: None,
        })
        .unwrap()
    }

    fn locator(source: SourceKey) -> SourceRecordLocator {
        SourceRecordLocator::new(
            source,
            NativeRecordCoordinate::Jsonl {
                byte_offset: 100,
                byte_length: 50,
                physical_ordinal: 4,
                native_session_key: None,
                native_event_key: None,
            },
            LocatorRevisionPolicy::StableRecordEvidence,
            None,
            [8; 32],
        )
        .unwrap()
    }

    #[test]
    fn append_stable_jsonl_locator_does_not_require_whole_source_revision() {
        let source = source(1);
        let locator = locator(source);
        assert_eq!(
            locator.revision_policy(),
            LocatorRevisionPolicy::StableRecordEvidence
        );
        assert!(locator.certified_source_revision_digest().is_none());
    }

    #[test]
    fn exact_revision_locator_requires_revision_evidence() {
        let source = source(1);
        let error = SourceRecordLocator::new(
            source,
            NativeRecordCoordinate::Document {
                object_key: TypedKey::utf8("object").unwrap(),
                json_pointer: Some("/messages/0".to_owned()),
            },
            LocatorRevisionPolicy::ExactSourceRevision,
            None,
            [8; 32],
        )
        .unwrap_err();
        assert_eq!(error, SourceResolverContractError::MissingSourceRevision);
    }

    #[test]
    fn event_and_locator_must_share_source_lineage() {
        let error = EventHydrationRequest::new(event(&source(1)), locator(source(2))).unwrap_err();
        assert_eq!(error, SourceResolverContractError::IdentitySourceMismatch);
    }
}
