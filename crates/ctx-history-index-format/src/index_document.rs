use std::{iter::Empty, slice, sync::Arc};

use tantivy::schema::{
    document::{ReferenceValue, ReferenceValueLeaf},
    Document, Field, Value,
};
use uuid::Uuid;

use ctx_history_core::{
    CoreContent, CoreRecord, ProviderNativeCopyProof, SourceKey, StableEntityId, StableEntityKind,
    TypedKey, MAX_CORE_CONTENT_BYTES, MAX_ENCODED_CORE_RECORD_BYTES,
};

use crate::{Fields, IndexError, Result};

const BASE_FIELD_VALUES: usize = 34;
pub const SOURCE_EVENT_ORDER_SOURCE_PREFIX_LEN: usize = 64;
pub const SOURCE_EVENT_ORDER_KEY_LEN: usize = 104;
const SOURCE_EVENT_ORDER_EVENT_DIGEST_OFFSET: usize = SOURCE_EVENT_ORDER_SOURCE_PREFIX_LEN;
const SOURCE_EVENT_ORDER_ENCODED_BYTES_OFFSET: usize = SOURCE_EVENT_ORDER_EVENT_DIGEST_OFFSET + 32;
const SOURCE_EVENT_ORDER_CONTENT_BYTES_OFFSET: usize = SOURCE_EVENT_ORDER_ENCODED_BYTES_OFFSET + 4;
pub const SOURCE_EVENT_ORDER_SIZE_SUFFIX_LEN: usize = 8;
const SOURCE_EVENT_ORDER_FIELD: &str = "source_event_order";

pub const SESSION_EVENT_ORDER_SESSION_PREFIX_LEN: usize = StableEntityId::CANONICAL_LEN;
pub const SESSION_EVENT_ORDER_KEY_LEN: usize = SESSION_EVENT_ORDER_SESSION_PREFIX_LEN + 8 + 9 + 16;
const SESSION_EVENT_ORDER_SEQUENCE_OFFSET: usize = SESSION_EVENT_ORDER_SESSION_PREFIX_LEN;
const SESSION_EVENT_ORDER_OCCURRED_AT_OFFSET: usize = SESSION_EVENT_ORDER_SEQUENCE_OFFSET + 8;
const SESSION_EVENT_ORDER_EVENT_ID_OFFSET: usize = SESSION_EVENT_ORDER_OCCURRED_AT_OFFSET + 9;
const SESSION_EVENT_ORDER_FIELD: &str = "session_event_order";

/// Sparse session witness key. The UUID prefix bounds dictionary traversal;
/// the two canonical identities make compact collisions fail closed.
pub const SESSION_AUTHORITY_UUID_PREFIX_LEN: usize = 16;
const SESSION_AUTHORITY_SESSION_OFFSET: usize = SESSION_AUTHORITY_UUID_PREFIX_LEN;
const SESSION_AUTHORITY_SOURCE_OFFSET: usize =
    SESSION_AUTHORITY_SESSION_OFFSET + StableEntityId::CANONICAL_LEN;
pub const SESSION_AUTHORITY_KEY_LEN: usize =
    SESSION_AUTHORITY_SOURCE_OFFSET + StableEntityId::CANONICAL_LEN;
const SESSION_AUTHORITY_FIELD: &str = "session_authority";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionAuthorityKey([u8; SESSION_AUTHORITY_KEY_LEN]);

impl SessionAuthorityKey {
    /// Constructs the exact query-safe key for one fully qualified session
    /// coordinate. The private representation prevents partial UUID terms.
    pub fn exact(session_id: StableEntityId, source_owner: StableEntityId) -> Result<Self> {
        if session_id.entity_kind() != StableEntityKind::Session
            || source_owner.entity_kind() != StableEntityKind::Source
            || session_id.source_digest() != source_owner.digest()
        {
            return Err(IndexError::InvalidStoredDocumentField(
                SESSION_AUTHORITY_FIELD,
            ));
        }
        let mut key = [0; SESSION_AUTHORITY_KEY_LEN];
        key[..SESSION_AUTHORITY_UUID_PREFIX_LEN].copy_from_slice(session_id.as_uuid().as_bytes());
        key[SESSION_AUTHORITY_SESSION_OFFSET..SESSION_AUTHORITY_SOURCE_OFFSET]
            .copy_from_slice(&session_id.encode_canonical()?);
        key[SESSION_AUTHORITY_SOURCE_OFFSET..].copy_from_slice(&source_owner.encode_canonical()?);
        Ok(Self(key))
    }

    pub fn decode(encoded: &[u8]) -> Result<Self> {
        let key: [u8; SESSION_AUTHORITY_KEY_LEN] = encoded
            .try_into()
            .map_err(|_| IndexError::InvalidStoredDocumentField(SESSION_AUTHORITY_FIELD))?;
        let key = Self(key);
        let (session_id, source_owner) = key.identities()?;
        if key.0[..SESSION_AUTHORITY_UUID_PREFIX_LEN] != *session_id.as_uuid().as_bytes() {
            return Err(IndexError::InvalidStoredDocumentField(
                SESSION_AUTHORITY_FIELD,
            ));
        }
        Self::exact(session_id, source_owner)?;
        Ok(key)
    }

