//! Source-backed content hydration contracts.
//!
//! Fresh ctx projections retain stable identity, metadata, and typed native
//! locators. Complete provider content remains in provider-owned sources.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::projection::{SourceKey, StableEntityId, StableEntityKind, TypedKey};

pub const NATIVE_LOCATOR_VERSION: u16 = 1;
/// Shared admission ceiling for ordered event batches and complete sessions.
///
/// This preserves the existing large-session ceiling while comfortably
/// covering the initial 200-result search hydration path.
pub const MAX_BATCH_HYDRATION_EVENTS: usize = 100_000;

const MAX_RELATION_BYTES: usize = 256;
const MAX_JSON_POINTER_BYTES: usize = 8 * 1024;

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
    #[error("JSONL locator byte range overflowed")]
    InvalidJsonlByteRange,
    #[error("event identity does not belong to the locator source")]
    IdentitySourceMismatch,
    #[error("event identity has the wrong entity kind")]
    InvalidEventIdentity,
    #[error("hydration request has too many locators")]
    TooManyLocators,
    #[error("hydration request repeats an event identity")]
    DuplicateEventIdentity,
    #[error("hydration result has too many records")]
    TooManyHydratedRecords,
    #[error("locator source identity is invalid")]
    InvalidSourceContract,
    #[error("unsupported native locator version {0}")]
    UnsupportedLocatorVersion(u16),
    #[error("hydration request identity is invalid")]
    InvalidIdentityContract,
}

pub type SourceResolverContractResult<T> = Result<T, SourceResolverContractError>;

/// Whether record evidence survives a benign append to the containing source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LocatorRevisionPolicy {
    /// The complete source revision must still equal the committed revision.
    ExactSourceRevision,
    /// The record's exact native coordinate and digest remain valid across an
    /// append, even though the containing source revision changed.
    StableRecordEvidence,
}

/// Path-independent provider-native coordinates.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
        let request = Self { event_id, locator };
        request.validate_contract()?;
        Ok(request)
    }

    pub fn event_id(&self) -> StableEntityId {
        self.event_id
    }

    pub fn locator(&self) -> &SourceRecordLocator {
        &self.locator
    }

    pub fn validate_contract(&self) -> SourceResolverContractResult<()> {
        self.event_id
            .validate_contract()
            .map_err(|_| SourceResolverContractError::InvalidIdentityContract)?;
        self.locator.validate_contract()?;
        if self.event_id.entity_kind() != StableEntityKind::Event {
            return Err(SourceResolverContractError::InvalidEventIdentity);
        }
        if self.event_id.source_digest() != self.locator.source.identity().digest()
            || self.event_id.source_descriptor_digest()
                != self.locator.source.exact_descriptor_digest()
        {
            return Err(SourceResolverContractError::IdentitySourceMismatch);
        }
        Ok(())
    }
}

/// One bounded, provider-neutral event batch in caller-requested order.
///
/// Events may belong to different sessions, providers, and exact sources.
/// Duplicate event identities are rejected so every result has one
/// unambiguous output position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchHydrationRequest {
    events: Vec<EventHydrationRequest>,
}

impl BatchHydrationRequest {
    pub fn new(events: Vec<EventHydrationRequest>) -> SourceResolverContractResult<Self> {
        if events.len() > MAX_BATCH_HYDRATION_EVENTS {
            return Err(SourceResolverContractError::TooManyLocators);
        }
        let mut event_ids = HashSet::with_capacity(events.len());
        for event in &events {
            event.validate_contract()?;
            if !event_ids.insert(event.event_id()) {
                return Err(SourceResolverContractError::DuplicateEventIdentity);
            }
        }
        Ok(Self { events })
    }

