use std::{
    io::{self, BufRead, Seek, SeekFrom},
    mem::size_of,
};

use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    validate_native_locator_value_len, CapturedBatch, CapturedBatchBuilder, CapturedBatchError,
    CapturedRecord, NativeLocator, NativePosition, ProviderRecordKind, SourceObservation,
    StructuralRejectionKind, CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES,
    CAPTURE_BATCH_MAX_PAYLOAD_BYTES, CAPTURE_BATCH_MAX_RECORDS,
};

const JSONL_POSITION_KIND: &str = "jsonl-byte-boundary-v1";
const JSONL_LOCATOR_KIND: &str = "jsonl-source-item-byte-range-v1";
const JSONL_BOUNDARY_MAX_BYTES: usize = 64 * 1024;
const JSONL_POSITION_MAGIC: &[u8; 8] = b"CTXJLBP\0";
const JSONL_POSITION_ENCODING_VERSION: u8 = 1;
const JSONL_POSITION_HASH_SHA256: u8 = 1;
const JSONL_POSITION_RESERVED_BYTES: usize = 2;
const JSONL_POSITION_OFFSET_START: usize =
    JSONL_POSITION_MAGIC.len() + size_of::<u8>() + size_of::<u8>() + JSONL_POSITION_RESERVED_BYTES;
const JSONL_POSITION_PROOF_LENGTH_START: usize = JSONL_POSITION_OFFSET_START + size_of::<u64>();
const JSONL_POSITION_DIGEST_START: usize = JSONL_POSITION_PROOF_LENGTH_START + size_of::<u32>();
const JSONL_POSITION_ENCODED_BYTES: usize = JSONL_POSITION_DIGEST_START + 32;
const JSONL_BOUNDARY_HASH_DOMAIN: &[u8] = b"ctx-jsonl-append-boundary-sha256-v1\0";
const JSONL_VERIFIED_APPEND_DOMAIN: &[u8] = b"ctx-jsonl-verified-append-sha256-v1\0";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct VerifiedJsonlAppend {
    boundary: u64,
    observed_length: u64,
    binding: [u8; 32],
}

impl VerifiedJsonlAppend {
    pub(crate) fn validates(
        &self,
        earlier_position: &NativePosition,
        current_source: &SourceObservation,
    ) -> bool {
        let Ok(earlier) = decode_jsonl_position(earlier_position) else {
            return false;
        };
        earlier.offset == self.boundary
            && verified_append_binding(earlier_position, current_source, self.observed_length)
                == self.binding
    }
}

impl std::fmt::Debug for VerifiedJsonlAppend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VerifiedJsonlAppend")
            .field("boundary", &self.boundary)
            .field("observed_length", &self.observed_length)
            .finish_non_exhaustive()
    }
}

pub(crate) struct JsonlBatchProducer<R> {
    reader: R,
    source: SourceObservation,
    source_item: Vec<u8>,
    record_kind: ProviderRecordKind,
    observation_end: u64,
    emitted_end: u64,
    emitted_position: NativePosition,
    reader_offset: u64,
    next_ordinal: u64,
    allow_unterminated_final_record: bool,
    max_record_bytes: usize,
    max_batch_payload_bytes: usize,
    poisoned: bool,
}

struct PendingRecord {
    record: CapturedRecord,
    range_end: u64,
}

enum ReadRecordOutcome {
    Record(PendingRecord),
    Deferred,
    End,
}