    pub fn uuid_prefix(
        session_id: StableEntityId,
    ) -> Result<[u8; SESSION_AUTHORITY_UUID_PREFIX_LEN]> {
        if session_id.entity_kind() != StableEntityKind::Session {
            return Err(IndexError::InvalidStoredDocumentField(
                SESSION_AUTHORITY_FIELD,
            ));
        }
        Ok(Self::uuid_prefix_from_uuid(session_id.as_uuid()))
    }

    pub fn uuid_range_end(session_id: StableEntityId) -> Result<Vec<u8>> {
        if session_id.entity_kind() != StableEntityKind::Session {
            return Err(IndexError::InvalidStoredDocumentField(
                SESSION_AUTHORITY_FIELD,
            ));
        }
        Ok(Self::uuid_range_end_from_uuid(session_id.as_uuid()))
    }

    pub fn uuid_prefix_from_uuid(session_id: Uuid) -> [u8; SESSION_AUTHORITY_UUID_PREFIX_LEN] {
        *session_id.as_bytes()
    }

    pub fn uuid_range_end_from_uuid(session_id: Uuid) -> Vec<u8> {
        let mut end = Vec::with_capacity(SESSION_AUTHORITY_KEY_LEN + 1);
        end.extend_from_slice(&Self::uuid_prefix_from_uuid(session_id));
        end.extend(std::iter::repeat_n(
            u8::MAX,
            SESSION_AUTHORITY_KEY_LEN - SESSION_AUTHORITY_UUID_PREFIX_LEN + 1,
        ));
        end
    }

    pub fn identities(self) -> Result<(StableEntityId, StableEntityId)> {
        let session_id = StableEntityId::decode_canonical(
            &self.0[SESSION_AUTHORITY_SESSION_OFFSET..SESSION_AUTHORITY_SOURCE_OFFSET],
        )?;
        let source_owner =
            StableEntityId::decode_canonical(&self.0[SESSION_AUTHORITY_SOURCE_OFFSET..])?;
        if session_id.entity_kind() != StableEntityKind::Session
            || source_owner.entity_kind() != StableEntityKind::Source
            || session_id.source_digest() != source_owner.digest()
        {
            return Err(IndexError::InvalidStoredDocumentField(
                SESSION_AUTHORITY_FIELD,
            ));
        }
        Ok((session_id, source_owner))
    }

    pub fn as_bytes(&self) -> &[u8; SESSION_AUTHORITY_KEY_LEN] {
        &self.0
    }
    pub fn into_bytes(self) -> [u8; SESSION_AUTHORITY_KEY_LEN] {
        self.0
    }
}

pub const SEMANTIC_EVENT_ORDER_KEY_LEN: usize = 32;
const SEMANTIC_EVENT_ORDER_FIELD: &str = "semantic_event_order";

pub const EVENT_RANGE_ORDER_KEY_LEN: usize = 57;
const EVENT_RANGE_ORDER_TIMESTAMP_OFFSET: usize = 1;
const EVENT_RANGE_ORDER_SEQUENCE_OFFSET: usize = 9;
const EVENT_RANGE_ORDER_EVENT_DIGEST_OFFSET: usize = 17;
const EVENT_RANGE_ORDER_ENCODED_BYTES_OFFSET: usize = 49;
const EVENT_RANGE_ORDER_CONTENT_BYTES_OFFSET: usize = 53;
const EVENT_RANGE_ORDER_FIELD: &str = "event_range_order";

/// Unique global chronological key for efficient bounded event traversal.
///
/// The fixed-width big-endian layout preserves `(time_class, occurred_at_ms,
/// event_sequence, full_event_digest)` order and carries exact size metadata
/// so page budgets can be decided before loading a stored Core record. A
/// timestamped range selects class zero; complete enumeration also includes
/// untimestamped class-one records without weakening identity equality to the
/// compact public UUID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct EventRangeOrderKey([u8; EVENT_RANGE_ORDER_KEY_LEN]);

impl EventRangeOrderKey {
    pub fn for_core_record(
        record: &CoreRecord,
        encoded_core_bytes: usize,
        content_bytes: usize,
    ) -> Result<Self> {
        if encoded_core_bytes == 0 || encoded_core_bytes > MAX_ENCODED_CORE_RECORD_BYTES {
            return Err(IndexError::DocumentFieldTooLarge {
                field: "core_record",
                actual: encoded_core_bytes,
                maximum: MAX_ENCODED_CORE_RECORD_BYTES,
            });
        }
        if content_bytes > MAX_CORE_CONTENT_BYTES {
            return Err(IndexError::DocumentFieldTooLarge {
                field: "core_content",
                actual: content_bytes,
                maximum: MAX_CORE_CONTENT_BYTES,
            });
        }
        let encoded_core_bytes = u32::try_from(encoded_core_bytes).map_err(|_| {
            IndexError::WriterInvariant("encoded Core size does not fit the event range key")
        })?;
        let content_bytes = u32::try_from(content_bytes).map_err(|_| {
            IndexError::WriterInvariant("Core content size does not fit the event range key")
        })?;
        let mut key = [0_u8; EVENT_RANGE_ORDER_KEY_LEN];
        if let Some(occurred_at_unix_ms) = record.occurred_at_unix_ms {
            let sortable_timestamp = (occurred_at_unix_ms as u64) ^ (1_u64 << 63);
            key[EVENT_RANGE_ORDER_TIMESTAMP_OFFSET..EVENT_RANGE_ORDER_SEQUENCE_OFFSET]
                .copy_from_slice(&sortable_timestamp.to_be_bytes());
        } else {
            key[0] = 1;
        }
        key[EVENT_RANGE_ORDER_SEQUENCE_OFFSET..EVENT_RANGE_ORDER_EVENT_DIGEST_OFFSET]
            .copy_from_slice(&record.event_sequence.to_be_bytes());
        key[EVENT_RANGE_ORDER_EVENT_DIGEST_OFFSET..EVENT_RANGE_ORDER_ENCODED_BYTES_OFFSET]
            .copy_from_slice(&record.event_id.digest());
        key[EVENT_RANGE_ORDER_ENCODED_BYTES_OFFSET..EVENT_RANGE_ORDER_CONTENT_BYTES_OFFSET]
            .copy_from_slice(&encoded_core_bytes.to_be_bytes());
        key[EVENT_RANGE_ORDER_CONTENT_BYTES_OFFSET..].copy_from_slice(&content_bytes.to_be_bytes());
        Ok(Self(key))
    }