    pub fn events(&self) -> &[EventHydrationRequest] {
        &self.events
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// Ordered locators for one logical session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionHydrationRequest {
    session_id: StableEntityId,
    batch: BatchHydrationRequest,
}

impl SessionHydrationRequest {
    pub fn new(
        session_id: StableEntityId,
        events: Vec<EventHydrationRequest>,
    ) -> SourceResolverContractResult<Self> {
        if events.len() > MAX_BATCH_HYDRATION_EVENTS {
            return Err(SourceResolverContractError::TooManyLocators);
        }
        session_id
            .validate_contract()
            .map_err(|_| SourceResolverContractError::InvalidIdentityContract)?;
        let batch = BatchHydrationRequest::new(events)?;
        if session_id.entity_kind() != StableEntityKind::Session
            || batch.events().iter().any(|event| {
                event.event_id.source_descriptor_digest()
                    != event.locator.source.exact_descriptor_digest()
                    || session_id.source_descriptor_digest()
                        != event.locator.source.exact_descriptor_digest()
            })
            || batch
                .events()
                .iter()
                .any(|event| event.event_id.source_digest() != session_id.source_digest())
        {
            return Err(SourceResolverContractError::IdentitySourceMismatch);
        }
        Ok(Self { session_id, batch })
    }

    pub fn session_id(&self) -> StableEntityId {
        self.session_id
    }

    pub fn events(&self) -> &[EventHydrationRequest] {
        self.batch.events()
    }

    pub fn batch(&self) -> &BatchHydrationRequest {
        &self.batch
    }
}

/// Exact provider bytes exist only for the duration of hydration/rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HydratedProviderRecord {
    pub event_id: StableEntityId,
    pub provider_bytes: Vec<u8>,
}

/// Complete batch output in the exact order of its corresponding request.
///
/// Construction enforces the allocation ceiling. Resolvers must additionally
/// call [`Self::validate_for_request`] before returning provider output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchHydrationResult {
    records: Vec<HydratedProviderRecord>,
}

impl BatchHydrationResult {
    pub fn new(
        records: Vec<HydratedProviderRecord>,
    ) -> SourceResolverContractResult<BatchHydrationResult> {
        if records.len() > MAX_BATCH_HYDRATION_EVENTS {
            return Err(SourceResolverContractError::TooManyHydratedRecords);
        }
        Ok(Self { records })
    }

    pub fn records(&self) -> &[HydratedProviderRecord] {
        &self.records
    }

    pub fn into_records(self) -> Vec<HydratedProviderRecord> {
        self.records
    }

    pub fn validate_for_request(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<(), HydrationFailure> {
        if self.records.len() != request.len() {
            return Err(invalid_batch_result(format!(
                "batch hydration returned {} records for {} requested events",
                self.records.len(),
                request.len()
            )));
        }

        let expected = request
            .events()
            .iter()
            .map(EventHydrationRequest::event_id)
            .collect::<HashSet<_>>();
        let mut observed = HashSet::with_capacity(self.records.len());
        for record in &self.records {
            if !observed.insert(record.event_id) {
                return Err(invalid_batch_result(
                    "batch hydration returned a duplicate event identity",
                ));
            }
            if !expected.contains(&record.event_id) {
                return Err(invalid_batch_result(
                    "batch hydration returned an unrequested event identity",
                ));
            }
        }

        if request
            .events()
            .iter()
            .zip(&self.records)
            .any(|(event, record)| event.event_id() != record.event_id)
        {
            return Err(invalid_batch_result(
                "batch hydration records are not in exact request order",
            ));
        }
        Ok(())
    }
}

/// Stable coarse classes shared by every source-backed error boundary.
///
/// Classes intentionally contain no provider detail, source path, or record
/// content. Precise causes remain available through [`HydrationFailureKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceBackedErrorClass {
    Unavailable,
    ConfirmedDeleted,
    StaleEvidence,
    Malformed,
    Unsupported,
    InvalidRequest,
    Internal,
}

