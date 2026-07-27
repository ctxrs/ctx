use std::io::Cursor;

use ctx_history_core::CaptureProvider;

use super::*;
use crate::captured_batch::{
    CapturedRecordPayload, CAPTURE_BATCH_MAX_RECORDS, MAX_NATIVE_LOCATOR_BYTES,
};

struct PartialIoFailureReader {
    bytes: Vec<u8>,
    position: usize,
    fail_after: usize,
    failed: bool,
}

impl io::Read for PartialIoFailureReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let available = self.fill_buf()?;
        let amount = available.len().min(buffer.len());
        buffer[..amount].copy_from_slice(&available[..amount]);
        self.consume(amount);
        Ok(amount)
    }
}

impl io::BufRead for PartialIoFailureReader {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.position == self.fail_after && !self.failed {
            self.failed = true;
            return Err(io::Error::other("injected partial read failure"));
        }
        let start = self.position.min(self.bytes.len());
        let end = if self.position < self.fail_after {
            self.fail_after.min(self.bytes.len())
        } else {
            self.bytes.len()
        };
        Ok(&self.bytes[start..end])
    }

    fn consume(&mut self, amount: usize) {
        self.position = self.position.saturating_add(amount).min(self.bytes.len());
    }
}

impl io::Seek for PartialIoFailureReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let SeekFrom::Start(position) = position else {
            return Err(io::Error::other("unsupported test seek"));
        };
        self.position =
            usize::try_from(position).map_err(|_| io::Error::other("test seek overflow"))?;
        Ok(position)
    }
}

struct RollbackSeekFailureReader {
    inner: Cursor<Vec<u8>>,
    seek_calls: usize,
}

impl io::Read for RollbackSeekFailureReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buffer)
    }
}

impl io::BufRead for RollbackSeekFailureReader {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.inner.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.inner.consume(amount);
    }
}

impl io::Seek for RollbackSeekFailureReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        if self.seek_calls > 0 {
            return Err(io::Error::other("injected rollback seek failure"));
        }
        self.seek_calls += 1;
        self.inner.seek(position)
    }
}

struct CountingReader {
    inner: Cursor<Vec<u8>>,
    bytes_read: usize,
    first_read_offset: Option<u64>,
}

impl CountingReader {
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            inner: Cursor::new(bytes),
            bytes_read: 0,
            first_read_offset: None,
        }
    }
}

impl io::Read for CountingReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let start = self.inner.position();
        let amount = self.inner.read(buffer)?;
        if amount > 0 {
            self.first_read_offset.get_or_insert(start);
            self.bytes_read = self.bytes_read.saturating_add(amount);
        }
        Ok(amount)
    }
}

impl io::BufRead for CountingReader {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.inner.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.inner.consume(amount);
    }
}

impl io::Seek for CountingReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.inner.seek(position)
    }
}

fn observation(bytes: usize) -> SourceObservation {
    SourceObservation::new(
        CaptureProvider::Codex,
        "codex_session_jsonl",
        "session:abc",
        format!("size:{bytes}"),
        "provider:codex:codex_session_jsonl:source:test",
        1,
        1,
        None,
    )
    .unwrap()
}

fn producer(
    bytes: Vec<u8>,
    allow_unterminated_final_record: bool,
) -> JsonlBatchProducer<Cursor<Vec<u8>>> {
    let len = bytes.len();
    JsonlBatchProducer::new(
        Cursor::new(bytes),
        observation(len),
        b"session.jsonl".to_vec(),
        ProviderRecordKind::new("codex-jsonl-v1").unwrap(),
        len as u64,
        0,
        0,
        allow_unterminated_final_record,
    )
    .unwrap()
}

fn verify_append_boundary<R: BufRead + Seek>(
    reader: &mut R,
    position: &NativePosition,
    observed_length: u64,
) -> Result<VerifiedJsonlAppend, JsonlBatchError> {
    let source = observation(usize::try_from(observed_length).unwrap());
    verify_jsonl_append_boundary(reader, position, &source, observed_length)
}

