use std::{
    fs::File,
    io::{BufRead, BufReader},
};

use sha2::{Digest, Sha256};

use super::revalidation::JsonlPrefixHasher;
use super::{JsonlFamilyError, JsonlResult, JsonlResumableSha256, MAX_PROVIDER_JSONL_LINE_BYTES};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonlRecordFraming {
    maximum_stored_bytes: usize,
    terminal_nul_padding: bool,
}

impl JsonlRecordFraming {
    pub const fn new(maximum_stored_bytes: usize, terminal_nul_padding: bool) -> Self {
        Self {
            maximum_stored_bytes,
            terminal_nul_padding,
        }
    }

    pub const fn ordinary() -> Self {
        Self::new(
            // Preserve the ordinary-family contract: a maximum-sized JSON
            // value may be followed by CRLF. The common framer omits LF from
            // storage, and the ordinary caller removes the optional CR.
            MAX_PROVIDER_JSONL_LINE_BYTES + 1,
            false,
        )
    }

    pub const fn terminal_nul_padded(maximum_stored_bytes: usize) -> Self {
        Self::new(maximum_stored_bytes, true)
    }

    pub(crate) const fn allows_terminal_nul_padding(self) -> bool {
        self.terminal_nul_padding
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonlBoundedRecordRead {
    pub complete: bool,
    pub terminal_nul_padding: bool,
    pub oversized: bool,
    pub stored_len: usize,
    pub byte_len: u64,
    pub sha256: [u8; 32],
}

// Static dispatch gives each caller its exact digest policy without adding a
// per-chunk branch to the scanner hot path.
trait JsonlRecordDigest {
    fn update(&mut self, chunk: &[u8]);
    fn finish(self) -> [u8; 32];
    fn finish_incomplete(self) -> [u8; 32];
}

struct FullAndCompleteSha256<'a> {
    full_hasher: &'a mut JsonlResumableSha256,
    complete_hasher: &'a mut JsonlResumableSha256,
    complete_before_record: JsonlResumableSha256,
    record_hasher: Sha256,
}

impl JsonlRecordDigest for FullAndCompleteSha256<'_> {
    #[inline(always)]
    fn update(&mut self, chunk: &[u8]) {
        self.full_hasher.update(chunk);
        self.complete_hasher.update(chunk);
        self.record_hasher.update(chunk);
    }

    #[inline(always)]
    fn finish(self) -> [u8; 32] {
        self.record_hasher.finalize().into()
    }

    #[inline(always)]
    fn finish_incomplete(self) -> [u8; 32] {
        *self.complete_hasher = self.complete_before_record;
        self.record_hasher.finalize().into()
    }
}

struct CompleteSha256<'a> {
    complete_hasher: &'a mut JsonlResumableSha256,
    complete_before_record: JsonlResumableSha256,
    record_hasher: Sha256,
}

struct CompleteAndBoundedPrefixSha256<'a> {
    complete_hasher: &'a mut JsonlResumableSha256,
    complete_before_record: JsonlResumableSha256,
    record_hasher: Sha256,
    bounded_prefix_hasher: &'a mut Sha256,
    bounded_prefix_remaining: &'a mut u64,
}

struct FullCompleteAndBoundedPrefixSha256<'a, H> {
    full_hasher: &'a mut JsonlResumableSha256,
    complete_hasher: &'a mut JsonlResumableSha256,
    complete_before_record: JsonlResumableSha256,
    record_hasher: Sha256,
    bounded_prefix_hasher: &'a mut H,
    bounded_prefix_remaining: &'a mut u64,
}

