use std::fmt;

#[cfg(test)]
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use ctx_history_core::CaptureProvider;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub(crate) mod jsonl;
pub(crate) mod sqlite_logical_rows;
pub(crate) mod whole_json;

pub(crate) const CAPTURE_BATCH_MAX_RECORDS: usize = 64;
pub(crate) const CAPTURE_BATCH_MAX_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const CAPTURE_BATCH_MAX_BATCHES_PER_GROUP: usize = 4;
pub(crate) const CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES: usize =
    crate::MAX_PROVIDER_JSONL_LINE_BYTES;
pub(crate) const CAPTURE_BATCH_MAX_PARSER_CHECKPOINT_BYTES: usize = 256 * 1024;

const MAX_SOURCE_FORMAT_BYTES: usize = 256;
const MAX_SOURCE_IDENTITY_BYTES: usize = 16 * 1024;
const MAX_SOURCE_REVISION_BYTES: usize = 4 * 1024;
const MAX_SOURCE_CURSOR_STREAM_BYTES: usize = 16 * 1024;
const MAX_NATIVE_KIND_BYTES: usize = 256;
const MAX_NATIVE_POSITION_BYTES: usize = 256 * 1024;
const MAX_NATIVE_LOCATOR_BYTES: usize = 64 * 1024;
const MAX_RECORD_KIND_BYTES: usize = 256;
const MAX_SQLITE_VALUES_PER_RECORD: usize = 256;
const INVENTORY_OBSERVATION_REVISION_DOMAIN: &[u8] = b"ctx-inventory-observed-source-revision-v1\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapturedDataClassification {
    LocalPrivate,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SourceObservation {
    provider: CaptureProvider,
    source_format: String,
    source_identity: String,
    source_revision: String,
    cursor_stream: String,
    capture_revision: u32,
    policy_revision: u32,
    inventory_observation_token: Option<String>,
}

impl SourceObservation {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        provider: CaptureProvider,
        source_format: impl Into<String>,
        source_identity: impl Into<String>,
        source_revision: impl Into<String>,
        cursor_stream: impl Into<String>,
        capture_revision: u32,
        policy_revision: u32,
        inventory_observation_token: Option<&str>,
    ) -> Result<Self, CapturedBatchError> {
        let observation = Self {
            provider,
            source_format: source_format.into(),
            source_identity: source_identity.into(),
            source_revision: source_revision.into(),
            cursor_stream: cursor_stream.into(),
            capture_revision,
            policy_revision,
            inventory_observation_token: None,
        }
        .with_inventory_observation_token(inventory_observation_token);
        validate_text(
            "source_format",
            &observation.source_format,
            MAX_SOURCE_FORMAT_BYTES,
        )?;
        validate_text(
            "source_identity",
            &observation.source_identity,
            MAX_SOURCE_IDENTITY_BYTES,
        )?;
        validate_text(
            "source_revision",
            &observation.source_revision,
            MAX_SOURCE_REVISION_BYTES,
        )?;
        validate_text(
            "cursor_stream",
            &observation.cursor_stream,
            MAX_SOURCE_CURSOR_STREAM_BYTES,
        )?;
        Ok(observation)
    }

    pub(crate) fn provider(&self) -> CaptureProvider {
        self.provider
    }

    pub(crate) fn source_format(&self) -> &str {
        &self.source_format
    }

    pub(crate) fn source_identity(&self) -> &str {
        &self.source_identity
    }

    pub(crate) fn source_revision(&self) -> &str {
        &self.source_revision
    }

    pub(crate) fn cursor_stream(&self) -> &str {
        &self.cursor_stream
    }

    pub(crate) fn capture_revision(&self) -> u32 {
        self.capture_revision
    }

    pub(crate) fn policy_revision(&self) -> u32 {
        self.policy_revision
    }

    pub(crate) fn inventory_observation_token(&self) -> Option<&str> {
        self.inventory_observation_token.as_deref()
    }

    /// Binds the provider's native source revision to the bounded observation
    /// that admitted this exact import unit.
    ///
    /// The provider revision still drives append verification and parser
    /// compatibility. The inventory token closes the gap where a same-size
    /// rewrite with a restored mtime would otherwise compare equal before the
    /// provider positioned its producer at the certified cursor.
    pub(crate) fn with_inventory_observation_token(mut self, token: Option<&str>) -> Self {
        let Some(token) = token else {
            return self;
        };
        let mut hasher = Sha256::new();
        hasher.update(INVENTORY_OBSERVATION_REVISION_DOMAIN);
        update_length_prefixed(&mut hasher, self.source_revision.as_bytes());
        update_length_prefixed(&mut hasher, token.as_bytes());
        self.inventory_observation_token = Some(token.to_owned());
        self.source_revision = format!(
            "inventory-observation-sha256-v1:{}",
            hasher
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        self
    }
}