impl<R: BufRead + Seek> JsonlBatchProducer<R> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        mut reader: R,
        source: SourceObservation,
        source_item: Vec<u8>,
        record_kind: ProviderRecordKind,
        observation_end: u64,
        start_offset: u64,
        start_ordinal: u64,
        allow_unterminated_final_record: bool,
    ) -> Result<Self, JsonlBatchError> {
        if start_offset > observation_end {
            return Err(JsonlBatchError::InvalidRange {
                start: start_offset,
                end: observation_end,
            });
        }
        validate_jsonl_source_item(&source_item)?;
        let emitted_position = jsonl_position_at(&mut reader, start_offset)?;
        Ok(Self {
            reader,
            source,
            source_item,
            record_kind,
            observation_end,
            emitted_end: start_offset,
            emitted_position,
            reader_offset: start_offset,
            next_ordinal: start_ordinal,
            allow_unterminated_final_record,
            max_record_bytes: CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES,
            max_batch_payload_bytes: CAPTURE_BATCH_MAX_PAYLOAD_BYTES,
            poisoned: false,
        })
    }

    #[cfg(test)]
    fn with_max_record_bytes(mut self, maximum: usize) -> Self {
        self.max_record_bytes = maximum;
        self
    }

    #[cfg(test)]
    fn with_max_batch_payload_bytes(mut self, maximum: usize) -> Self {
        self.max_batch_payload_bytes = maximum;
        self
    }

    pub(crate) fn next_batch(&mut self) -> Result<Option<CapturedBatch>, JsonlBatchError> {
        if self.poisoned {
            return Err(JsonlBatchError::ProducerPoisoned);
        }

        let operation_start = self.emitted_end;
        let result = self.next_batch_unpoisoned();
        if result.is_err() && self.reader_offset != operation_start {
            self.poisoned = true;
        }
        result
    }

    pub(crate) fn current_position(&self) -> &NativePosition {
        &self.emitted_position
    }

    fn next_batch_unpoisoned(&mut self) -> Result<Option<CapturedBatch>, JsonlBatchError> {
        let mut range_end = self.emitted_end;
        let mut builder =
            CapturedBatchBuilder::new(self.source.clone(), self.emitted_position.clone());
        let mut source_exhausted = false;

        loop {
            if builder.record_count() >= CAPTURE_BATCH_MAX_RECORDS
                || builder.retained_payload_bytes() >= self.max_batch_payload_bytes
            {
                break;
            }
            let retention_limit = (!builder.is_empty()).then(|| {
                self.max_batch_payload_bytes
                    .saturating_sub(builder.retained_payload_bytes())
            });
            let pending = match self.read_record(retention_limit)? {
                ReadRecordOutcome::Record(pending) => pending,
                ReadRecordOutcome::Deferred => break,
                ReadRecordOutcome::End => {
                    source_exhausted = true;
                    break;
                }
            };
            if !builder.can_accept(&pending.record) {
                return Err(JsonlBatchError::Batch(CapturedBatchError::BatchFull));
            }
            range_end = pending.range_end;
            builder.push(pending.record)?;
        }

        if builder.is_empty() {
            return Ok(None);
        }
        if source_exhausted || self.reader_offset >= self.observation_end {
            builder.mark_source_exhausted();
        }
        let range_end_position = jsonl_position_at(&mut self.reader, range_end)?;
        let batch = builder.finish(range_end_position.clone())?;
        self.emitted_end = range_end;
        self.emitted_position = range_end_position;
        Ok(Some(batch))
    }

    fn read_record(
        &mut self,
        retention_limit: Option<usize>,
    ) -> Result<ReadRecordOutcome, JsonlBatchError> {
        if self.reader_offset >= self.observation_end {
            return Ok(ReadRecordOutcome::End);
        }
        let record_start = self.reader_offset;
        let ordinal = self.next_ordinal;
        let mut payload = Vec::new();
        let mut observed_bytes = 0_u64;
        let mut oversize = false;
        let mut deferred = false;
        let mut terminated = false;

        while self.reader_offset < self.observation_end {
            let available = self.reader.fill_buf()?;
            if available.is_empty() {
                return Err(JsonlBatchError::SourceChangedDuringRead {
                    expected: self.observation_end,
                    actual: self.reader_offset,
                });
            }
            let remaining =
                usize::try_from(self.observation_end - self.reader_offset).unwrap_or(usize::MAX);
            let available = &available[..available.len().min(remaining)];
            let newline = available.iter().position(|byte| *byte == b'\n');
            let consume = newline.map_or(available.len(), |index| index + 1);
            if !oversize && !deferred {
                let next_payload_bytes = payload.len().saturating_add(consume);
                if retention_limit.is_some_and(|limit| next_payload_bytes > limit) {
                    deferred = true;
                    payload.clear();
                } else if next_payload_bytes > self.max_record_bytes {
                    oversize = true;
                    payload.clear();
                } else {
                    payload.extend_from_slice(&available[..consume]);
                }
            }
            self.reader.consume(consume);
            self.reader_offset = self
                .reader_offset
                .checked_add(u64::try_from(consume).map_err(|_| JsonlBatchError::LengthOverflow)?)
                .ok_or(JsonlBatchError::LengthOverflow)?;
            observed_bytes = observed_bytes
                .checked_add(u64::try_from(consume).map_err(|_| JsonlBatchError::LengthOverflow)?)
                .ok_or(JsonlBatchError::LengthOverflow)?;
            if newline.is_some() {
                terminated = true;
                break;
            }
        }

        if observed_bytes == 0 {
            return Ok(ReadRecordOutcome::End);
        }
        if !terminated && !self.allow_unterminated_final_record {
            self.reader.seek(SeekFrom::Start(record_start))?;
            self.reader_offset = record_start;
            return Ok(ReadRecordOutcome::End);
        }
        if deferred {
            self.reader.seek(SeekFrom::Start(record_start))?;
            self.reader_offset = record_start;
            return Ok(ReadRecordOutcome::Deferred);
        }

        self.next_ordinal = self
            .next_ordinal
            .checked_add(1)
            .ok_or(JsonlBatchError::LengthOverflow)?;
        let locator = jsonl_locator(&self.source_item, record_start, self.reader_offset)?;
        let record = if oversize {
            CapturedRecord::structural_rejection(
                ordinal,
                locator,
                self.record_kind.clone(),
                StructuralRejectionKind::OversizeRecord,
                observed_bytes,
            )
        } else {
            if terminated {
                payload.pop();
                if payload.last() == Some(&b'\r') {
                    payload.pop();
                }
            }
            CapturedRecord::content(ordinal, locator, self.record_kind.clone(), payload)?
        };
        Ok(ReadRecordOutcome::Record(PendingRecord {
            record,
            range_end: self.reader_offset,
        }))
    }
}