    pub fn decode(encoded: &[u8]) -> Result<Self> {
        let key: [u8; EVENT_RANGE_ORDER_KEY_LEN] = encoded
            .try_into()
            .map_err(|_| IndexError::InvalidStoredDocumentField(EVENT_RANGE_ORDER_FIELD))?;
        let key = Self(key);
        if key.0[0] > 1
            || (key.0[0] == 1
                && key.0[EVENT_RANGE_ORDER_TIMESTAMP_OFFSET..EVENT_RANGE_ORDER_SEQUENCE_OFFSET]
                    != [0_u8; 8])
            || key.encoded_core_bytes() == 0
            || key.encoded_core_bytes() > MAX_ENCODED_CORE_RECORD_BYTES
            || key.content_bytes() > MAX_CORE_CONTENT_BYTES
        {
            return Err(IndexError::InvalidStoredDocumentField(
                EVENT_RANGE_ORDER_FIELD,
            ));
        }
        Ok(key)
    }

    pub fn timestamp_prefix(occurred_at_unix_ms: i64) -> [u8; 9] {
        let mut prefix = [0_u8; 9];
        prefix[1..].copy_from_slice(&((occurred_at_unix_ms as u64) ^ (1_u64 << 63)).to_be_bytes());
        prefix
    }

    pub fn occurred_at_unix_ms(self) -> Option<i64> {
        (self.0[0] == 0).then(|| {
            let encoded = self.0
                [EVENT_RANGE_ORDER_TIMESTAMP_OFFSET..EVENT_RANGE_ORDER_SEQUENCE_OFFSET]
                .try_into()
                .expect("fixed event range timestamp layout");
            (u64::from_be_bytes(encoded) ^ (1_u64 << 63)) as i64
        })
    }

    /// Returns the exact event sequence carried by this authenticated order
    /// key without loading the stored Core record.
    pub fn event_sequence(self) -> u64 {
        u64::from_be_bytes(
            self.0[EVENT_RANGE_ORDER_SEQUENCE_OFFSET..EVENT_RANGE_ORDER_EVENT_DIGEST_OFFSET]
                .try_into()
                .expect("fixed event range sequence layout"),
        )
    }

    /// Returns the full stable event-identity digest used as the final global
    /// ordering authority. All keys are event identities at the same contract
    /// version, so this is their canonical stable-ID order.
    pub fn event_identity_digest(self) -> [u8; 32] {
        self.0[EVENT_RANGE_ORDER_EVENT_DIGEST_OFFSET..EVENT_RANGE_ORDER_ENCODED_BYTES_OFFSET]
            .try_into()
            .expect("fixed event range identity layout")
    }

    pub fn encoded_core_bytes(self) -> usize {
        u32::from_be_bytes(
            self.0[EVENT_RANGE_ORDER_ENCODED_BYTES_OFFSET..EVENT_RANGE_ORDER_CONTENT_BYTES_OFFSET]
                .try_into()
                .expect("fixed event range encoded-size layout"),
        ) as usize
    }

    pub fn content_bytes(self) -> usize {
        u32::from_be_bytes(
            self.0[EVENT_RANGE_ORDER_CONTENT_BYTES_OFFSET..]
                .try_into()
                .expect("fixed event range content-size layout"),
        ) as usize
    }

    pub fn into_bytes(self) -> [u8; EVENT_RANGE_ORDER_KEY_LEN] {
        self.0
    }

    pub fn as_bytes(&self) -> &[u8; EVENT_RANGE_ORDER_KEY_LEN] {
        &self.0
    }
}

/// Core-owned global event order term.
///
/// Every key is one full event-identity digest. All event identities have the
/// same kind and identity-version prefix, so digest byte order is exactly the
/// canonical `StableEntityId` order. Semantic consumers filter this neutral
/// immutable enumeration under their own current policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SemanticEventOrderKey([u8; SEMANTIC_EVENT_ORDER_KEY_LEN]);