fn update_length_prefixed(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

impl fmt::Debug for SourceObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceObservation")
            .field("provider", &self.provider)
            .field("source_format", &self.source_format)
            .field("source_identity_bytes", &self.source_identity.len())
            .field("source_revision_bytes", &self.source_revision.len())
            .field("cursor_stream_bytes", &self.cursor_stream.len())
            .field("capture_revision", &self.capture_revision)
            .field("policy_revision", &self.policy_revision)
            .field(
                "inventory_observation_token",
                &self.inventory_observation_token.as_ref().map(|_| "<token>"),
            )
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct NativePosition {
    kind: String,
    value: Vec<u8>,
}

impl NativePosition {
    pub(crate) fn new(kind: impl Into<String>, value: Vec<u8>) -> Result<Self, CapturedBatchError> {
        let position = Self {
            kind: kind.into(),
            value,
        };
        validate_text("position_kind", &position.kind, MAX_NATIVE_KIND_BYTES)?;
        validate_bytes("position_value", &position.value, MAX_NATIVE_POSITION_BYTES)?;
        Ok(position)
    }

    pub(crate) fn kind(&self) -> &str {
        &self.kind
    }

    pub(crate) fn value(&self) -> &[u8] {
        &self.value
    }
}

impl fmt::Debug for NativePosition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativePosition")
            .field("kind", &self.kind)
            .field("value_bytes", &self.value.len())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct NativeLocator {
    kind: String,
    value: Vec<u8>,
}

impl NativeLocator {
    pub(crate) fn new(kind: impl Into<String>, value: Vec<u8>) -> Result<Self, CapturedBatchError> {
        let locator = Self {
            kind: kind.into(),
            value,
        };
        validate_text("locator_kind", &locator.kind, MAX_NATIVE_KIND_BYTES)?;
        validate_native_locator_value_len(locator.value.len())?;
        Ok(locator)
    }

    pub(crate) fn kind(&self) -> &str {
        &self.kind
    }

    pub(crate) fn value(&self) -> &[u8] {
        &self.value
    }
}

impl fmt::Debug for NativeLocator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeLocator")
            .field("kind", &self.kind)
            .field("value_bytes", &self.value.len())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderRecordKind(String);

impl ProviderRecordKind {
    pub(crate) fn new(value: impl Into<String>) -> Result<Self, CapturedBatchError> {
        let value = value.into();
        validate_text("record_kind", &value, MAX_RECORD_KIND_BYTES)?;
        Ok(Self(value))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StructuralRejectionKind {
    OversizeRecord,
}

#[derive(PartialEq, Eq)]
pub(crate) enum CapturedRecordPayload {
    NativeBytes(Vec<u8>),
    SqliteValues(Vec<CapturedSqliteValue>),
    StructuralRejection {
        kind: StructuralRejectionKind,
        observed_bytes: u64,
    },
}

impl CapturedRecordPayload {
    pub(crate) fn retained_bytes(&self) -> usize {
        match self {
            Self::NativeBytes(payload) => payload.len(),
            Self::SqliteValues(values) => values.iter().fold(0usize, |total, value| {
                total.saturating_add(value.retained_bytes())
            }),
            Self::StructuralRejection { .. } => 0,
        }
    }
}

impl fmt::Debug for CapturedRecordPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NativeBytes(payload) => formatter
                .debug_struct("NativeBytes")
                .field("bytes", &payload.len())
                .finish(),
            Self::SqliteValues(values) => formatter
                .debug_struct("SqliteValues")
                .field("values", &values.len())
                .field("retained_bytes", &self.retained_bytes())
                .finish(),
            Self::StructuralRejection {
                kind,
                observed_bytes,
            } => formatter
                .debug_struct("StructuralRejection")
                .field("kind", kind)
                .field("observed_bytes", observed_bytes)
                .finish(),
        }
    }
}

