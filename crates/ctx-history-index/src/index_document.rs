use std::{iter::Empty, slice, sync::Arc};

use tantivy::schema::{
    document::{ReferenceValue, ReferenceValueLeaf},
    Document, Field, Value,
};

use ctx_history_core::{
    CoreContent, CoreRecord, EventOrigin, RepositoryVcsObservationKind, SourceKey, StableEntityId,
    StableEntityKind, TypedKey, MAX_CORE_CONTENT_BYTES, MAX_ENCODED_CORE_RECORD_BYTES,
};

use crate::{Fields, IndexError, Result};

const BASE_FIELD_VALUES: usize = 33;
pub(crate) const SOURCE_EVENT_ORDER_SOURCE_PREFIX_LEN: usize = 64;
pub(crate) const SOURCE_EVENT_ORDER_KEY_LEN: usize = 104;
const SOURCE_EVENT_ORDER_EVENT_DIGEST_OFFSET: usize = SOURCE_EVENT_ORDER_SOURCE_PREFIX_LEN;
const SOURCE_EVENT_ORDER_ENCODED_BYTES_OFFSET: usize = SOURCE_EVENT_ORDER_EVENT_DIGEST_OFFSET + 32;
const SOURCE_EVENT_ORDER_CONTENT_BYTES_OFFSET: usize = SOURCE_EVENT_ORDER_ENCODED_BYTES_OFFSET + 4;
const SOURCE_EVENT_ORDER_SIZE_SUFFIX_LEN: usize = 8;
const SOURCE_EVENT_ORDER_FIELD: &str = "source_event_order";

pub(crate) const SESSION_EVENT_ORDER_SESSION_PREFIX_LEN: usize = StableEntityId::CANONICAL_LEN;
pub(crate) const SESSION_EVENT_ORDER_KEY_LEN: usize =
    SESSION_EVENT_ORDER_SESSION_PREFIX_LEN + 8 + 9 + 16;
const SESSION_EVENT_ORDER_SEQUENCE_OFFSET: usize = SESSION_EVENT_ORDER_SESSION_PREFIX_LEN;
const SESSION_EVENT_ORDER_OCCURRED_AT_OFFSET: usize = SESSION_EVENT_ORDER_SEQUENCE_OFFSET + 8;
const SESSION_EVENT_ORDER_EVENT_ID_OFFSET: usize = SESSION_EVENT_ORDER_OCCURRED_AT_OFFSET + 9;
const SESSION_EVENT_ORDER_FIELD: &str = "session_event_order";

pub(crate) const SEMANTIC_EVENT_ORDER_KEY_LEN: usize = 32;
const SEMANTIC_EVENT_ORDER_FIELD: &str = "semantic_event_order";

pub(crate) const EVENT_RANGE_ORDER_KEY_LEN: usize = 57;
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
pub(crate) struct EventRangeOrderKey([u8; EVENT_RANGE_ORDER_KEY_LEN]);