impl SourceBackedErrorClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::ConfirmedDeleted => "confirmed_deleted",
            Self::StaleEvidence => "stale_evidence",
            Self::Malformed => "malformed",
            Self::Unsupported => "unsupported",
            Self::InvalidRequest => "invalid_request",
            Self::Internal => "internal",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "unavailable" => Some(Self::Unavailable),
            "confirmed_deleted" => Some(Self::ConfirmedDeleted),
            "stale_evidence" => Some(Self::StaleEvidence),
            "malformed" => Some(Self::Malformed),
            "unsupported" => Some(Self::Unsupported),
            "invalid_request" => Some(Self::InvalidRequest),
            "internal" => Some(Self::Internal),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HydrationFailureKind {
    TemporarilyUnavailable,
    ConfirmedDeleted,
    StaleSourceEvidence,
    StaleRecordEvidence,
    MissingRecord,
    MalformedSource,
    UnsupportedParserRevision,
    InvalidLocator,
    ContentTooLarge,
    InvalidRequest,
    Internal,
}

impl HydrationFailureKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TemporarilyUnavailable => "temporarily_unavailable",
            Self::ConfirmedDeleted => "confirmed_deleted",
            Self::StaleSourceEvidence => "stale_source_evidence",
            Self::StaleRecordEvidence => "stale_record_evidence",
            Self::MissingRecord => "missing_record",
            Self::MalformedSource => "malformed_source",
            Self::UnsupportedParserRevision => "unsupported_parser_revision",
            Self::InvalidLocator => "invalid_locator",
            Self::ContentTooLarge => "content_too_large",
            Self::InvalidRequest => "invalid_request",
            Self::Internal => "internal",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "temporarily_unavailable" => Some(Self::TemporarilyUnavailable),
            "confirmed_deleted" => Some(Self::ConfirmedDeleted),
            "stale_source_evidence" => Some(Self::StaleSourceEvidence),
            "stale_record_evidence" => Some(Self::StaleRecordEvidence),
            "missing_record" => Some(Self::MissingRecord),
            "malformed_source" => Some(Self::MalformedSource),
            "unsupported_parser_revision" => Some(Self::UnsupportedParserRevision),
            "invalid_locator" => Some(Self::InvalidLocator),
            "content_too_large" => Some(Self::ContentTooLarge),
            "invalid_request" => Some(Self::InvalidRequest),
            "internal" => Some(Self::Internal),
            _ => None,
        }
    }

    pub const fn class(self) -> SourceBackedErrorClass {
        match self {
            Self::TemporarilyUnavailable => SourceBackedErrorClass::Unavailable,
            Self::ConfirmedDeleted => SourceBackedErrorClass::ConfirmedDeleted,
            Self::StaleSourceEvidence | Self::StaleRecordEvidence | Self::MissingRecord => {
                SourceBackedErrorClass::StaleEvidence
            }
            Self::MalformedSource => SourceBackedErrorClass::Malformed,
            Self::UnsupportedParserRevision => SourceBackedErrorClass::Unsupported,
            Self::InvalidLocator | Self::ContentTooLarge | Self::InvalidRequest => {
                SourceBackedErrorClass::InvalidRequest
            }
            Self::Internal => SourceBackedErrorClass::Internal,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HydrationFailure {
    pub kind: HydrationFailureKind,
    pub detail: String,
}

impl HydrationFailure {
    pub fn new(kind: HydrationFailureKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
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

    fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        let records = request
            .events()
            .iter()
            .map(|event| self.hydrate_event(event))
            .collect::<Result<Vec<_>, _>>()?;
        let result = BatchHydrationResult::new(records).map_err(contract_hydration_failure)?;
        result.validate_for_request(request)?;
        Ok(result)
    }

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        self.hydrate_batch(request.batch())
            .map(BatchHydrationResult::into_records)
    }
}

fn contract_hydration_failure(error: SourceResolverContractError) -> HydrationFailure {
    provider_contract_failure(format!("invalid batch hydration contract: {error}"))
}

fn invalid_batch_result(detail: impl Into<String>) -> HydrationFailure {
    provider_contract_failure(detail)
}

fn provider_contract_failure(detail: impl Into<String>) -> HydrationFailure {
    HydrationFailure::new(HydrationFailureKind::Internal, detail)
}

fn validate_coordinate(coordinate: &NativeRecordCoordinate) -> SourceResolverContractResult<()> {
    match coordinate {
        NativeRecordCoordinate::Jsonl {
            byte_offset,
            byte_length,
            ..
        } => {
            if *byte_length == 0 {
                return Err(SourceResolverContractError::EmptyField {
                    field: "jsonl_byte_length",
                });
            }
            byte_offset
                .checked_add(*byte_length)
                .ok_or(SourceResolverContractError::InvalidJsonlByteRange)?;
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
mod error_contract_tests;

#[cfg(test)]
mod tests {
    use std::cell::Cell;

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

    fn session(source: &SourceKey) -> StableEntityId {
        let session_key =
            NativeSessionKey::native_id("session", TypedKey::utf8("session").unwrap()).unwrap();
        derive_session_id(SessionIdentityInput {
            source,
            logical_session_kind: "thread",
            native_session_key: &session_key,
        })
        .unwrap()
    }

    fn event(source: &SourceKey) -> StableEntityId {
        event_named(source, "event")
    }

    fn event_named(source: &SourceKey, native_id: &str) -> StableEntityId {
        let item = NativeItemKey::native_id("message", TypedKey::utf8(native_id).unwrap()).unwrap();
        derive_event_id(EventIdentityInput {
            source,
            session_id: session(source),
            logical_item_kind: "message",
            native_item_key: &item,
            subrecord_selector: None,
        })
        .unwrap()
    }

    fn request(source: &SourceKey, native_id: &str) -> EventHydrationRequest {
        EventHydrationRequest::new(event_named(source, native_id), locator(source.clone())).unwrap()
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

    #[test]
    fn ordered_batch_is_bounded_and_rejects_duplicate_event_identities() {
        const {
            assert!(MAX_BATCH_HYDRATION_EVENTS == 100_000);
            assert!(MAX_BATCH_HYDRATION_EVENTS >= 200);
        }

        let source = source(1);
        let event = request(&source, "event");
        let duplicate = BatchHydrationRequest::new(vec![event.clone(), event.clone()]).unwrap_err();
        assert_eq!(
            duplicate,
            SourceResolverContractError::DuplicateEventIdentity
        );

        let oversized =
            BatchHydrationRequest::new(vec![event.clone(); MAX_BATCH_HYDRATION_EVENTS + 1])
                .unwrap_err();
        assert_eq!(oversized, SourceResolverContractError::TooManyLocators);

        let record = HydratedProviderRecord {
            event_id: event.event_id(),
            provider_bytes: Vec::new(),
        };
        let oversized_result =
            BatchHydrationResult::new(vec![record; MAX_BATCH_HYDRATION_EVENTS + 1]).unwrap_err();
        assert_eq!(
            oversized_result,
            SourceResolverContractError::TooManyHydratedRecords
        );
    }

    struct EchoResolver;

    impl ContentSourceResolver for EchoResolver {
        fn hydrate_event(
            &self,
            request: &EventHydrationRequest,
        ) -> Result<HydratedProviderRecord, HydrationFailure> {
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: request.event_id().as_uuid().as_bytes().to_vec(),
            })
        }
    }

    #[test]
    fn default_batch_hydration_accepts_multiple_sources_and_preserves_order() {
        let first = source(1);
        let second = source(2);
        let requests = vec![
            request(&first, "first"),
            request(&second, "second"),
            request(&first, "third"),
        ];
        let expected = requests
            .iter()
            .map(EventHydrationRequest::event_id)
            .collect::<Vec<_>>();
        let batch = BatchHydrationRequest::new(requests).unwrap();

        let result = EchoResolver.hydrate_batch(&batch).unwrap();
        assert_eq!(
            result
                .records()
                .iter()
                .map(|record| record.event_id)
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn provider_output_count_order_and_identity_violations_are_internal() {
        let source = source(1);
        let requests = vec![request(&source, "first"), request(&source, "second")];
        let event_ids = requests
            .iter()
            .map(EventHydrationRequest::event_id)
            .collect::<Vec<_>>();
        let batch = BatchHydrationRequest::new(requests).unwrap();
        let record = |event_id| HydratedProviderRecord {
            event_id,
            provider_bytes: Vec::new(),
        };

        let wrong_count = BatchHydrationResult::new(vec![record(event_ids[0])]).unwrap();
        let wrong_order =
            BatchHydrationResult::new(vec![record(event_ids[1]), record(event_ids[0])]).unwrap();
        let wrong_identity = BatchHydrationResult::new(vec![
            record(event_ids[0]),
            record(event_named(&source, "unrequested")),
        ])
        .unwrap();
        let invalid_result_contract =
            contract_hydration_failure(SourceResolverContractError::TooManyHydratedRecords);

        for failure in [
            wrong_count.validate_for_request(&batch).unwrap_err(),
            wrong_order.validate_for_request(&batch).unwrap_err(),
            wrong_identity.validate_for_request(&batch).unwrap_err(),
            invalid_result_contract,
        ] {
            assert_eq!(failure.kind, HydrationFailureKind::Internal);
            assert_eq!(failure.kind.class(), SourceBackedErrorClass::Internal);
        }
    }

    struct ExactFailureResolver {
        failed_event: StableEntityId,
        failure: HydrationFailure,
    }

    impl ContentSourceResolver for ExactFailureResolver {
        fn hydrate_event(
            &self,
            request: &EventHydrationRequest,
        ) -> Result<HydratedProviderRecord, HydrationFailure> {
            if request.event_id() == self.failed_event {
                return Err(self.failure.clone());
            }
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: Vec::new(),
            })
        }
    }

    #[test]
    fn default_batch_hydration_preserves_exact_typed_failure() {
        let source = source(1);
        let first = request(&source, "first");
        let failed = request(&source, "failed");
        let failure = HydrationFailure {
            kind: HydrationFailureKind::StaleRecordEvidence,
            detail: "exact provider failure".to_owned(),
        };
        let resolver = ExactFailureResolver {
            failed_event: failed.event_id(),
            failure: failure.clone(),
        };
        let batch = BatchHydrationRequest::new(vec![first, failed]).unwrap();

        assert_eq!(resolver.hydrate_batch(&batch).unwrap_err(), failure);
    }

    struct BatchOnlyResolver {
        event_calls: Cell<usize>,
        batch_calls: Cell<usize>,
    }

    impl ContentSourceResolver for BatchOnlyResolver {
        fn hydrate_event(
            &self,
            _request: &EventHydrationRequest,
        ) -> Result<HydratedProviderRecord, HydrationFailure> {
            self.event_calls.set(self.event_calls.get() + 1);
            Err(HydrationFailure {
                kind: HydrationFailureKind::MissingRecord,
                detail: "event fallback must not run".to_owned(),
            })
        }

        fn hydrate_batch(
            &self,
            request: &BatchHydrationRequest,
        ) -> Result<BatchHydrationResult, HydrationFailure> {
            self.batch_calls.set(self.batch_calls.get() + 1);
            BatchHydrationResult::new(
                request
                    .events()
                    .iter()
                    .map(|event| HydratedProviderRecord {
                        event_id: event.event_id(),
                        provider_bytes: Vec::new(),
                    })
                    .collect(),
            )
            .map_err(contract_hydration_failure)
        }
    }

    #[test]
    fn default_session_hydration_uses_the_ordered_batch_path() {
        let source = source(1);
        let request = SessionHydrationRequest::new(
            session(&source),
            vec![request(&source, "first"), request(&source, "second")],
        )
        .unwrap();
        let resolver = BatchOnlyResolver {
            event_calls: Cell::new(0),
            batch_calls: Cell::new(0),
        };

        let result = resolver.hydrate_session(&request).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(resolver.batch_calls.get(), 1);
        assert_eq!(resolver.event_calls.get(), 0);
    }
}