impl SemanticEventOrderKey {
    pub fn for_event(event_id: StableEntityId) -> Result<Self> {
        if event_id.entity_kind() != StableEntityKind::Event {
            return Err(IndexError::WriterInvariant(
                "semantic event order requires an event identity",
            ));
        }
        Ok(Self(event_id.digest()))
    }

    pub fn decode(encoded: &[u8]) -> Result<Self> {
        let key = encoded
            .try_into()
            .map_err(|_| IndexError::InvalidStoredDocumentField(SEMANTIC_EVENT_ORDER_FIELD))?;
        Ok(Self(key))
    }

    pub fn event_digest(self) -> [u8; SEMANTIC_EVENT_ORDER_KEY_LEN] {
        self.0
    }

    pub fn as_bytes(&self) -> &[u8; SEMANTIC_EVENT_ORDER_KEY_LEN] {
        &self.0
    }

    pub fn into_bytes(self) -> [u8; SEMANTIC_EVENT_ORDER_KEY_LEN] {
        self.0
    }
}

/// Exact session-coordinate term used for bounded forward traversal.
///
/// Big-endian encoding preserves the existing deterministic session order:
/// sequence, `None` before `Some(timestamp)`, signed timestamp, then compact
/// event UUID. The full canonical session identity is the range prefix, so a
/// compact UUID collision can never mix session ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionEventOrderKey([u8; SESSION_EVENT_ORDER_KEY_LEN]);

impl SessionEventOrderKey {
    pub fn for_core_record(record: &CoreRecord) -> Result<Self> {
        Self::from_parts(
            record.session_id,
            record.event_sequence,
            record.occurred_at_unix_ms,
            record.event_id.as_uuid(),
        )
    }

    fn from_parts(
        session_id: StableEntityId,
        event_sequence: u64,
        occurred_at_unix_ms: Option<i64>,
        event_id: uuid::Uuid,
    ) -> Result<Self> {
        if session_id.entity_kind() != StableEntityKind::Session {
            return Err(IndexError::WriterInvariant(
                "session event order requires a session identity",
            ));
        }
        let mut key = [0_u8; SESSION_EVENT_ORDER_KEY_LEN];
        key[..SESSION_EVENT_ORDER_SESSION_PREFIX_LEN]
            .copy_from_slice(&session_id.encode_canonical()?);
        key[SESSION_EVENT_ORDER_SEQUENCE_OFFSET..SESSION_EVENT_ORDER_OCCURRED_AT_OFFSET]
            .copy_from_slice(&event_sequence.to_be_bytes());
        if let Some(occurred_at_unix_ms) = occurred_at_unix_ms {
            key[SESSION_EVENT_ORDER_OCCURRED_AT_OFFSET] = 1;
            let sortable = (occurred_at_unix_ms as u64) ^ (1_u64 << 63);
            key[SESSION_EVENT_ORDER_OCCURRED_AT_OFFSET + 1..SESSION_EVENT_ORDER_EVENT_ID_OFFSET]
                .copy_from_slice(&sortable.to_be_bytes());
        }
        key[SESSION_EVENT_ORDER_EVENT_ID_OFFSET..].copy_from_slice(event_id.as_bytes());
        Ok(Self(key))
    }

    pub fn decode_for_session(session_id: StableEntityId, encoded: &[u8]) -> Result<Self> {
        let key: [u8; SESSION_EVENT_ORDER_KEY_LEN] = encoded
            .try_into()
            .map_err(|_| IndexError::InvalidStoredDocumentField(SESSION_EVENT_ORDER_FIELD))?;
        let expected_prefix = Self::session_prefix(session_id)?;
        if key[..SESSION_EVENT_ORDER_SESSION_PREFIX_LEN] != expected_prefix {
            return Err(IndexError::InvalidStoredDocumentField(
                SESSION_EVENT_ORDER_FIELD,
            ));
        }
        if key[SESSION_EVENT_ORDER_OCCURRED_AT_OFFSET] > 1
            || (key[SESSION_EVENT_ORDER_OCCURRED_AT_OFFSET] == 0
                && key[SESSION_EVENT_ORDER_OCCURRED_AT_OFFSET + 1
                    ..SESSION_EVENT_ORDER_EVENT_ID_OFFSET]
                    .iter()
                    .any(|byte| *byte != 0))
        {
            return Err(IndexError::InvalidStoredDocumentField(
                SESSION_EVENT_ORDER_FIELD,
            ));
        }
        Ok(Self(key))
    }

    pub fn session_prefix(
        session_id: StableEntityId,
    ) -> Result<[u8; SESSION_EVENT_ORDER_SESSION_PREFIX_LEN]> {
        if session_id.entity_kind() != StableEntityKind::Session {
            return Err(IndexError::InvalidStoredDocumentField(
                SESSION_EVENT_ORDER_FIELD,
            ));
        }
        Ok(session_id.encode_canonical()?)
    }

    pub fn session_range_end(session_id: StableEntityId) -> Result<Vec<u8>> {
        let mut bound = Vec::with_capacity(SESSION_EVENT_ORDER_KEY_LEN + 1);
        bound.extend_from_slice(&Self::session_prefix(session_id)?);
        bound.extend(std::iter::repeat_n(
            u8::MAX,
            SESSION_EVENT_ORDER_KEY_LEN - SESSION_EVENT_ORDER_SESSION_PREFIX_LEN + 1,
        ));
        Ok(bound)
    }