pub(crate) fn initial_jsonl_position() -> Result<NativePosition, JsonlBatchError> {
    encode_jsonl_position(0, &[])
}

pub(crate) fn jsonl_position_offset(position: &NativePosition) -> Result<u64, JsonlBatchError> {
    Ok(decode_jsonl_position(position)?.offset)
}

pub(crate) fn jsonl_locator_range(locator: &NativeLocator) -> Result<(u64, u64), JsonlBatchError> {
    if locator.kind() != JSONL_LOCATOR_KIND {
        return Err(JsonlBatchError::UnknownLocatorKind {
            kind: locator.kind().to_owned(),
        });
    }
    let value = locator.value();
    let source_length_bytes: [u8; size_of::<u32>()] = value
        .get(..size_of::<u32>())
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(JsonlBatchError::MalformedLocator)?;
    let source_length = usize::try_from(u32::from_be_bytes(source_length_bytes))
        .map_err(|_| JsonlBatchError::LengthOverflow)?;
    let range_start = size_of::<u32>()
        .checked_add(source_length)
        .ok_or(JsonlBatchError::LengthOverflow)?;
    let expected_length = range_start
        .checked_add(2 * size_of::<u64>())
        .ok_or(JsonlBatchError::LengthOverflow)?;
    if value.len() != expected_length {
        return Err(JsonlBatchError::MalformedLocator);
    }
    let start = u64::from_be_bytes(
        value[range_start..range_start + size_of::<u64>()]
            .try_into()
            .map_err(|_| JsonlBatchError::MalformedLocator)?,
    );
    let end = u64::from_be_bytes(
        value[range_start + size_of::<u64>()..]
            .try_into()
            .map_err(|_| JsonlBatchError::MalformedLocator)?,
    );
    if start > end {
        return Err(JsonlBatchError::InvalidRange { start, end });
    }
    Ok((start, end))
}

pub(crate) fn verify_jsonl_append_boundary<R: BufRead + Seek>(
    reader: &mut R,
    earlier_position: &NativePosition,
    current_source: &SourceObservation,
    observed_length: u64,
) -> Result<VerifiedJsonlAppend, JsonlBatchError> {
    let earlier = decode_jsonl_position(earlier_position)?;
    if observed_length < earlier.offset {
        return Err(JsonlBatchError::AppendBoundaryTruncated {
            boundary: earlier.offset,
            observed_length,
        });
    }
    let current = jsonl_position_at(reader, earlier.offset)?;
    if current != *earlier_position {
        return Err(JsonlBatchError::AppendBoundaryMismatch {
            boundary: earlier.offset,
        });
    }
    Ok(VerifiedJsonlAppend {
        boundary: earlier.offset,
        observed_length,
        binding: verified_append_binding(earlier_position, current_source, observed_length),
    })
}