impl<H: JsonlPrefixHasher> JsonlRecordDigest for FullCompleteAndBoundedPrefixSha256<'_, H> {
    #[inline(always)]
    fn update(&mut self, chunk: &[u8]) {
        self.full_hasher.update(chunk);
        self.complete_hasher.update(chunk);
        self.record_hasher.update(chunk);
        let take = usize::try_from((*self.bounded_prefix_remaining).min(chunk.len() as u64))
            .unwrap_or(chunk.len());
        self.bounded_prefix_hasher.update_bytes(&chunk[..take]);
        *self.bounded_prefix_remaining = self
            .bounded_prefix_remaining
            .saturating_sub(u64::try_from(take).unwrap_or(u64::MAX));
    }

    #[inline(always)]
    fn finish(self) -> [u8; 32] {
        self.record_hasher.finalize().into()
    }

    #[inline(always)]
    fn finish_incomplete(self) -> [u8; 32] {
        *self.complete_hasher = self.complete_before_record;
        self.record_hasher.finalize().into()
    }
}

impl JsonlRecordDigest for CompleteAndBoundedPrefixSha256<'_> {
    #[inline(always)]
    fn update(&mut self, chunk: &[u8]) {
        self.complete_hasher.update(chunk);
        self.record_hasher.update(chunk);
        let take = usize::try_from((*self.bounded_prefix_remaining).min(chunk.len() as u64))
            .unwrap_or(chunk.len());
        self.bounded_prefix_hasher.update(&chunk[..take]);
        *self.bounded_prefix_remaining = self
            .bounded_prefix_remaining
            .saturating_sub(u64::try_from(take).unwrap_or(u64::MAX));
    }

    #[inline(always)]
    fn finish(self) -> [u8; 32] {
        self.record_hasher.finalize().into()
    }

    #[inline(always)]
    fn finish_incomplete(self) -> [u8; 32] {
        *self.complete_hasher = self.complete_before_record;
        self.record_hasher.finalize().into()
    }
}

impl JsonlRecordDigest for CompleteSha256<'_> {
    #[inline(always)]
    fn update(&mut self, chunk: &[u8]) {
        self.complete_hasher.update(chunk);
        self.record_hasher.update(chunk);
    }

    #[inline(always)]
    fn finish(self) -> [u8; 32] {
        self.record_hasher.finalize().into()
    }

    #[inline(always)]
    fn finish_incomplete(self) -> [u8; 32] {
        *self.complete_hasher = self.complete_before_record;
        self.record_hasher.finalize().into()
    }
}

struct Unhashed;

impl JsonlRecordDigest for Unhashed {
    #[inline(always)]
    fn update(&mut self, _chunk: &[u8]) {}

    #[inline(always)]
    fn finish(self) -> [u8; 32] {
        [0; 32]
    }

    #[inline(always)]
    fn finish_incomplete(self) -> [u8; 32] {
        [0; 32]
    }
}

pub fn read_bounded_record<E: JsonlFamilyError>(
    reader: &mut BufReader<File>,
    storage: &mut Vec<u8>,
    full_hasher: &mut JsonlResumableSha256,
    complete_hasher: &mut JsonlResumableSha256,
    maximum_bytes: u64,
    framing: JsonlRecordFraming,
    source_changed: fn() -> E,
) -> JsonlResult<Option<JsonlBoundedRecordRead>, E> {
    if maximum_bytes == 0 {
        return Ok(None);
    }
    let digest = FullAndCompleteSha256 {
        full_hasher,
        complete_before_record: complete_hasher.clone(),
        complete_hasher,
        record_hasher: Sha256::new(),
    };
    read_bounded_record_with_digest(
        reader,
        storage,
        maximum_bytes,
        framing,
        source_changed,
        digest,
    )
}

pub(crate) fn read_bounded_record_complete_sha256<E: JsonlFamilyError>(
    reader: &mut BufReader<File>,
    storage: &mut Vec<u8>,
    complete_hasher: &mut JsonlResumableSha256,
    maximum_bytes: u64,
    framing: JsonlRecordFraming,
    source_changed: fn() -> E,
) -> JsonlResult<Option<JsonlBoundedRecordRead>, E> {
    if maximum_bytes == 0 {
        return Ok(None);
    }
    let digest = CompleteSha256 {
        complete_before_record: complete_hasher.clone(),
        complete_hasher,
        record_hasher: Sha256::new(),
    };
    read_bounded_record_with_digest(
        reader,
        storage,
        maximum_bytes,
        framing,
        source_changed,
        digest,
    )
}