#[test]
fn partitions_at_sixty_four_records_without_changing_ordinals() {
    let bytes = (0..CAPTURE_BATCH_MAX_RECORDS + 1)
        .map(|index| format!("{{\"index\":{index}}}\n"))
        .collect::<String>()
        .into_bytes();
    let mut producer = producer(bytes, false);

    let first = producer.next_batch().unwrap().unwrap();
    let second = producer.next_batch().unwrap().unwrap();
    assert_eq!(first.records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert_eq!(second.records().len(), 1);
    assert!(!first.source_exhausted());
    assert!(second.source_exhausted());
    assert_eq!(first.records()[0].ordinal(), 0);
    assert_eq!(
        second.records()[0].ordinal(),
        CAPTURE_BATCH_MAX_RECORDS as u64
    );
    assert_eq!(first.range_end(), second.range_before());
    assert_eq!(jsonl_position_offset(first.range_before()).unwrap(), 0);
    assert!(producer.next_batch().unwrap().is_none());
}

#[test]
fn byte_boundary_rewinds_without_retaining_the_next_payload() {
    let mut producer = producer(b"1234567\nabcdefg\n".to_vec(), false)
        .with_max_record_bytes(16)
        .with_max_batch_payload_bytes(8);

    let first = producer.next_batch().unwrap().unwrap();
    assert_eq!(first.records().len(), 1);
    assert_eq!(first.retained_payload_bytes(), 7);
    assert_eq!(producer.reader_offset, 8);
    assert_eq!(producer.next_ordinal, 1);

    let second = producer.next_batch().unwrap().unwrap();
    assert_eq!(second.records().len(), 1);
    assert_eq!(second.records()[0].ordinal(), 1);
    assert_eq!(second.retained_payload_bytes(), 7);
    assert!(producer.next_batch().unwrap().is_none());
}

#[test]
fn incomplete_tail_does_not_advance_the_emitted_range() {
    let mut producer = producer(b"{\"ok\":1}\n{\"partial\":".to_vec(), false);

    let batch = producer.next_batch().unwrap().unwrap();
    assert_eq!(batch.records().len(), 1);
    assert_eq!(jsonl_position_offset(batch.range_end()).unwrap(), 9);
    assert!(batch.source_exhausted());
    assert!(producer.next_batch().unwrap().is_none());
    assert_eq!(producer.emitted_end, 9);
}

#[test]
fn empty_boundary_is_fixed_and_verifies_without_reading_source_bytes() {
    let position = initial_jsonl_position().unwrap();
    assert_eq!(position.kind(), JSONL_POSITION_KIND);
    assert_eq!(position.value().len(), JSONL_POSITION_ENCODED_BYTES);
    assert_eq!(jsonl_position_offset(&position).unwrap(), 0);

    let mut reader = CountingReader::new(b"later bytes\n".to_vec());
    verify_append_boundary(&mut reader, &position, 12).unwrap();
    assert_eq!(reader.bytes_read, 0);
}

#[test]
fn true_append_preserves_the_committed_boundary() {
    let original = b"{\"first\":1}\n{\"second\":2}\n".to_vec();
    let mut original_reader = Cursor::new(original.clone());
    let position = jsonl_position_at(&mut original_reader, original.len() as u64).unwrap();
    let mut appended = original;
    appended.extend_from_slice(b"{\"third\":3}\n");
    let observed_length = appended.len() as u64;

    let source = observation(observed_length as usize);
    let verified = verify_jsonl_append_boundary(
        &mut Cursor::new(appended),
        &position,
        &source,
        observed_length,
    )
    .unwrap();
    assert!(verified.validates(&position, &source));
    assert!(!verified.validates(&initial_jsonl_position().unwrap(), &source));
    assert!(!verified.validates(&position, &observation(observed_length as usize + 1)));
}

#[test]
fn committed_prefix_and_tail_rewrites_are_rejected() {
    let original = vec![b'x'; JSONL_BOUNDARY_MAX_BYTES];
    let mut original_reader = Cursor::new(original.clone());
    let position = jsonl_position_at(&mut original_reader, original.len() as u64).unwrap();

    let mut prefix_rewrite = original.clone();
    prefix_rewrite[0] = b'y';
    assert!(matches!(
        verify_append_boundary(
            &mut Cursor::new(prefix_rewrite),
            &position,
            original.len() as u64,
        )
        .unwrap_err(),
        JsonlBatchError::AppendBoundaryMismatch { boundary }
            if boundary == original.len() as u64
    ));

    let mut tail_rewrite = original.clone();
    *tail_rewrite.last_mut().unwrap() = b'z';
    assert!(matches!(
        verify_append_boundary(
            &mut Cursor::new(tail_rewrite),
            &position,
            original.len() as u64,
        )
        .unwrap_err(),
        JsonlBatchError::AppendBoundaryMismatch { boundary }
            if boundary == original.len() as u64
    ));
}

#[test]
fn truncation_before_the_committed_boundary_is_rejected() {
    let original = b"one\ntwo\nthree\n".to_vec();
    let mut original_reader = Cursor::new(original.clone());
    let position = jsonl_position_at(&mut original_reader, original.len() as u64).unwrap();
    let truncated = b"one\ntwo\n".to_vec();

    assert!(matches!(
        verify_append_boundary(
            &mut Cursor::new(truncated.clone()),
            &position,
            truncated.len() as u64,
        )
        .unwrap_err(),
        JsonlBatchError::AppendBoundaryTruncated {
            boundary,
            observed_length,
        } if boundary == original.len() as u64 && observed_length == truncated.len() as u64
    ));
}

#[test]
fn unknown_and_malformed_positions_fail_closed_with_typed_errors() {
    let initial = initial_jsonl_position().unwrap();
    let unknown_kind =
        NativePosition::new("jsonl-byte-boundary-v999", initial.value().to_vec()).unwrap();
    assert!(matches!(
        jsonl_position_offset(&unknown_kind).unwrap_err(),
        JsonlBatchError::UnknownPositionKind { .. }
    ));

    let malformed = NativePosition::new(
        JSONL_POSITION_KIND,
        vec![0_u8; JSONL_POSITION_ENCODED_BYTES],
    )
    .unwrap();
    assert!(matches!(
        verify_append_boundary(&mut Cursor::new(Vec::new()), &malformed, 0).unwrap_err(),
        JsonlBatchError::MalformedPosition { .. }
    ));

    let mut unknown_version_value = initial.value().to_vec();
    unknown_version_value[JSONL_POSITION_MAGIC.len()] = 2;
    let unknown_version = NativePosition::new(JSONL_POSITION_KIND, unknown_version_value).unwrap();
    assert!(matches!(
        jsonl_position_offset(&unknown_version).unwrap_err(),
        JsonlBatchError::UnknownPositionVersion { version: 2 }
    ));
}

#[test]
fn append_verification_reads_only_the_final_sixty_four_kibibytes() {
    let length = JSONL_BOUNDARY_MAX_BYTES * 3 + 17;
    let original = (0..length)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let mut original_reader = Cursor::new(original.clone());
    let position = jsonl_position_at(&mut original_reader, length as u64).unwrap();
    let mut current_reader = CountingReader::new(original);

    verify_append_boundary(&mut current_reader, &position, length as u64).unwrap();

    assert_eq!(current_reader.bytes_read, JSONL_BOUNDARY_MAX_BYTES);
    assert_eq!(
        current_reader.first_read_offset,
        Some((length - JSONL_BOUNDARY_MAX_BYTES) as u64)
    );
}

#[test]
fn crlf_delimiter_is_not_part_of_exact_record_payload() {
    let mut producer = producer(b"{\"ok\":1}\r\n".to_vec(), false);
    let batch = producer.next_batch().unwrap().unwrap();

    assert!(matches!(
        batch.records()[0].payload(),
        CapturedRecordPayload::NativeBytes(payload) if payload == b"{\"ok\":1}"
    ));
}

#[test]
fn complete_oversize_record_is_an_explicit_zero_payload_rejection() {
    let mut producer = producer(b"123456789\n{}\n".to_vec(), false).with_max_record_bytes(8);
    let batch = producer.next_batch().unwrap().unwrap();

    assert_eq!(batch.records().len(), 2);
    assert_eq!(batch.retained_payload_bytes(), 2);
    assert!(matches!(
        batch.records()[0].payload(),
        CapturedRecordPayload::StructuralRejection {
            kind: StructuralRejectionKind::OversizeRecord,
            observed_bytes: 10,
        }
    ));
}

#[test]
fn immutable_source_may_admit_an_unterminated_final_record() {
    let mut producer = producer(b"{\"ok\":1}".to_vec(), true);
    let batch = producer.next_batch().unwrap().unwrap();

    assert!(matches!(
        batch.records()[0].payload(),
        CapturedRecordPayload::NativeBytes(payload) if payload == b"{\"ok\":1}"
    ));
}

#[test]
fn early_eof_before_observation_end_is_a_typed_source_change() {
    let actual = b"{\"ok\":1}\n".to_vec();
    let expected = actual.len() as u64 + 5;
    let mut producer = JsonlBatchProducer::new(
        Cursor::new(actual.clone()),
        observation(expected as usize),
        b"session.jsonl".to_vec(),
        ProviderRecordKind::new("codex-jsonl-v1").unwrap(),
        expected,
        0,
        0,
        false,
    )
    .unwrap();

    assert!(matches!(
        producer.next_batch().unwrap_err(),
        JsonlBatchError::SourceChangedDuringRead {
            expected: observed,
            actual: reached,
        } if observed == expected && reached == actual.len() as u64
    ));
}

#[test]
fn partial_io_failure_poisons_retry() {
    let bytes = b"abcdef\n".to_vec();
    let mut producer = JsonlBatchProducer::new(
        PartialIoFailureReader {
            bytes: bytes.clone(),
            position: 0,
            fail_after: 3,
            failed: false,
        },
        observation(bytes.len()),
        b"session.jsonl".to_vec(),
        ProviderRecordKind::new("codex-jsonl-v1").unwrap(),
        bytes.len() as u64,
        0,
        0,
        false,
    )
    .unwrap();

    assert!(matches!(
        producer.next_batch().unwrap_err(),
        JsonlBatchError::Io(_)
    ));
    assert!(matches!(
        producer.next_batch().unwrap_err(),
        JsonlBatchError::ProducerPoisoned
    ));
}

#[test]
fn partial_seek_failure_poisons_retry() {
    let bytes = b"partial".to_vec();
    let mut producer = JsonlBatchProducer::new(
        RollbackSeekFailureReader {
            inner: Cursor::new(bytes.clone()),
            seek_calls: 0,
        },
        observation(bytes.len()),
        b"session.jsonl".to_vec(),
        ProviderRecordKind::new("codex-jsonl-v1").unwrap(),
        bytes.len() as u64,
        0,
        0,
        false,
    )
    .unwrap();

    assert!(matches!(
        producer.next_batch().unwrap_err(),
        JsonlBatchError::Io(_)
    ));
    assert!(matches!(
        producer.next_batch().unwrap_err(),
        JsonlBatchError::ProducerPoisoned
    ));
}

#[test]
fn source_item_locator_input_is_bounded_before_capture() {
    let overhead = size_of::<u32>() + 2 * size_of::<u64>();
    let maximum_source_item = MAX_NATIVE_LOCATOR_BYTES - overhead;
    assert!(JsonlBatchProducer::new(
        Cursor::new(Vec::new()),
        observation(0),
        vec![b'x'; maximum_source_item],
        ProviderRecordKind::new("codex-jsonl-v1").unwrap(),
        0,
        0,
        0,
        false,
    )
    .is_ok());

    let error = match JsonlBatchProducer::new(
        Cursor::new(Vec::new()),
        observation(0),
        vec![b'x'; maximum_source_item + 1],
        ProviderRecordKind::new("codex-jsonl-v1").unwrap(),
        0,
        0,
        0,
        false,
    ) {
        Ok(_) => panic!("oversized source item unexpectedly accepted"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        JsonlBatchError::Batch(CapturedBatchError::FieldTooLarge {
            field: "locator_value",
            actual,
            maximum: MAX_NATIVE_LOCATOR_BYTES,
        }) if actual == MAX_NATIVE_LOCATOR_BYTES + 1
    ));
}