fn verified_append_binding(
    earlier_position: &NativePosition,
    current_source: &SourceObservation,
    observed_length: u64,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(JSONL_VERIFIED_APPEND_DOMAIN);
    update_length_prefixed(&mut hasher, current_source.provider().as_str().as_bytes());
    update_length_prefixed(&mut hasher, current_source.source_format().as_bytes());
    update_length_prefixed(&mut hasher, current_source.source_identity().as_bytes());
    update_length_prefixed(&mut hasher, current_source.source_revision().as_bytes());
    update_length_prefixed(&mut hasher, current_source.cursor_stream().as_bytes());
    hasher.update(current_source.capture_revision().to_be_bytes());
    hasher.update(current_source.policy_revision().to_be_bytes());
    update_length_prefixed(&mut hasher, earlier_position.kind().as_bytes());
    update_length_prefixed(&mut hasher, earlier_position.value());
    hasher.update(observed_length.to_be_bytes());
    hasher.finalize().into()
}

fn update_length_prefixed(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

struct DecodedJsonlPosition {
    offset: u64,
}

fn decode_jsonl_position(
    position: &NativePosition,
) -> Result<DecodedJsonlPosition, JsonlBatchError> {
    if position.kind() != JSONL_POSITION_KIND {
        return Err(JsonlBatchError::UnknownPositionKind {
            kind: position.kind().to_owned(),
        });
    }
    let value = position.value();
    if value.len() != JSONL_POSITION_ENCODED_BYTES {
        return Err(JsonlBatchError::MalformedPosition {
            reason: "invalid encoded length",
        });
    }
    if &value[..JSONL_POSITION_MAGIC.len()] != JSONL_POSITION_MAGIC {
        return Err(JsonlBatchError::MalformedPosition {
            reason: "invalid encoding domain",
        });
    }
    let version = value[JSONL_POSITION_MAGIC.len()];
    if version != JSONL_POSITION_ENCODING_VERSION {
        return Err(JsonlBatchError::UnknownPositionVersion { version });
    }
    let hash_algorithm = value[JSONL_POSITION_MAGIC.len() + size_of::<u8>()];
    if hash_algorithm != JSONL_POSITION_HASH_SHA256 {
        return Err(JsonlBatchError::UnknownPositionHashAlgorithm { hash_algorithm });
    }
    let reserved_start = JSONL_POSITION_MAGIC.len() + size_of::<u8>() + size_of::<u8>();
    if value[reserved_start..JSONL_POSITION_OFFSET_START]
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(JsonlBatchError::MalformedPosition {
            reason: "nonzero reserved bytes",
        });
    }

    let offset = u64::from_be_bytes(
        value[JSONL_POSITION_OFFSET_START..JSONL_POSITION_PROOF_LENGTH_START]
            .try_into()
            .map_err(|_| JsonlBatchError::MalformedPosition {
                reason: "invalid offset encoding",
            })?,
    );
    let proof_length = u32::from_be_bytes(
        value[JSONL_POSITION_PROOF_LENGTH_START..JSONL_POSITION_DIGEST_START]
            .try_into()
            .map_err(|_| JsonlBatchError::MalformedPosition {
                reason: "invalid proof length encoding",
            })?,
    );
    if u64::from(proof_length) != jsonl_boundary_proof_length(offset) {
        return Err(JsonlBatchError::MalformedPosition {
            reason: "noncanonical proof length",
        });
    }
    Ok(DecodedJsonlPosition { offset })
}