    pub fn event_sequence(self) -> u64 {
        u64::from_be_bytes(
            self.0[SESSION_EVENT_ORDER_SEQUENCE_OFFSET..SESSION_EVENT_ORDER_OCCURRED_AT_OFFSET]
                .try_into()
                .expect("fixed session event order sequence layout"),
        )
    }

    pub fn occurred_at_unix_ms(self) -> Option<i64> {
        (self.0[SESSION_EVENT_ORDER_OCCURRED_AT_OFFSET] == 1).then(|| {
            let sortable = u64::from_be_bytes(
                self.0[SESSION_EVENT_ORDER_OCCURRED_AT_OFFSET + 1
                    ..SESSION_EVENT_ORDER_EVENT_ID_OFFSET]
                    .try_into()
                    .expect("fixed session event order timestamp layout"),
            );
            (sortable ^ (1_u64 << 63)) as i64
        })
    }

    pub fn event_id(self) -> uuid::Uuid {
        uuid::Uuid::from_bytes(
            self.0[SESSION_EVENT_ORDER_EVENT_ID_OFFSET..]
                .try_into()
                .expect("fixed session event order UUID layout"),
        )
    }

    pub fn as_bytes(&self) -> &[u8; SESSION_EVENT_ORDER_KEY_LEN] {
        &self.0
    }

    pub fn into_bytes(self) -> [u8; SESSION_EVENT_ORDER_KEY_LEN] {
        self.0
    }
}

#[derive(Clone)]
struct IndexSourceFields {
    token: Arc<str>,
    identity_digest: [u8; 32],
    descriptor_digest: [u8; 32],
    provider: Arc<str>,
    source_format: Arc<str>,
}