pub fn read_bounded_record_complete_and_prefix_sha256<E: JsonlFamilyError>(
    reader: &mut BufReader<File>,
    storage: &mut Vec<u8>,
    complete_hasher: &mut JsonlResumableSha256,
    bounded_prefix: (&mut Sha256, &mut u64),
    maximum_bytes: u64,
    framing: JsonlRecordFraming,
    source_changed: fn() -> E,
) -> JsonlResult<Option<JsonlBoundedRecordRead>, E> {
    if maximum_bytes == 0 {
        return Ok(None);
    }
    let digest = CompleteAndBoundedPrefixSha256 {
        complete_before_record: complete_hasher.clone(),
        complete_hasher,
        record_hasher: Sha256::new(),
        bounded_prefix_hasher: bounded_prefix.0,
        bounded_prefix_remaining: bounded_prefix.1,
    };
    read_bounded_record_with_digest(
        reader,
        storage,
        maximum_bytes,
        framing,
        source_changed,
        digest,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn read_bounded_record_full_complete_and_prefix_sha256<E: JsonlFamilyError>(
    reader: &mut BufReader<File>,
    storage: &mut Vec<u8>,
    full_hasher: &mut JsonlResumableSha256,
    complete_hasher: &mut JsonlResumableSha256,
    bounded_prefix_hasher: &mut Sha256,
    bounded_prefix_remaining: &mut u64,
    maximum_bytes: u64,
    framing: JsonlRecordFraming,
    source_changed: fn() -> E,
) -> JsonlResult<Option<JsonlBoundedRecordRead>, E> {
    read_bounded_record_full_complete_and_prefix(
        reader,
        storage,
        full_hasher,
        complete_hasher,
        bounded_prefix_hasher,
        bounded_prefix_remaining,
        maximum_bytes,
        framing,
        source_changed,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn read_bounded_record_full_complete_and_prefix<
    E: JsonlFamilyError,
    H: JsonlPrefixHasher,
>(
    reader: &mut BufReader<File>,
    storage: &mut Vec<u8>,
    full_hasher: &mut JsonlResumableSha256,
    complete_hasher: &mut JsonlResumableSha256,
    bounded_prefix_hasher: &mut H,
    bounded_prefix_remaining: &mut u64,
    maximum_bytes: u64,
    framing: JsonlRecordFraming,
    source_changed: fn() -> E,
) -> JsonlResult<Option<JsonlBoundedRecordRead>, E> {
    if maximum_bytes == 0 {
        return Ok(None);
    }
    let digest = FullCompleteAndBoundedPrefixSha256 {
        full_hasher,
        complete_before_record: complete_hasher.clone(),
        complete_hasher,
        record_hasher: Sha256::new(),
        bounded_prefix_hasher,
        bounded_prefix_remaining,
    };
    read_bounded_record_with_digest(
        reader,
        storage,
        maximum_bytes,
        framing,
        source_changed,
        digest,
    )
}

pub fn read_bounded_record_unhashed<E: JsonlFamilyError>(
    reader: &mut BufReader<File>,
    storage: &mut Vec<u8>,
    maximum_bytes: u64,
    framing: JsonlRecordFraming,
    source_changed: fn() -> E,
) -> JsonlResult<Option<JsonlBoundedRecordRead>, E> {
    if maximum_bytes == 0 {
        return Ok(None);
    }
    read_bounded_record_with_digest(
        reader,
        storage,
        maximum_bytes,
        framing,
        source_changed,
        Unhashed,
    )
}

fn read_bounded_record_with_digest<E: JsonlFamilyError, D: JsonlRecordDigest>(
    reader: &mut BufReader<File>,
    storage: &mut Vec<u8>,
    maximum_bytes: u64,
    framing: JsonlRecordFraming,
    source_changed: fn() -> E,
    mut digest: D,
) -> JsonlResult<Option<JsonlBoundedRecordRead>, E> {
    storage.clear();
    let mut byte_len = 0_u64;
    let mut oversized = false;
    let mut all_nul = true;

    loop {
        let (consumed, complete) = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                if byte_len == 0 {
                    return Err(source_changed());
                }
                if framing.terminal_nul_padding && all_nul {
                    return Ok(Some(JsonlBoundedRecordRead {
                        complete: true,
                        terminal_nul_padding: true,
                        oversized,
                        stored_len: storage.len(),
                        byte_len,
                        sha256: [0; 32],
                    }));
                }
                return Ok(Some(JsonlBoundedRecordRead {
                    complete: false,
                    terminal_nul_padding: false,
                    oversized,
                    stored_len: storage.len(),
                    byte_len,
                    sha256: digest.finish_incomplete(),
                }));
            }

            let remaining = maximum_bytes.saturating_sub(byte_len);
            let bounded = usize::try_from(remaining.min(available.len() as u64))
                .map_err(|_| E::system_invariant("JSONL record bound exceeds usize"))?;
            let newline = available[..bounded].iter().position(|byte| *byte == b'\n');
            let consumed = newline.map_or(bounded, |index| index + 1);
            let chunk = &available[..consumed];
            digest.update(chunk);
            all_nul &= chunk.iter().all(|byte| *byte == 0);
            byte_len = byte_len
                .checked_add(
                    u64::try_from(consumed)
                        .map_err(|_| E::system_invariant("JSONL record chunk exceeds u64"))?,
                )
                .ok_or_else(|| E::system_invariant("JSONL record length exceeds u64"))?;

            let content_len = if newline.is_some() {
                consumed.saturating_sub(1)
            } else {
                consumed
            };
            let remaining = framing.maximum_stored_bytes.saturating_sub(storage.len());
            let copied = content_len.min(remaining);
            storage.extend_from_slice(&chunk[..copied]);
            if copied != content_len {
                oversized = true;
            }
            if newline.is_none() && byte_len == maximum_bytes {
                if framing.terminal_nul_padding && all_nul {
                    return Ok(Some(JsonlBoundedRecordRead {
                        complete: true,
                        terminal_nul_padding: true,
                        oversized,
                        stored_len: storage.len(),
                        byte_len,
                        sha256: [0; 32],
                    }));
                }
                return Ok(Some(JsonlBoundedRecordRead {
                    complete: false,
                    terminal_nul_padding: false,
                    oversized,
                    stored_len: storage.len(),
                    byte_len,
                    sha256: digest.finish_incomplete(),
                }));
            }
            (consumed, newline.is_some())
        };
        reader.consume(consumed);
        if complete {
            return Ok(Some(JsonlBoundedRecordRead {
                complete: true,
                terminal_nul_padding: false,
                oversized,
                stored_len: storage.len(),
                byte_len,
                sha256: digest.finish(),
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;

    const TEST_STORED_BYTES: usize = 1024;

    fn source_changed() -> super::super::CaptureError {
        super::super::CaptureError::SourceChangedDuringCapture
    }

    fn assert_digest_policies_match(contents: &[u8], frozen_len: u64) {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("bounded-record.jsonl");
        std::fs::write(&path, contents).unwrap();
        let framing = JsonlRecordFraming::terminal_nul_padded(TEST_STORED_BYTES);
        let mut hashed_reader = BufReader::with_capacity(8 * 1024, File::open(&path).unwrap());
        let mut unhashed_reader = BufReader::with_capacity(64 * 1024, File::open(&path).unwrap());
        let mut hashed_storage = Vec::new();
        let mut unhashed_storage = Vec::new();
        let mut full_hasher = JsonlResumableSha256::new();
        let mut complete_hasher = JsonlResumableSha256::new();
        let mut offset = 0_u64;
        let mut complete_end = 0_u64;

        while offset < frozen_len {
            let hashed = read_bounded_record(
                &mut hashed_reader,
                &mut hashed_storage,
                &mut full_hasher,
                &mut complete_hasher,
                frozen_len.saturating_sub(offset),
                framing,
                source_changed,
            )
            .unwrap()
            .unwrap();
            let unhashed = read_bounded_record_unhashed(
                &mut unhashed_reader,
                &mut unhashed_storage,
                frozen_len.saturating_sub(offset),
                framing,
                source_changed,
            )
            .unwrap()
            .unwrap();

            assert_eq!(unhashed.complete, hashed.complete);
            assert_eq!(unhashed.terminal_nul_padding, hashed.terminal_nul_padding);
            assert_eq!(unhashed.oversized, hashed.oversized);
            assert_eq!(unhashed.stored_len, hashed.stored_len);
            assert_eq!(unhashed.byte_len, hashed.byte_len);
            assert_eq!(unhashed.sha256, [0; 32]);
            assert_eq!(unhashed_storage, hashed_storage);

            let record_end = offset.saturating_add(hashed.byte_len);
            if hashed.terminal_nul_padding {
                assert_eq!(hashed.sha256, [0; 32]);
            } else {
                let start = usize::try_from(offset).unwrap();
                let end = usize::try_from(record_end).unwrap();
                assert_eq!(
                    hashed.sha256,
                    <[u8; 32]>::from(Sha256::digest(&contents[start..end]))
                );
            }
            if hashed.complete {
                complete_end = record_end;
            }
            offset = record_end;
        }

        let frozen_end = usize::try_from(frozen_len).unwrap();
        assert_eq!(offset, frozen_len);
        assert_eq!(
            full_hasher.digest(),
            <[u8; 32]>::from(Sha256::digest(&contents[..frozen_end]))
        );
        assert_eq!(
            complete_hasher.digest(),
            <[u8; 32]>::from(Sha256::digest(
                &contents[..usize::try_from(complete_end).unwrap()]
            ))
        );
    }

    #[test]
    fn digest_policies_preserve_bounds_terminal_padding_and_incomplete_tails() {
        let mut records = b"alpha\r\n".to_vec();
        records.resize(records.len() + TEST_STORED_BYTES + 17, b'x');
        records.push(b'\n');
        records.extend_from_slice(b"incomplete tail");
        assert_digest_policies_match(&records, records.len() as u64);

        let terminal_nul = vec![0; 64 * 1024 + 17];
        assert_digest_policies_match(&terminal_nul, terminal_nul.len() as u64);

        let frozen = b"first\nsecond\nnot admitted";
        assert_digest_policies_match(frozen, b"first\nsec".len() as u64);
    }

    #[test]
    fn digest_policies_fail_closed_when_frozen_bytes_are_missing() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("truncated.jsonl");
        std::fs::write(&path, b"complete\n").unwrap();
        let framing = JsonlRecordFraming::terminal_nul_padded(TEST_STORED_BYTES);
        let mut hashed_reader = BufReader::new(File::open(&path).unwrap());
        let mut unhashed_reader = BufReader::new(File::open(&path).unwrap());
        let mut hashed_storage = Vec::new();
        let mut unhashed_storage = Vec::new();
        let mut full_hasher = JsonlResumableSha256::new();
        let mut complete_hasher = JsonlResumableSha256::new();

        read_bounded_record(
            &mut hashed_reader,
            &mut hashed_storage,
            &mut full_hasher,
            &mut complete_hasher,
            10,
            framing,
            source_changed,
        )
        .unwrap();
        read_bounded_record_unhashed(
            &mut unhashed_reader,
            &mut unhashed_storage,
            10,
            framing,
            source_changed,
        )
        .unwrap();
        let hashed_error = read_bounded_record(
            &mut hashed_reader,
            &mut hashed_storage,
            &mut full_hasher,
            &mut complete_hasher,
            1,
            framing,
            source_changed,
        )
        .unwrap_err();
        let unhashed_error = read_bounded_record_unhashed(
            &mut unhashed_reader,
            &mut unhashed_storage,
            1,
            framing,
            source_changed,
        )
        .unwrap_err();

        assert_eq!(unhashed_error.to_string(), hashed_error.to_string());
        assert!(matches!(
            unhashed_error,
            super::super::CaptureError::SourceChangedDuringCapture
        ));
    }
}