fn jsonl_position_at<R: BufRead + Seek>(
    reader: &mut R,
    offset: u64,
) -> Result<NativePosition, JsonlBatchError> {
    let proof_length = jsonl_boundary_proof_length(offset);
    let proof_length_usize =
        usize::try_from(proof_length).map_err(|_| JsonlBatchError::LengthOverflow)?;
    let proof_start = offset
        .checked_sub(proof_length)
        .ok_or(JsonlBatchError::LengthOverflow)?;
    reader.seek(SeekFrom::Start(proof_start))?;

    let mut scratch = [0_u8; JSONL_BOUNDARY_MAX_BYTES];
    let mut total = 0;
    while total < proof_length_usize {
        let amount = match reader.read(&mut scratch[total..proof_length_usize]) {
            Ok(0) => {
                let actual = proof_start
                    .checked_add(u64::try_from(total).map_err(|_| JsonlBatchError::LengthOverflow)?)
                    .ok_or(JsonlBatchError::LengthOverflow)?;
                return Err(JsonlBatchError::SourceChangedDuringRead {
                    expected: offset,
                    actual,
                });
            }
            Ok(amount) => amount,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        total = total
            .checked_add(amount)
            .ok_or(JsonlBatchError::LengthOverflow)?;
    }
    encode_jsonl_position(offset, &scratch[..proof_length_usize])
}

fn encode_jsonl_position(
    offset: u64,
    proof_bytes: &[u8],
) -> Result<NativePosition, JsonlBatchError> {
    let proof_length = jsonl_boundary_proof_length(offset);
    if u64::try_from(proof_bytes.len()).map_err(|_| JsonlBatchError::LengthOverflow)?
        != proof_length
    {
        return Err(JsonlBatchError::InvalidBoundaryProofLength {
            offset,
            actual: proof_bytes.len(),
        });
    }
    let proof_length = u32::try_from(proof_length).map_err(|_| JsonlBatchError::LengthOverflow)?;
    let mut hasher = Sha256::new();
    hasher.update(JSONL_BOUNDARY_HASH_DOMAIN);
    hasher.update(offset.to_be_bytes());
    hasher.update(proof_length.to_be_bytes());
    hasher.update(proof_bytes);
    let digest = hasher.finalize();

    let mut value = Vec::with_capacity(JSONL_POSITION_ENCODED_BYTES);
    value.extend_from_slice(JSONL_POSITION_MAGIC);
    value.push(JSONL_POSITION_ENCODING_VERSION);
    value.push(JSONL_POSITION_HASH_SHA256);
    value.extend_from_slice(&[0; JSONL_POSITION_RESERVED_BYTES]);
    value.extend_from_slice(&offset.to_be_bytes());
    value.extend_from_slice(&proof_length.to_be_bytes());
    value.extend_from_slice(&digest);
    NativePosition::new(JSONL_POSITION_KIND, value).map_err(JsonlBatchError::from)
}

fn jsonl_boundary_proof_length(offset: u64) -> u64 {
    offset.min(JSONL_BOUNDARY_MAX_BYTES as u64)
}

fn jsonl_locator(
    source_item: &[u8],
    start: u64,
    end: u64,
) -> Result<NativeLocator, JsonlBatchError> {
    let value_len = validate_jsonl_source_item(source_item)?;
    let source_len =
        u32::try_from(source_item.len()).map_err(|_| JsonlBatchError::LengthOverflow)?;
    let mut value = Vec::with_capacity(value_len);
    value.extend_from_slice(&source_len.to_be_bytes());
    value.extend_from_slice(source_item);
    value.extend_from_slice(&start.to_be_bytes());
    value.extend_from_slice(&end.to_be_bytes());
    Ok(NativeLocator::new(JSONL_LOCATOR_KIND, value)?)
}

fn validate_jsonl_source_item(source_item: &[u8]) -> Result<usize, JsonlBatchError> {
    let value_len = size_of::<u32>()
        .checked_add(source_item.len())
        .and_then(|length| length.checked_add(2 * size_of::<u64>()))
        .ok_or(JsonlBatchError::LengthOverflow)?;
    validate_native_locator_value_len(value_len)?;
    Ok(value_len)
}

#[derive(Debug, Error)]
pub(crate) enum JsonlBatchError {
    #[error("invalid JSONL capture range: start {start}, end {end}")]
    InvalidRange { start: u64, end: u64 },
    #[error("JSONL capture length overflow")]
    LengthOverflow,
    #[error("unknown JSONL native position kind {kind}")]
    UnknownPositionKind { kind: String },
    #[error("unknown JSONL native locator kind {kind}")]
    UnknownLocatorKind { kind: String },
    #[error("malformed JSONL native locator")]
    MalformedLocator,
    #[error("unknown JSONL native position encoding version {version}")]
    UnknownPositionVersion { version: u8 },
    #[error("unknown JSONL native position hash algorithm {hash_algorithm}")]
    UnknownPositionHashAlgorithm { hash_algorithm: u8 },
    #[error("malformed JSONL native position: {reason}")]
    MalformedPosition { reason: &'static str },
    #[error("invalid JSONL boundary proof length {actual} for offset {offset}")]
    InvalidBoundaryProofLength { offset: u64, actual: usize },
    #[error(
        "JSONL append boundary {boundary} is beyond the observed source length {observed_length}"
    )]
    AppendBoundaryTruncated { boundary: u64, observed_length: u64 },
    #[error("JSONL append boundary commitment does not match at offset {boundary}")]
    AppendBoundaryMismatch { boundary: u64 },
    #[error("JSONL source changed during read: expected end {expected}, reached end {actual}")]
    SourceChangedDuringRead { expected: u64, actual: u64 },
    #[error("JSONL batch producer is poisoned after a partial capture failure")]
    ProducerPoisoned,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Batch(#[from] CapturedBatchError),
}

#[cfg(test)]
#[path = "jsonl_tests.rs"]
mod tests;