impl IndexSourceFields {
    fn new(document_source: &ctx_history_core::SourceKey, token: &str) -> Self {
        Self {
            token: Arc::from(token),
            identity_digest: document_source.identity().digest(),
            descriptor_digest: document_source.exact_descriptor_digest(),
            provider: Arc::from(document_source.provider()),
            source_format: Arc::from(document_source.source_format()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceEventOrderKey([u8; SOURCE_EVENT_ORDER_KEY_LEN]);

impl SourceEventOrderKey {
    fn for_document(
        source: &IndexSourceFields,
        event_digest: [u8; 32],
        encoded_core_bytes: usize,
        content_bytes: usize,
    ) -> Result<Self> {
        Self::from_parts(
            source.identity_digest,
            source.descriptor_digest,
            event_digest,
            encoded_core_bytes,
            content_bytes,
        )
    }

    fn from_parts(
        source_digest: [u8; 32],
        source_descriptor_digest: [u8; 32],
        event_digest: [u8; 32],
        encoded_core_bytes: usize,
        content_bytes: usize,
    ) -> Result<Self> {
        if encoded_core_bytes == 0 || encoded_core_bytes > MAX_ENCODED_CORE_RECORD_BYTES {
            return Err(IndexError::DocumentFieldTooLarge {
                field: "core_record",
                actual: encoded_core_bytes,
                maximum: MAX_ENCODED_CORE_RECORD_BYTES,
            });
        }
        if content_bytes > MAX_CORE_CONTENT_BYTES {
            return Err(IndexError::DocumentFieldTooLarge {
                field: "core_content",
                actual: content_bytes,
                maximum: MAX_CORE_CONTENT_BYTES,
            });
        }
        let encoded_core_bytes = u32::try_from(encoded_core_bytes).map_err(|_| {
            IndexError::WriterInvariant("encoded Core size does not fit the source order key")
        })?;
        let content_bytes = u32::try_from(content_bytes).map_err(|_| {
            IndexError::WriterInvariant("Core content size does not fit the source order key")
        })?;

        let mut key = [0_u8; SOURCE_EVENT_ORDER_KEY_LEN];
        key[..32].copy_from_slice(&source_digest);
        key[32..SOURCE_EVENT_ORDER_SOURCE_PREFIX_LEN].copy_from_slice(&source_descriptor_digest);
        key[SOURCE_EVENT_ORDER_EVENT_DIGEST_OFFSET..SOURCE_EVENT_ORDER_ENCODED_BYTES_OFFSET]
            .copy_from_slice(&event_digest);
        key[SOURCE_EVENT_ORDER_ENCODED_BYTES_OFFSET..SOURCE_EVENT_ORDER_CONTENT_BYTES_OFFSET]
            .copy_from_slice(&encoded_core_bytes.to_be_bytes());
        key[SOURCE_EVENT_ORDER_CONTENT_BYTES_OFFSET..]
            .copy_from_slice(&content_bytes.to_be_bytes());
        Ok(Self(key))
    }

    pub fn for_core_record(record: &CoreRecord, encoded_core_bytes: usize) -> Result<Self> {
        Self::from_parts(
            record.source.identity().digest(),
            record.source.exact_descriptor_digest(),
            record.event_id.digest(),
            encoded_core_bytes,
            core_content_bytes(&record.content)?,
        )
    }

    pub fn decode_for_source(source: &SourceKey, encoded: &[u8]) -> Result<Self> {
        let key: [u8; SOURCE_EVENT_ORDER_KEY_LEN] = encoded
            .try_into()
            .map_err(|_| IndexError::InvalidStoredDocumentField(SOURCE_EVENT_ORDER_FIELD))?;
        if key[..SOURCE_EVENT_ORDER_SOURCE_PREFIX_LEN] != Self::source_prefix(source) {
            return Err(IndexError::InvalidStoredDocumentField(
                SOURCE_EVENT_ORDER_FIELD,
            ));
        }
        let key = Self(key);
        if key.encoded_core_bytes() == 0
            || key.encoded_core_bytes() > MAX_ENCODED_CORE_RECORD_BYTES
            || key.content_bytes() > MAX_CORE_CONTENT_BYTES
        {
            return Err(IndexError::InvalidStoredDocumentField(
                SOURCE_EVENT_ORDER_FIELD,
            ));
        }
        Ok(key)
    }

    pub fn source_prefix(source: &SourceKey) -> [u8; SOURCE_EVENT_ORDER_SOURCE_PREFIX_LEN] {
        let mut prefix = [0_u8; SOURCE_EVENT_ORDER_SOURCE_PREFIX_LEN];
        prefix[..32].copy_from_slice(&source.identity().digest());
        prefix[32..].copy_from_slice(&source.exact_descriptor_digest());
        prefix
    }

    pub fn source_range_end(source: &SourceKey) -> Vec<u8> {
        let mut bound = Vec::with_capacity(SOURCE_EVENT_ORDER_KEY_LEN + 1);
        bound.extend_from_slice(&Self::source_prefix(source));
        bound.extend(std::iter::repeat_n(
            u8::MAX,
            SOURCE_EVENT_ORDER_KEY_LEN - SOURCE_EVENT_ORDER_SOURCE_PREFIX_LEN + 1,
        ));
        bound
    }

    pub fn source_after_bound(source: &SourceKey, event_digest: [u8; 32]) -> Vec<u8> {
        let mut bound = Vec::with_capacity(SOURCE_EVENT_ORDER_KEY_LEN + 1);
        bound.extend_from_slice(&Self::source_prefix(source));
        bound.extend_from_slice(&event_digest);
        bound.extend(std::iter::repeat_n(
            u8::MAX,
            SOURCE_EVENT_ORDER_SIZE_SUFFIX_LEN + 1,
        ));
        bound
    }

    pub fn event_digest(self) -> [u8; 32] {
        let mut digest = [0_u8; 32];
        digest.copy_from_slice(
            &self.0
                [SOURCE_EVENT_ORDER_EVENT_DIGEST_OFFSET..SOURCE_EVENT_ORDER_ENCODED_BYTES_OFFSET],
        );
        digest
    }

    pub fn encoded_core_bytes(self) -> usize {
        let mut encoded = [0_u8; 4];
        encoded.copy_from_slice(
            &self.0
                [SOURCE_EVENT_ORDER_ENCODED_BYTES_OFFSET..SOURCE_EVENT_ORDER_CONTENT_BYTES_OFFSET],
        );
        u32::from_be_bytes(encoded) as usize
    }

    pub fn content_bytes(self) -> usize {
        let mut encoded = [0_u8; 4];
        encoded.copy_from_slice(&self.0[SOURCE_EVENT_ORDER_CONTENT_BYTES_OFFSET..]);
        u32::from_be_bytes(encoded) as usize
    }

    pub fn into_bytes(self) -> [u8; SOURCE_EVENT_ORDER_KEY_LEN] {
        self.0
    }
}

pub fn core_content_bytes(content: &CoreContent) -> Result<usize> {
    let content_bytes = content
        .encoded_content_bytes()
        .map_err(IndexError::CoreRecord)?;
    if content_bytes > MAX_CORE_CONTENT_BYTES {
        return Err(IndexError::DocumentFieldTooLarge {
            field: "core_content",
            actual: content_bytes,
            maximum: MAX_CORE_CONTENT_BYTES,
        });
    }
    Ok(content_bytes)
}

/// Derives the lexical body projection for one complete Core record.
///
/// Provider-native copy claims do not imply a derived origin classification or
/// alter discovery. Only the content-owned discovery policy controls postings.
pub fn project_indexed_body_search(content: CoreContent) -> Result<Option<String>> {
    crate::project_body_search(content)
}

pub(crate) fn event_copy_proof_str(proof: ProviderNativeCopyProof) -> &'static str {
    match proof {
        ProviderNativeCopyProof::NativeEventIdentity => "native_event_identity",
        ProviderNativeCopyProof::NativeCopiedFromField => "native_copied_from_field",
        ProviderNativeCopyProof::NativeCallResultIdentity => "native_call_result_identity",
    }
}
#[cfg(test)]
pub struct SourceToken([u8; 64]);

#[cfg(test)]
impl SourceToken {
    pub fn new(source_digest: &[u8; 32]) -> Self {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";

        let mut encoded = [0_u8; 64];
        for (index, byte) in source_digest.iter().copied().enumerate() {
            encoded[index * 2] = DIGITS[(byte >> 4) as usize];
            encoded[index * 2 + 1] = DIGITS[(byte & 0x0f) as usize];
        }
        Self(encoded)
    }

    pub fn as_str(&self) -> Result<&str> {
        std::str::from_utf8(&self.0).map_err(|_| {
            IndexError::WriterInvariant("source token encoding produced invalid UTF-8")
        })
    }
}

#[derive(Debug)]
pub enum IndexValue {
    Text(String),
    SharedText(Arc<str>),
    Bytes(Vec<u8>),
    U64(u64),
    I64(i64),
}

impl<'a> Value<'a> for &'a IndexValue {
    type ArrayIter = Empty<Self>;
    type ObjectIter = Empty<(&'a str, Self)>;

    fn as_value(&self) -> ReferenceValue<'a, Self> {
        let leaf = match self {
            IndexValue::Text(value) => ReferenceValueLeaf::Str(value),
            IndexValue::SharedText(value) => ReferenceValueLeaf::Str(value),
            IndexValue::Bytes(value) => ReferenceValueLeaf::Bytes(value),
            IndexValue::U64(value) => ReferenceValueLeaf::U64(*value),
            IndexValue::I64(value) => ReferenceValueLeaf::I64(*value),
        };
        ReferenceValue::Leaf(leaf)
    }
}

/// Opaque schema-owned projection accepted by the production index writer.
///
/// Callers can construct this type only through the canonical [`Self::from_core`]
/// projection. Raw construction, mutation, and conversion remain unavailable
/// even when every Cargo feature is enabled:
///
/// ```compile_fail
/// use ctx_history_index_format::IndexDocument;
///
/// let _ = IndexDocument::with_capacity(1);
/// ```
///
/// ```compile_fail
/// use ctx_history_index_format::IndexDocument;
/// use tantivy::schema::Field;
///
/// fn mutate(document: &mut IndexDocument, field: Field) {
///     document.add_u64(field, 1);
/// }
/// ```
///
/// ```compile_fail
/// use ctx_history_index_format::IndexDocument;
///
/// fn convert(document: IndexDocument) {
///     let _ = document.into_tantivy_document();
/// }
/// ```
///
/// ```compile_fail
/// use ctx_history_index_format::{Fields, IndexDocument, SessionAuthorityKey};
///
/// fn attach_arbitrary_authority(
///     document: &mut IndexDocument,
///     fields: Fields,
///     authority: SessionAuthorityKey,
/// ) {
///     document.add_session_authority(fields, authority);
/// }
/// ```
pub struct IndexDocument {
    fields: Vec<(Field, IndexValue)>,
    // This remains private so a writer can elect a witness but cannot choose
    // the session/source identity written for that witness.
    session_authority: Option<SessionAuthorityKey>,
}

impl IndexDocument {
    fn with_capacity(field_values: usize) -> Self {
        Self {
            fields: Vec::with_capacity(field_values),
            session_authority: None,
        }
    }

    fn add_text(&mut self, field: Field, value: String) {
        self.fields.push((field, IndexValue::Text(value)));
    }

    fn add_shared_text(&mut self, field: Field, value: Arc<str>) {
        self.fields.push((field, IndexValue::SharedText(value)));
    }

    fn add_bytes(&mut self, field: Field, value: impl Into<Vec<u8>>) {
        self.fields.push((field, IndexValue::Bytes(value.into())));
    }

    /// Adds the sparse stored+indexed Core witness selected by the writer.
    ///
    /// The witness key is derived from this document's private Core identity
    /// in [`Self::from_core`] and can be attached only once.
    #[doc(hidden)]
    pub fn add_session_authority(&mut self, fields: Fields) {
        if let Some(authority) = self.session_authority.take() {
            self.add_bytes(fields.session_authority, authority.into_bytes());
        }
    }

    fn add_u64(&mut self, field: Field, value: u64) {
        self.fields.push((field, IndexValue::U64(value)));
    }

    fn add_i64(&mut self, field: Field, value: i64) {
        self.fields.push((field, IndexValue::I64(value)));
    }

    #[cfg(test)]
    fn into_tantivy_document(self) -> tantivy::TantivyDocument {
        let mut document = tantivy::TantivyDocument::default();
        for (field, value) in self.fields {
            match value {
                IndexValue::Text(value) => document.add_text(field, value),
                IndexValue::SharedText(value) => document.add_text(field, value),
                IndexValue::Bytes(value) => document.add_bytes(field, &value),
                IndexValue::U64(value) => document.add_u64(field, value),
                IndexValue::I64(value) => document.add_i64(field, value),
            }
        }
        document
    }

    #[doc(hidden)]
    pub fn from_core(
        fields: Fields,
        record: CoreRecord,
        core_record_bytes: Vec<u8>,
        core_content_bytes: usize,
    ) -> Result<Self> {
        let session_authority =
            SessionAuthorityKey::exact(record.session_id, record.source.identity())?;
        let source_token = crate::source_token(&record.source);
        let source = IndexSourceFields::new(&record.source, &source_token);
        let core_record_encoded_bytes = core_record_bytes.len();
        let discovery_eligible = record.content.is_discovery_eligible();
        let semantic_event_order = SemanticEventOrderKey::for_event(record.event_id)?;
        let source_event_order = SourceEventOrderKey::for_document(
            &source,
            record.event_id.digest(),
            core_record_encoded_bytes,
            core_content_bytes,
        )?;
        let session_event_order = SessionEventOrderKey::for_core_record(&record)?;
        let event_range_order = EventRangeOrderKey::for_core_record(
            &record,
            core_record_encoded_bytes,
            core_content_bytes,
        )?;
        let literal_fact_values = record
            .content
            .activity
            .as_ref()
            .map_or(0, |activity| activity.facts.len());
        let mut target = Self::with_capacity(BASE_FIELD_VALUES + literal_fact_values);
        target.session_authority = Some(session_authority);
        target.add_text(fields.event_id, record.event_id.to_string());
        target.add_text(
            fields.event_identity_digest,
            crate::hex(&record.event_id.digest()),
        );
        let event_uuid = record.event_id.as_uuid().as_u128();
        target.add_u64(fields.event_id_high, (event_uuid >> 64) as u64);
        target.add_u64(fields.event_id_low, event_uuid as u64);
        target.add_text(fields.session_id, record.session_id.to_string());
        let session_uuid = record.session_id.as_uuid().as_u128();
        target.add_u64(fields.session_id_high, (session_uuid >> 64) as u64);
        target.add_u64(fields.session_id_low, session_uuid as u64);
        if let Some(parent_session_id) = record.parent_session_id {
            target.add_text(fields.parent_session_id, parent_session_id.to_string());
        }
        if let Some(root_session_id) = record.root_session_id {
            target.add_text(fields.root_session_id, root_session_id.to_string());
        }
        if let Some(relationship) = record.session_relationship {
            target.add_text(
                fields.provider_native_session_relationship,
                relationship.as_str().to_owned(),
            );
        }
        if let Some(copy) = record.event_copy {
            target.add_text(
                fields.event_copy_ancestor_session_id,
                copy.ancestor_session_id.to_string(),
            );
            target.add_text(
                fields.event_copy_ancestor_event_id,
                copy.ancestor_event_id.to_string(),
            );
            target.add_text(
                fields.event_copy_proof,
                event_copy_proof_str(copy.proof).to_owned(),
            );
        }
        target.add_shared_text(fields.source_key, source.token);
        target.add_shared_text(fields.provider, source.provider);
        target.add_shared_text(fields.source_format, source.source_format);
        if record.source.provider() == "custom" {
            if let Some(TypedKey::Composite(values)) = record.native_event_id.as_ref() {
                if let [TypedKey::Utf8(provider_key), TypedKey::Utf8(source_id), TypedKey::Utf8(_)] =
                    values.as_slice()
                {
                    target.add_text(fields.custom_provider_key, provider_key.clone());
                    target.add_text(fields.custom_source_id, source_id.clone());
                }
            }
        }
        if let Some(provider_session_id) = record.provider_session_id {
            target.add_text(fields.provider_session_id, provider_session_id);
        }
        if let Some(agent_scope) = record.agent_scope {
            target.add_text(fields.agent_scope, agent_scope.as_str().to_owned());
        }
        target.add_u64(fields.event_sequence, record.event_sequence);
        if let Some(occurred_at_unix_ms) = record.occurred_at_unix_ms {
            target.add_i64(fields.occurred_at_unix_ms, occurred_at_unix_ms);
        }
        target.add_text(fields.event_type, record.event_type);
        if let Some(role) = record.role {
            target.add_text(fields.role, role);
        }
        if let Some(activity) = record.content.activity.as_ref() {
            for fact in &activity.facts {
                target.add_text(fields.literal_fact(fact.kind), fact.value.clone());
            }
        }
        if let Some(body) = project_indexed_body_search(record.content)? {
            target.add_text(fields.body_search, body);
        }
        target.add_u64(
            fields.core_content_bytes,
            u64::try_from(core_content_bytes)
                .map_err(|_| IndexError::WriterInvariant("Core content size does not fit u64"))?,
        );
        target.add_u64(
            fields.core_record_encoded_bytes,
            u64::try_from(core_record_encoded_bytes).map_err(|_| {
                IndexError::WriterInvariant("encoded Core record size does not fit u64")
            })?,
        );
        target.add_bytes(fields.core_record, core_record_bytes);
        target.add_bytes(fields.source_event_order, source_event_order.into_bytes());
        target.add_bytes(fields.session_event_order, session_event_order.into_bytes());
        target.add_bytes(
            fields.semantic_event_order,
            semantic_event_order.into_bytes(),
        );
        target.add_bytes(fields.event_range_order, event_range_order.into_bytes());
        if discovery_eligible {
            target.add_u64(fields.discovery_eligible, 1);
        }
        Ok(target)
    }
}

pub struct IndexDocumentIter<'a>(slice::Iter<'a, (Field, IndexValue)>);

impl<'a> Iterator for IndexDocumentIter<'a> {
    type Item = (Field, &'a IndexValue);

    fn next(&mut self) -> Option<Self::Item> {
        let (field, value) = self.0.next()?;
        Some((*field, value))
    }
}

impl Document for IndexDocument {
    type Value<'a> = &'a IndexValue;
    type FieldsValuesIter<'a> = IndexDocumentIter<'a>;

    fn iter_fields_and_values(&self) -> Self::FieldsValuesIter<'_> {
        IndexDocumentIter(self.fields.iter())
    }
}

#[cfg(test)]
mod tests;