#[derive(PartialEq, Eq)]
pub(crate) enum CapturedSqliteValue {
    Null,
    Integer(i64),
    RealBits(u64),
    Text(String),
    Blob(Vec<u8>),
}

impl CapturedSqliteValue {
    pub(crate) fn from_real(value: f64) -> Self {
        Self::RealBits(value.to_bits())
    }

    pub(crate) fn as_real(&self) -> Option<f64> {
        match self {
            Self::RealBits(bits) => Some(f64::from_bits(*bits)),
            _ => None,
        }
    }

    fn retained_bytes(&self) -> usize {
        match self {
            Self::Null => 1,
            Self::Integer(_) | Self::RealBits(_) => 9,
            Self::Text(value) => value.len().saturating_add(5),
            Self::Blob(value) => value.len().saturating_add(5),
        }
    }
}

impl fmt::Debug for CapturedSqliteValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => formatter.write_str("Null"),
            Self::Integer(_) => formatter.write_str("Integer(<redacted>)"),
            Self::RealBits(_) => formatter.write_str("RealBits(<redacted>)"),
            Self::Text(value) => formatter
                .debug_struct("Text")
                .field("bytes", &value.len())
                .finish(),
            Self::Blob(value) => formatter
                .debug_struct("Blob")
                .field("bytes", &value.len())
                .finish(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CapturedRecord {
    ordinal: u64,
    locator: NativeLocator,
    record_kind: ProviderRecordKind,
    payload: CapturedRecordPayload,
}

impl CapturedRecord {
    pub(crate) fn content(
        ordinal: u64,
        locator: NativeLocator,
        record_kind: ProviderRecordKind,
        payload: Vec<u8>,
    ) -> Result<Self, CapturedBatchError> {
        if payload.len() > CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES {
            return Err(CapturedBatchError::RecordPayloadTooLarge {
                actual: payload.len(),
                maximum: CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES,
            });
        }
        Ok(Self {
            ordinal,
            locator,
            record_kind,
            payload: CapturedRecordPayload::NativeBytes(payload),
        })
    }

    pub(crate) fn sqlite_logical(
        ordinal: u64,
        locator: NativeLocator,
        record_kind: ProviderRecordKind,
        values: Vec<CapturedSqliteValue>,
    ) -> Result<Self, CapturedBatchError> {
        if values.len() > MAX_SQLITE_VALUES_PER_RECORD {
            return Err(CapturedBatchError::TooManySqliteValues {
                actual: values.len(),
                maximum: MAX_SQLITE_VALUES_PER_RECORD,
            });
        }
        let retained_bytes = values.iter().try_fold(0usize, |total, value| {
            total
                .checked_add(value.retained_bytes())
                .ok_or(CapturedBatchError::LengthOverflow)
        })?;
        if retained_bytes > CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES {
            return Err(CapturedBatchError::RecordPayloadTooLarge {
                actual: retained_bytes,
                maximum: CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES,
            });
        }
        Ok(Self {
            ordinal,
            locator,
            record_kind,
            payload: CapturedRecordPayload::SqliteValues(values),
        })
    }

    pub(crate) fn structural_rejection(
        ordinal: u64,
        locator: NativeLocator,
        record_kind: ProviderRecordKind,
        kind: StructuralRejectionKind,
        observed_bytes: u64,
    ) -> Self {
        Self {
            ordinal,
            locator,
            record_kind,
            payload: CapturedRecordPayload::StructuralRejection {
                kind,
                observed_bytes,
            },
        }
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        self.payload.retained_bytes()
    }

    pub(crate) fn ordinal(&self) -> u64 {
        self.ordinal
    }

    pub(crate) fn locator(&self) -> &NativeLocator {
        &self.locator
    }

    pub(crate) fn record_kind(&self) -> &ProviderRecordKind {
        &self.record_kind
    }

    pub(crate) fn payload(&self) -> &CapturedRecordPayload {
        &self.payload
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CapturedBatch {
    classification: CapturedDataClassification,
    source: SourceObservation,
    range_before: NativePosition,
    range_end: NativePosition,
    records: Vec<CapturedRecord>,
    retained_payload_bytes: usize,
    // True only when the producer proved that this observation has no following batch while
    // constructing this delivery. The importer uses the proof instead of probing past a live raw
    // batch merely to discover EOF.
    source_exhausted: bool,
    #[cfg(test)]
    drop_observer: Option<CapturedBatchDropObserver>,
}

impl CapturedBatch {
    pub(crate) fn classification(&self) -> CapturedDataClassification {
        self.classification
    }

    pub(crate) fn source(&self) -> &SourceObservation {
        &self.source
    }

    pub(crate) fn range_before(&self) -> &NativePosition {
        &self.range_before
    }

    pub(crate) fn range_end(&self) -> &NativePosition {
        &self.range_end
    }

    pub(crate) fn records(&self) -> &[CapturedRecord] {
        &self.records
    }

    pub(crate) fn retained_payload_bytes(&self) -> usize {
        self.retained_payload_bytes
    }

    pub(crate) fn source_exhausted(&self) -> bool {
        self.source_exhausted
    }

    #[cfg(test)]
    pub(crate) fn into_source_exhausted(mut self) -> Self {
        self.source_exhausted = true;
        self
    }

    pub(crate) fn into_source_continues(mut self) -> Self {
        // Composite producers use this when a final inner batch is followed by their own record.
        self.source_exhausted = false;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_drop_observer(mut self, observer: CapturedBatchDropObserver) -> Self {
        self.drop_observer = Some(observer);
        self
    }
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct CapturedBatchDropObserver(Arc<AtomicUsize>);

#[cfg(test)]
impl CapturedBatchDropObserver {
    pub(crate) fn new() -> Self {
        Self(Arc::new(AtomicUsize::new(0)))
    }

    pub(crate) fn observed_drops(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
impl PartialEq for CapturedBatchDropObserver {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[cfg(test)]
impl Eq for CapturedBatchDropObserver {}

#[cfg(test)]
impl Drop for CapturedBatch {
    fn drop(&mut self) {
        if let Some(observer) = &self.drop_observer {
            observer.0.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[derive(Debug)]
pub(crate) struct CapturedBatchBuilder {
    source: SourceObservation,
    range_before: NativePosition,
    records: Vec<CapturedRecord>,
    retained_payload_bytes: usize,
    source_exhausted: bool,
}

impl CapturedBatchBuilder {
    pub(crate) fn new(source: SourceObservation, range_before: NativePosition) -> Self {
        Self {
            source,
            range_before,
            records: Vec::new(),
            retained_payload_bytes: 0,
            source_exhausted: false,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub(crate) fn record_count(&self) -> usize {
        self.records.len()
    }

    pub(crate) fn retained_payload_bytes(&self) -> usize {
        self.retained_payload_bytes
    }

    pub(crate) fn mark_source_exhausted(&mut self) {
        self.source_exhausted = true;
    }

    pub(crate) fn can_accept(&self, record: &CapturedRecord) -> bool {
        if self.records.len() >= CAPTURE_BATCH_MAX_RECORDS {
            return false;
        }
        let record_bytes = record.retained_bytes();
        if record_bytes > CAPTURE_BATCH_MAX_PAYLOAD_BYTES {
            return self.records.is_empty()
                && record_bytes <= CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES;
        }
        self.retained_payload_bytes
            .checked_add(record_bytes)
            .is_some_and(|total| total <= CAPTURE_BATCH_MAX_PAYLOAD_BYTES)
    }

    pub(crate) fn push(&mut self, record: CapturedRecord) -> Result<(), CapturedBatchError> {
        if !self.can_accept(&record) {
            return Err(CapturedBatchError::BatchFull);
        }
        self.retained_payload_bytes = self
            .retained_payload_bytes
            .checked_add(record.retained_bytes())
            .ok_or(CapturedBatchError::LengthOverflow)?;
        self.records.push(record);
        Ok(())
    }

    pub(crate) fn finish(
        self,
        range_end: NativePosition,
    ) -> Result<CapturedBatch, CapturedBatchError> {
        if self.records.is_empty() {
            return Err(CapturedBatchError::EmptyBatch);
        }
        for pair in self.records.windows(2) {
            if pair[0].ordinal >= pair[1].ordinal {
                return Err(CapturedBatchError::NonIncreasingOrdinals);
            }
        }
        Ok(CapturedBatch {
            classification: CapturedDataClassification::LocalPrivate,
            source: self.source,
            range_before: self.range_before,
            range_end,
            records: self.records,
            retained_payload_bytes: self.retained_payload_bytes,
            source_exhausted: self.source_exhausted,
            #[cfg(test)]
            drop_observer: None,
        })
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum CapturedBatchError {
    #[error("captured batch field {field} is empty")]
    EmptyField { field: &'static str },
    #[error("captured batch field {field} is too large: {actual} bytes, maximum {maximum}")]
    FieldTooLarge {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("captured record payload is too large: {actual} bytes, maximum {maximum}")]
    RecordPayloadTooLarge { actual: usize, maximum: usize },
    #[error("captured SQLite logical record has {actual} values, maximum {maximum}")]
    TooManySqliteValues { actual: usize, maximum: usize },
    #[error("captured batch reached its fixed record or payload bound")]
    BatchFull,
    #[error("captured batch cannot be empty")]
    EmptyBatch,
    #[error("captured batch record ordinals must be strictly increasing")]
    NonIncreasingOrdinals,
    #[error("captured batch length overflow")]
    LengthOverflow,
}

fn validate_text(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), CapturedBatchError> {
    if value.is_empty() {
        return Err(CapturedBatchError::EmptyField { field });
    }
    validate_bytes(field, value.as_bytes(), maximum)
}

fn validate_bytes(
    field: &'static str,
    value: &[u8],
    maximum: usize,
) -> Result<(), CapturedBatchError> {
    if value.len() > maximum {
        return Err(CapturedBatchError::FieldTooLarge {
            field,
            actual: value.len(),
            maximum,
        });
    }
    Ok(())
}

fn validate_native_locator_value_len(actual: usize) -> Result<(), CapturedBatchError> {
    if actual > MAX_NATIVE_LOCATOR_BYTES {
        return Err(CapturedBatchError::FieldTooLarge {
            field: "locator_value",
            actual,
            maximum: MAX_NATIVE_LOCATOR_BYTES,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation() -> SourceObservation {
        SourceObservation::new(
            CaptureProvider::Codex,
            "codex_session_jsonl",
            "session:abc",
            "size:42;tail:abcd",
            "provider:codex:codex_session_jsonl:source:test",
            1,
            1,
            None,
        )
        .unwrap()
    }

    fn position(value: u64) -> NativePosition {
        NativePosition::new("jsonl-byte", value.to_be_bytes().to_vec()).unwrap()
    }

    fn record(ordinal: u64, bytes: usize) -> CapturedRecord {
        CapturedRecord::content(
            ordinal,
            NativeLocator::new(
                "jsonl-range",
                [b"session.jsonl:".as_slice(), ordinal.to_string().as_bytes()].concat(),
            )
            .unwrap(),
            ProviderRecordKind::new("codex-jsonl-v1").unwrap(),
            vec![b'x'; bytes],
        )
        .unwrap()
    }

    #[test]
    fn fixed_partition_accepts_normal_records() {
        let mut builder = CapturedBatchBuilder::new(observation(), position(0));
        builder.push(record(1, 4)).unwrap();
        builder.push(record(2, 5)).unwrap();
        let batch = builder.finish(position(9)).unwrap();

        assert_eq!(
            batch.classification(),
            CapturedDataClassification::LocalPrivate
        );
        assert_eq!(batch.records().len(), 2);
        assert_eq!(batch.retained_payload_bytes(), 9);
    }

    #[test]
    fn batch_boundary_does_not_accept_a_sixty_fifth_record() {
        let mut builder = CapturedBatchBuilder::new(observation(), position(0));
        for ordinal in 0..CAPTURE_BATCH_MAX_RECORDS as u64 {
            builder.push(record(ordinal, 0)).unwrap();
        }

        assert!(!builder.can_accept(&record(CAPTURE_BATCH_MAX_RECORDS as u64, 0)));
        assert_eq!(
            builder
                .push(record(CAPTURE_BATCH_MAX_RECORDS as u64, 0))
                .unwrap_err(),
            CapturedBatchError::BatchFull
        );
    }

    #[test]
    fn oversize_record_must_be_a_singleton() {
        let mut singleton = CapturedBatchBuilder::new(observation(), position(0));
        singleton
            .push(record(0, CAPTURE_BATCH_MAX_PAYLOAD_BYTES + 1))
            .unwrap();
        assert!(!singleton.can_accept(&record(1, 0)));

        let mut normal = CapturedBatchBuilder::new(observation(), position(0));
        normal.push(record(0, 1)).unwrap();
        assert!(!normal.can_accept(&record(1, CAPTURE_BATCH_MAX_PAYLOAD_BYTES + 1)));
    }

    #[test]
    fn structural_rejection_advances_without_retaining_payload() {
        let rejection = CapturedRecord::structural_rejection(
            9,
            NativeLocator::new("jsonl-range", b"session.jsonl:9".to_vec()).unwrap(),
            ProviderRecordKind::new("codex-jsonl-v1").unwrap(),
            StructuralRejectionKind::OversizeRecord,
            (CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES as u64) + 1,
        );
        let mut builder = CapturedBatchBuilder::new(observation(), position(0));
        builder.push(rejection).unwrap();
        let batch = builder.finish(position(1)).unwrap();

        assert_eq!(batch.retained_payload_bytes(), 0);
        assert!(matches!(
            batch.records()[0].payload(),
            CapturedRecordPayload::StructuralRejection { .. }
        ));
    }

    #[test]
    fn sqlite_logical_values_preserve_storage_types_and_real_bits() {
        let values = vec![
            CapturedSqliteValue::Null,
            CapturedSqliteValue::Integer(-7),
            CapturedSqliteValue::from_real(-0.0),
            CapturedSqliteValue::Text("hello".to_owned()),
            CapturedSqliteValue::Blob(vec![0, 255]),
        ];
        let record = CapturedRecord::sqlite_logical(
            0,
            NativeLocator::new("sqlite-primary-key-v1", b"messages:7".to_vec()).unwrap(),
            ProviderRecordKind::new("opencode-message-row-v1").unwrap(),
            values,
        )
        .unwrap();

        let CapturedRecordPayload::SqliteValues(actual) = record.payload() else {
            panic!("expected SQLite logical payload");
        };
        assert!(matches!(&actual[0], CapturedSqliteValue::Null));
        assert!(matches!(&actual[1], CapturedSqliteValue::Integer(-7)));
        assert_eq!(actual[2].as_real().unwrap().to_bits(), (-0.0_f64).to_bits());
        assert!(matches!(
            &actual[3],
            CapturedSqliteValue::Text(value) if value == "hello"
        ));
        assert!(matches!(
            &actual[4],
            CapturedSqliteValue::Blob(value) if value.as_slice() == [0, 255]
        ));
    }

    #[test]
    fn builder_rejects_non_increasing_ordinals() {
        let mut builder = CapturedBatchBuilder::new(observation(), position(0));
        builder.push(record(2, 0)).unwrap();
        builder.push(record(1, 0)).unwrap();

        assert_eq!(
            builder.finish(position(2)).unwrap_err(),
            CapturedBatchError::NonIncreasingOrdinals
        );
    }

    #[test]
    fn immutable_accessors_expose_captured_invariants() {
        let mut builder = CapturedBatchBuilder::new(observation(), position(0));
        builder.push(record(7, 3)).unwrap();
        let batch = builder.finish(position(3)).unwrap();

        assert_eq!(batch.source().provider(), CaptureProvider::Codex);
        assert_eq!(batch.source().source_format(), "codex_session_jsonl");
        assert_eq!(batch.source().source_identity(), "session:abc");
        assert_eq!(batch.source().source_revision(), "size:42;tail:abcd");
        assert_eq!(batch.source().capture_revision(), 1);
        assert_eq!(batch.source().policy_revision(), 1);
        assert_eq!(batch.range_before().kind(), "jsonl-byte");
        assert_eq!(batch.range_before().value(), 0_u64.to_be_bytes());
        assert_eq!(batch.range_end().kind(), "jsonl-byte");
        assert_eq!(batch.range_end().value(), 3_u64.to_be_bytes());

        let captured = &batch.records()[0];
        assert_eq!(captured.ordinal(), 7);
        assert_eq!(captured.locator().kind(), "jsonl-range");
        assert_eq!(captured.locator().value(), b"session.jsonl:7");
        assert_eq!(captured.record_kind().as_str(), "codex-jsonl-v1");
        assert!(matches!(
            captured.payload(),
            CapturedRecordPayload::NativeBytes(payload) if payload == b"xxx"
        ));
    }

    #[test]
    fn debug_redacts_native_and_sqlite_transcript_values() {
        let transcript = b"debug-transcript-secret".to_vec();
        let transcript_bytes = format!("{transcript:?}");
        let mut builder = CapturedBatchBuilder::new(observation(), position(0));
        builder
            .push(
                CapturedRecord::content(
                    0,
                    NativeLocator::new("jsonl-range", b"private-source-item".to_vec()).unwrap(),
                    ProviderRecordKind::new("codex-jsonl-v1").unwrap(),
                    transcript,
                )
                .unwrap(),
            )
            .unwrap();
        let debug = format!("{:?}", builder.finish(position(1)).unwrap());

        assert!(debug.contains("NativeBytes"));
        assert!(debug.contains("codex_session_jsonl"));
        assert!(!debug.contains("session:abc"));
        assert!(!debug.contains("size:42;tail:abcd"));
        assert!(!debug.contains("private-source-item"));
        assert!(!debug.contains(&transcript_bytes));

        let sqlite = CapturedRecord::sqlite_logical(
            0,
            NativeLocator::new("sqlite-primary-key-v1", b"messages:7".to_vec()).unwrap(),
            ProviderRecordKind::new("opencode-message-row-v1").unwrap(),
            vec![
                CapturedSqliteValue::Integer(8675309),
                CapturedSqliteValue::Text("sqlite-transcript-secret".to_owned()),
                CapturedSqliteValue::Blob(b"sqlite-blob-secret".to_vec()),
            ],
        )
        .unwrap();
        let debug = format!("{sqlite:?}");

        assert!(debug.contains("SqliteValues"));
        assert!(!debug.contains("8675309"));
        assert!(!debug.contains("sqlite-transcript-secret"));
        assert!(!debug.contains(&format!("{:?}", b"sqlite-blob-secret")));
    }
}