impl EventRangeOrderKey {
    pub(crate) fn for_core_record(
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

    pub(crate) fn decode(encoded: &[u8]) -> Result<Self> {
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

    pub(crate) fn timestamp_prefix(occurred_at_unix_ms: i64) -> [u8; 9] {
        let mut prefix = [0_u8; 9];
        prefix[1..].copy_from_slice(&((occurred_at_unix_ms as u64) ^ (1_u64 << 63)).to_be_bytes());
        prefix
    }

    pub(crate) fn occurred_at_unix_ms(self) -> Option<i64> {
        (self.0[0] == 0).then(|| {
            let encoded = self.0
                [EVENT_RANGE_ORDER_TIMESTAMP_OFFSET..EVENT_RANGE_ORDER_SEQUENCE_OFFSET]
                .try_into()
                .expect("fixed event range timestamp layout");
            (u64::from_be_bytes(encoded) ^ (1_u64 << 63)) as i64
        })
    }

    pub(crate) fn encoded_core_bytes(self) -> usize {
        u32::from_be_bytes(
            self.0[EVENT_RANGE_ORDER_ENCODED_BYTES_OFFSET..EVENT_RANGE_ORDER_CONTENT_BYTES_OFFSET]
                .try_into()
                .expect("fixed event range encoded-size layout"),
        ) as usize
    }

    pub(crate) fn content_bytes(self) -> usize {
        u32::from_be_bytes(
            self.0[EVENT_RANGE_ORDER_CONTENT_BYTES_OFFSET..]
                .try_into()
                .expect("fixed event range content-size layout"),
        ) as usize
    }

    pub(crate) fn into_bytes(self) -> [u8; EVENT_RANGE_ORDER_KEY_LEN] {
        self.0
    }

    pub(crate) fn as_bytes(&self) -> &[u8; EVENT_RANGE_ORDER_KEY_LEN] {
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
pub(crate) struct SemanticEventOrderKey([u8; SEMANTIC_EVENT_ORDER_KEY_LEN]);

impl SemanticEventOrderKey {
    pub(crate) fn for_event(event_id: StableEntityId) -> Result<Self> {
        if event_id.entity_kind() != StableEntityKind::Event {
            return Err(IndexError::WriterInvariant(
                "semantic event order requires an event identity",
            ));
        }
        Ok(Self(event_id.digest()))
    }

    pub(crate) fn decode(encoded: &[u8]) -> Result<Self> {
        let key = encoded
            .try_into()
            .map_err(|_| IndexError::InvalidStoredDocumentField(SEMANTIC_EVENT_ORDER_FIELD))?;
        Ok(Self(key))
    }

    pub(crate) fn event_digest(self) -> [u8; SEMANTIC_EVENT_ORDER_KEY_LEN] {
        self.0
    }

    pub(crate) fn as_bytes(&self) -> &[u8; SEMANTIC_EVENT_ORDER_KEY_LEN] {
        &self.0
    }

    pub(crate) fn into_bytes(self) -> [u8; SEMANTIC_EVENT_ORDER_KEY_LEN] {
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
pub(crate) struct SessionEventOrderKey([u8; SESSION_EVENT_ORDER_KEY_LEN]);

impl SessionEventOrderKey {
    pub(crate) fn for_core_record(record: &CoreRecord) -> Result<Self> {
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

    pub(crate) fn decode_for_session(session_id: StableEntityId, encoded: &[u8]) -> Result<Self> {
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

    pub(crate) fn session_prefix(
        session_id: StableEntityId,
    ) -> Result<[u8; SESSION_EVENT_ORDER_SESSION_PREFIX_LEN]> {
        if session_id.entity_kind() != StableEntityKind::Session {
            return Err(IndexError::InvalidStoredDocumentField(
                SESSION_EVENT_ORDER_FIELD,
            ));
        }
        Ok(session_id.encode_canonical()?)
    }

    pub(crate) fn session_range_end(session_id: StableEntityId) -> Result<Vec<u8>> {
        let mut bound = Vec::with_capacity(SESSION_EVENT_ORDER_KEY_LEN + 1);
        bound.extend_from_slice(&Self::session_prefix(session_id)?);
        bound.extend(std::iter::repeat_n(
            u8::MAX,
            SESSION_EVENT_ORDER_KEY_LEN - SESSION_EVENT_ORDER_SESSION_PREFIX_LEN + 1,
        ));
        Ok(bound)
    }

    pub(crate) fn event_sequence(self) -> u64 {
        u64::from_be_bytes(
            self.0[SESSION_EVENT_ORDER_SEQUENCE_OFFSET..SESSION_EVENT_ORDER_OCCURRED_AT_OFFSET]
                .try_into()
                .expect("fixed session event order sequence layout"),
        )
    }

    pub(crate) fn occurred_at_unix_ms(self) -> Option<i64> {
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

    pub(crate) fn event_id(self) -> uuid::Uuid {
        uuid::Uuid::from_bytes(
            self.0[SESSION_EVENT_ORDER_EVENT_ID_OFFSET..]
                .try_into()
                .expect("fixed session event order UUID layout"),
        )
    }

    pub(crate) fn as_bytes(&self) -> &[u8; SESSION_EVENT_ORDER_KEY_LEN] {
        &self.0
    }

    pub(crate) fn into_bytes(self) -> [u8; SESSION_EVENT_ORDER_KEY_LEN] {
        self.0
    }
}

#[derive(Clone)]
pub(super) struct IndexSourceFields {
    token: Arc<str>,
    identity_digest: [u8; 32],
    descriptor_digest: [u8; 32],
    provider: Arc<str>,
    source_format: Arc<str>,
}

impl IndexSourceFields {
    pub(super) fn new(document_source: &ctx_history_core::SourceKey, token: &str) -> Self {
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
pub(crate) struct SourceEventOrderKey([u8; SOURCE_EVENT_ORDER_KEY_LEN]);

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

    pub(crate) fn for_core_record(record: &CoreRecord, encoded_core_bytes: usize) -> Result<Self> {
        Self::from_parts(
            record.source.identity().digest(),
            record.source.exact_descriptor_digest(),
            record.event_id.digest(),
            encoded_core_bytes,
            core_content_bytes(&record.content)?,
        )
    }

    pub(crate) fn decode_for_source(source: &SourceKey, encoded: &[u8]) -> Result<Self> {
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

    pub(crate) fn source_prefix(source: &SourceKey) -> [u8; SOURCE_EVENT_ORDER_SOURCE_PREFIX_LEN] {
        let mut prefix = [0_u8; SOURCE_EVENT_ORDER_SOURCE_PREFIX_LEN];
        prefix[..32].copy_from_slice(&source.identity().digest());
        prefix[32..].copy_from_slice(&source.exact_descriptor_digest());
        prefix
    }

    pub(crate) fn source_range_end(source: &SourceKey) -> Vec<u8> {
        let mut bound = Vec::with_capacity(SOURCE_EVENT_ORDER_KEY_LEN + 1);
        bound.extend_from_slice(&Self::source_prefix(source));
        bound.extend(std::iter::repeat_n(
            u8::MAX,
            SOURCE_EVENT_ORDER_KEY_LEN - SOURCE_EVENT_ORDER_SOURCE_PREFIX_LEN + 1,
        ));
        bound
    }

    pub(crate) fn source_after_bound(source: &SourceKey, event_digest: [u8; 32]) -> Vec<u8> {
        let mut bound = Vec::with_capacity(SOURCE_EVENT_ORDER_KEY_LEN + 1);
        bound.extend_from_slice(&Self::source_prefix(source));
        bound.extend_from_slice(&event_digest);
        bound.extend(std::iter::repeat_n(
            u8::MAX,
            SOURCE_EVENT_ORDER_SIZE_SUFFIX_LEN + 1,
        ));
        bound
    }

    pub(crate) fn event_digest(self) -> [u8; 32] {
        let mut digest = [0_u8; 32];
        digest.copy_from_slice(
            &self.0
                [SOURCE_EVENT_ORDER_EVENT_DIGEST_OFFSET..SOURCE_EVENT_ORDER_ENCODED_BYTES_OFFSET],
        );
        digest
    }

    pub(crate) fn encoded_core_bytes(self) -> usize {
        let mut encoded = [0_u8; 4];
        encoded.copy_from_slice(
            &self.0
                [SOURCE_EVENT_ORDER_ENCODED_BYTES_OFFSET..SOURCE_EVENT_ORDER_CONTENT_BYTES_OFFSET],
        );
        u32::from_be_bytes(encoded) as usize
    }

    pub(crate) fn content_bytes(self) -> usize {
        let mut encoded = [0_u8; 4];
        encoded.copy_from_slice(&self.0[SOURCE_EVENT_ORDER_CONTENT_BYTES_OFFSET..]);
        u32::from_be_bytes(encoded) as usize
    }

    pub(crate) fn into_bytes(self) -> [u8; SOURCE_EVENT_ORDER_KEY_LEN] {
        self.0
    }
}

pub(crate) fn core_content_bytes(content: &CoreContent) -> Result<usize> {
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
/// Proven ancestor copies remain complete stored Core records, but their body
/// must not create another full-text posting set. Unknown and positively
/// unique records retain the ordinary policy-selected body projection.
pub(crate) fn project_indexed_body_search(
    event_origin: &EventOrigin,
    content: CoreContent,
) -> Result<Option<String>> {
    if matches!(event_origin, EventOrigin::CopiedFromAncestor { .. }) {
        return Ok(None);
    }
    crate::project_body_search(content)
}
#[cfg(test)]
pub(super) struct SourceToken([u8; 64]);

#[cfg(test)]
impl SourceToken {
    pub(super) fn new(source_digest: &[u8; 32]) -> Self {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";

        let mut encoded = [0_u8; 64];
        for (index, byte) in source_digest.iter().copied().enumerate() {
            encoded[index * 2] = DIGITS[(byte >> 4) as usize];
            encoded[index * 2 + 1] = DIGITS[(byte & 0x0f) as usize];
        }
        Self(encoded)
    }

    pub(super) fn as_str(&self) -> Result<&str> {
        std::str::from_utf8(&self.0).map_err(|_| {
            IndexError::WriterInvariant("source token encoding produced invalid UTF-8")
        })
    }
}

#[derive(Debug)]
pub(super) enum IndexValue {
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

pub(super) struct IndexDocument {
    fields: Vec<(Field, IndexValue)>,
}

impl IndexDocument {
    pub(super) fn with_capacity(field_values: usize) -> Self {
        Self {
            fields: Vec::with_capacity(field_values),
        }
    }

    pub(super) fn add_text(&mut self, field: Field, value: String) {
        self.fields.push((field, IndexValue::Text(value)));
    }

    pub(super) fn add_shared_text(&mut self, field: Field, value: Arc<str>) {
        self.fields.push((field, IndexValue::SharedText(value)));
    }

    pub(super) fn add_bytes(&mut self, field: Field, value: impl Into<Vec<u8>>) {
        self.fields.push((field, IndexValue::Bytes(value.into())));
    }

    pub(super) fn add_u64(&mut self, field: Field, value: u64) {
        self.fields.push((field, IndexValue::U64(value)));
    }

    pub(super) fn add_i64(&mut self, field: Field, value: i64) {
        self.fields.push((field, IndexValue::I64(value)));
    }

    #[cfg(test)]
    pub(super) fn into_tantivy_document(self) -> tantivy::TantivyDocument {
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

    pub(super) fn from_core(
        fields: Fields,
        record: CoreRecord,
        core_record_bytes: Vec<u8>,
        core_content_bytes: usize,
        source: IndexSourceFields,
    ) -> Result<Self> {
        let core_record_encoded_bytes = core_record_bytes.len();
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
        let repository_path_values = record
            .repository_file_observations
            .iter()
            .map(|observation| 1 + usize::from(observation.prior_relative_path.is_some()))
            .sum::<usize>();
        let produced_object_values = record
            .repository_vcs_observations
            .iter()
            .filter_map(|observation| match &observation.kind {
                RepositoryVcsObservationKind::Outcome(outcome) => Some(outcome),
                _ => None,
            })
            .map(|outcome| outcome.produced_object_ids.len())
            .sum::<usize>();
        let mut target = Self::with_capacity(
            BASE_FIELD_VALUES + repository_path_values + produced_object_values,
        );
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
        target.add_text(fields.root_session_id, record.root_session_id.to_string());
        target.add_text(
            fields.session_relationship_kind,
            record.session_relationship.as_str().to_owned(),
        );
        target.add_text(
            fields.event_origin_kind,
            record.event_origin.kind_str().to_owned(),
        );
        if let EventOrigin::CopiedFromAncestor {
            ancestor_event_id, ..
        } = &record.event_origin
        {
            target.add_text(
                fields.origin_event_identity_digest,
                crate::hex(&ancestor_event_id.digest()),
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
        if let Some(branch) = record.branch {
            target.add_text(fields.branch, branch);
        }
        target.add_text(fields.agent_type, record.agent_type);
        target.add_u64(fields.is_primary, u64::from(record.is_primary));
        target.add_u64(fields.event_sequence, record.event_sequence);
        if let Some(occurred_at_unix_ms) = record.occurred_at_unix_ms {
            target.add_i64(fields.occurred_at_unix_ms, occurred_at_unix_ms);
        }
        target.add_text(fields.event_type, record.event_type);
        if let Some(role) = record.role {
            target.add_text(fields.role, role);
        }
        if let Some(body) = project_indexed_body_search(&record.event_origin, record.content)? {
            target.add_text(fields.body_search, body);
        }
        for observation in record.repository_vcs_observations {
            if let RepositoryVcsObservationKind::Outcome(outcome) = observation.kind {
                for object_id in outcome.produced_object_ids {
                    target.add_text(fields.repository_produced_object_id, object_id.hex);
                }
            }
        }
        if let Some(workspace) = record.workspace {
            target.add_text(fields.workspace_filter, workspace.to_lowercase());
        }
        if let Some(cwd) = record.cwd {
            target.add_text(fields.workspace_filter, cwd.to_lowercase());
        }
        for observation in record.repository_file_observations {
            target.add_text(
                fields.touched_file_filter,
                observation.relative_path.to_lowercase(),
            );
            if let Some(prior_relative_path) = observation.prior_relative_path {
                target.add_text(
                    fields.touched_file_filter,
                    prior_relative_path.to_lowercase(),
                );
            }
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
        Ok(target)
    }
}

pub(super) struct IndexDocumentIter<'a>(slice::Iter<'a, (Field, IndexValue)>);

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
mod tests {
    use ctx_history_core::{
        derive_event_id, derive_session_id, CoreRecord, EventCopyProofKind, EventIdentityInput,
        NativeItemKey, NativeSessionKey, SessionIdentityInput, SourceAnchor, SourceKey, TypedKey,
    };
    use tantivy::schema::{Document, TantivyDocument};
    use tempfile::tempdir;

    use super::*;
    use crate::{fields_from_schema, lexical_schema, GenerationWriter, IndexError, WriterOptions};

    fn source(source_format: &str) -> SourceKey {
        SourceKey::derive(
            "codex",
            source_format,
            "session",
            1,
            SourceAnchor::provider_native(
                "session-file",
                TypedKey::utf8("move-backed-document-test").unwrap(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn core_record(source: &SourceKey) -> CoreRecord {
        let session_key =
            NativeSessionKey::native_id("session", TypedKey::utf8("session").unwrap()).unwrap();
        let session_id = derive_session_id(SessionIdentityInput {
            source,
            logical_session_kind: "thread",
            native_session_key: &session_key,
        })
        .unwrap();
        let native_item_key = NativeItemKey::native_id("message", TypedKey::U64(1)).unwrap();
        let event_id = derive_event_id(EventIdentityInput {
            source,
            session_id,
            logical_item_kind: "message",
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })
        .unwrap();
        let mut record = CoreRecord::new_selected(
            event_id,
            session_id,
            session_id,
            source.clone(),
            1,
            "message",
            "primary",
            true,
            "index-document-test-v1",
            "body",
        )
        .unwrap();
        record.native_event_id = Some(TypedKey::U64(1));
        record
    }

    #[test]
    fn move_backed_values_match_tantivy_document_field_semantics() {
        let schema = lexical_schema();
        let fields = fields_from_schema(&schema).unwrap();
        let body = "move-backed body".repeat(512);
        let body_pointer = body.as_ptr();
        let source = Arc::<str>::from("shared-source-token");
        let source_pointer = source.as_ptr();
        let bytes = vec![7_u8; 113];
        let bytes_pointer = bytes.as_ptr();

        let mut actual = IndexDocument::with_capacity(7);
        actual.add_text(fields.body_search, body);
        actual.add_shared_text(fields.source_key, Arc::clone(&source));
        actual.add_bytes(fields.core_record, bytes);
        actual.add_u64(fields.event_sequence, 42);
        actual.add_i64(fields.occurred_at_unix_ms, -9);
        actual.add_text(fields.touched_file_filter, "first.rs".to_owned());
        actual.add_text(fields.touched_file_filter, "second.rs".to_owned());

        assert!(actual.fields.iter().any(|(field, value)| {
            *field == fields.body_search
                && matches!(value, IndexValue::Text(value) if value.as_ptr() == body_pointer)
        }));
        assert!(actual.fields.iter().any(|(field, value)| {
            *field == fields.source_key
                && matches!(value, IndexValue::SharedText(value) if value.as_ptr() == source_pointer)
        }));
        assert!(actual.fields.iter().any(|(field, value)| {
            *field == fields.core_record
                && matches!(value, IndexValue::Bytes(value) if value.as_ptr() == bytes_pointer)
        }));

        let mut expected = TantivyDocument::default();
        expected.add_text(fields.body_search, "move-backed body".repeat(512));
        expected.add_text(fields.source_key, source.as_ref());
        expected.add_bytes(fields.core_record, &[7_u8; 113]);
        expected.add_u64(fields.event_sequence, 42);
        expected.add_i64(fields.occurred_at_unix_ms, -9);
        expected.add_text(fields.touched_file_filter, "first.rs");
        expected.add_text(fields.touched_file_filter, "second.rs");

        assert_eq!(
            serde_json::to_value(actual.to_named_doc(&schema)).unwrap(),
            serde_json::to_value(expected.to_named_doc(&schema)).unwrap()
        );
    }

    #[test]
    fn stack_source_token_matches_the_persisted_token_encoding() {
        let digest = [0xa5; 32];
        let token = SourceToken::new(&digest);
        assert_eq!(token.as_str().unwrap(), crate::hex(&digest));
    }

    #[test]
    fn core_content_accounting_preserves_the_index_maximum_for_direct_callers() {
        let source = source("codex_session_jsonl");
        let mut record = core_record(&source);
        record.content.normalized_body = Some("x".repeat(MAX_CORE_CONTENT_BYTES + 1));

        assert!(matches!(
            core_content_bytes(&record.content),
            Err(IndexError::DocumentFieldTooLarge {
                field: "core_content",
                actual,
                maximum: MAX_CORE_CONTENT_BYTES,
            }) if actual == MAX_CORE_CONTENT_BYTES + 1
        ));
    }

    #[test]
    fn copied_core_keeps_exact_fields_and_stored_body_without_body_search() {
        let schema = lexical_schema();
        let fields = fields_from_schema(&schema).unwrap();
        let source = source("codex_session_jsonl");
        let mut record = core_record(&source);
        let ancestor_session_key =
            NativeSessionKey::native_id("session", TypedKey::utf8("ancestor-session").unwrap())
                .unwrap();
        let ancestor_session_id = derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "thread",
            native_session_key: &ancestor_session_key,
        })
        .unwrap();
        let ancestor_item_key = NativeItemKey::native_id("message", TypedKey::U64(9)).unwrap();
        let ancestor_event_id = derive_event_id(EventIdentityInput {
            source: &source,
            session_id: ancestor_session_id,
            logical_item_kind: "message",
            native_item_key: &ancestor_item_key,
            subrecord_selector: None,
        })
        .unwrap();
        record.event_origin = EventOrigin::CopiedFromAncestor {
            ancestor_session_id,
            ancestor_event_id,
            proof: EventCopyProofKind::NativeCopiedFromField,
        };
        let expected_event_id = record.event_id.to_string();
        let expected_session_id = record.session_id.to_string();
        let expected_body = record.content.normalized_body.clone();
        let encoded = record.encode_stored().unwrap();
        let content_bytes = core_content_bytes(&record.content).unwrap();
        let source_fields = IndexSourceFields::new(&source, &crate::source_token(&source));

        let document =
            IndexDocument::from_core(fields, record, encoded, content_bytes, source_fields)
                .unwrap()
                .into_tantivy_document();

        assert!(document.get_first(fields.body_search).is_none());
        assert_eq!(
            document
                .get_first(fields.event_id)
                .and_then(|value| value.as_str()),
            Some(expected_event_id.as_str())
        );
        assert_eq!(
            document
                .get_first(fields.session_id)
                .and_then(|value| value.as_str()),
            Some(expected_session_id.as_str())
        );
        assert_eq!(
            document
                .get_first(fields.event_origin_kind)
                .and_then(|value| value.as_str()),
            Some("copied_from_ancestor")
        );
        let stored = document
            .get_first(fields.core_record)
            .and_then(|value| value.as_bytes())
            .map(CoreRecord::decode_stored)
            .unwrap()
            .unwrap();
        assert_eq!(stored.content.normalized_body, expected_body);
    }

    #[test]
    fn source_event_order_key_has_exact_source_order_and_size_layout() {
        let source = source("codex_session_jsonl");
        let record = core_record(&source);
        let core_record_bytes = record.encode_stored().unwrap();
        let content_bytes = core_content_bytes(&record.content).unwrap();
        let index_source = IndexSourceFields::new(&source, &crate::source_token(&source));
        let key = SourceEventOrderKey::for_document(
            &index_source,
            record.event_id.digest(),
            core_record_bytes.len(),
            content_bytes,
        )
        .unwrap()
        .into_bytes();

        assert_eq!(&key[..32], &source.identity().digest());
        assert_eq!(
            &key[32..SOURCE_EVENT_ORDER_SOURCE_PREFIX_LEN],
            &source.exact_descriptor_digest()
        );
        assert_eq!(
            &key[SOURCE_EVENT_ORDER_EVENT_DIGEST_OFFSET..SOURCE_EVENT_ORDER_ENCODED_BYTES_OFFSET],
            &record.event_id.digest()
        );
        assert_eq!(
            u32::from_be_bytes(
                key[SOURCE_EVENT_ORDER_ENCODED_BYTES_OFFSET
                    ..SOURCE_EVENT_ORDER_CONTENT_BYTES_OFFSET]
                    .try_into()
                    .unwrap()
            ) as usize,
            core_record_bytes.len()
        );
        assert_eq!(
            u32::from_be_bytes(
                key[SOURCE_EVENT_ORDER_CONTENT_BYTES_OFFSET..]
                    .try_into()
                    .unwrap()
            ) as usize,
            content_bytes
        );
    }

    #[test]
    fn session_event_order_key_matches_deterministic_session_coordinates() {
        let source = source("codex_session_jsonl");
        let mut record = core_record(&source);
        record.event_sequence = 42;
        record.occurred_at_unix_ms = Some(-9);
        let key = SessionEventOrderKey::for_core_record(&record).unwrap();

        assert_eq!(
            &key.as_bytes()[..SESSION_EVENT_ORDER_SESSION_PREFIX_LEN],
            &record.session_id.encode_canonical().unwrap()
        );
        assert_eq!(key.event_sequence(), 42);
        assert_eq!(key.occurred_at_unix_ms(), Some(-9));
        assert_eq!(key.event_id(), record.event_id.as_uuid());
        assert!(
            SessionEventOrderKey::session_range_end(record.session_id)
                .unwrap()
                .as_slice()
                > key.as_bytes().as_slice()
        );
    }

    #[test]
    fn cached_source_descriptor_preserves_core_record_and_active_source_faults() {
        let active = source("codex_session_jsonl");
        let descriptor_alias = source("codex_prompt_history_jsonl");
        assert_eq!(active, descriptor_alias);
        assert!(!active.exact_descriptor_eq(&descriptor_alias));

        let directory = tempdir().unwrap();
        let mut writer = GenerationWriter::open(directory.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        writer.begin_source(active.clone()).unwrap();

        let mut mismatched_identity = core_record(&active);
        mismatched_identity.event_id = mismatched_identity.session_id;
        assert!(matches!(
            writer.add_core_record(mismatched_identity),
            Err(IndexError::CoreRecord(_))
        ));
        assert!(matches!(
            writer.add_core_record(core_record(&descriptor_alias)),
            Err(IndexError::DocumentSourceNotActive)
        ));
    }
}
