use std::io::{Read, SeekFrom, Write};

use base64::{engine::general_purpose::STANDARD, read::DecoderReader, Engine as _};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use crate::{
    limits::{check_limit, inspect_json_reader},
    CanonicalState, FxDigest, FxId, FxProviderError, FxProviderResult, ReplacementScratch,
    ReplayLimits,
};

pub const RAW_STATE_CHUNK_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReplacementReason {
    Compaction,
    Migration,
    Recovery,
    LogCompaction,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReplacementStarted {
    pub replacement_id: FxId,
    pub reason: ReplacementReason,
    pub encoded_bytes: u64,
    pub sha256: FxDigest,
    pub chunk_count: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReplacementChunk {
    pub replacement_id: FxId,
    pub chunk_index: u64,
    pub raw_bytes: u64,
    pub chunk_sha256: FxDigest,
    pub base64: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReplacementCommitted {
    pub replacement_id: FxId,
    pub encoded_bytes: u64,
    pub sha256: FxDigest,
    pub chunk_count: u64,
}

pub(crate) struct ReplacementAccumulator {
    start: ReplacementStarted,
    scratch: Box<dyn crate::ScratchFile>,
    next_chunk: u64,
    decoded_bytes: u64,
    aggregate: Sha256,
}

impl ReplacementAccumulator {
    pub(crate) fn new(
        start: ReplacementStarted,
        scratch: &dyn ReplacementScratch,
        limits: ReplayLimits,
    ) -> FxProviderResult<Self> {
        if start.encoded_bytes == 0
            || start.chunk_count == 0
            || start.chunk_count != start.encoded_bytes.div_ceil(RAW_STATE_CHUNK_BYTES)
        {
            return Err(FxProviderError::InvalidReplacement(
                "replacement start metadata is inconsistent",
            ));
        }
        check_limit(
            "replacement decoded bytes",
            start.encoded_bytes,
            limits.max_replacement_decoded_bytes,
        )?;
        check_limit(
            "replacement scratch bytes",
            start.encoded_bytes,
            limits.max_scratch_bytes,
        )?;
        Ok(Self {
            start,
            scratch: scratch.create(limits.max_scratch_bytes)?,
            next_chunk: 0,
            decoded_bytes: 0,
            aggregate: Sha256::new(),
        })
    }

    pub(crate) fn push_chunk(
        &mut self,
        chunk: ReplacementChunk,
        limits: ReplayLimits,
    ) -> FxProviderResult<()> {
        let final_chunk = self.next_chunk + 1 == self.start.chunk_count;
        let expected = if final_chunk {
            self.start
                .encoded_bytes
                .checked_sub(self.decoded_bytes)
                .ok_or(FxProviderError::InvalidReplacement(
                    "replacement byte count underflow",
                ))?
        } else {
            RAW_STATE_CHUNK_BYTES
        };
        if chunk.replacement_id != self.start.replacement_id
            || chunk.chunk_index != self.next_chunk
            || chunk.raw_bytes != expected
            || expected == 0
            || expected > RAW_STATE_CHUNK_BYTES
        {
            return Err(FxProviderError::InvalidReplacement(
                "replacement chunk metadata is inconsistent",
            ));
        }
        let mut decoder = DecoderReader::new(chunk.base64.as_bytes(), &STANDARD);
        let mut chunk_hash = Sha256::new();
        let mut decoded = 0_u64;
        let decoded_capacity = usize::try_from(chunk.raw_bytes).map_err(|_| {
            FxProviderError::InvalidReplacement("replacement chunk size does not fit memory")
        })?;
        let mut canonical_source = Vec::with_capacity(decoded_capacity);
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            let read = decoder
                .read(&mut buffer)
                .map_err(|_| FxProviderError::InvalidReplacement("invalid chunk base64"))?;
            if read == 0 {
                break;
            }
            decoded = decoded.saturating_add(read as u64);
            let total = self.decoded_bytes.saturating_add(decoded);
            check_limit(
                "replacement decoded bytes",
                total,
                limits.max_replacement_decoded_bytes,
            )?;
            check_limit("replacement scratch bytes", total, limits.max_scratch_bytes)?;
            chunk_hash.update(&buffer[..read]);
            self.aggregate.update(&buffer[..read]);
            canonical_source.extend_from_slice(&buffer[..read]);
            self.scratch.write_all(&buffer[..read])?;
        }
        let digest = chunk_hash.finalize();
        if decoded != chunk.raw_bytes
            || digest.as_slice() != chunk.chunk_sha256.0
            || STANDARD.encode(&canonical_source) != chunk.base64
        {
            return Err(FxProviderError::InvalidReplacement(
                "replacement chunk length or hash mismatch",
            ));
        }
        self.decoded_bytes = self.decoded_bytes.saturating_add(decoded);
        self.next_chunk = self.next_chunk.saturating_add(1);
        Ok(())
    }

    pub(crate) fn commit(
        mut self,
        commit: ReplacementCommitted,
        prior: &CanonicalState,
        timestamp_ms: i64,
        limits: ReplayLimits,
    ) -> FxProviderResult<CanonicalState> {
        let digest = self.aggregate.finalize();
        if commit.replacement_id != self.start.replacement_id
            || commit.encoded_bytes != self.start.encoded_bytes
            || commit.chunk_count != self.start.chunk_count
            || commit.sha256 != self.start.sha256
            || self.next_chunk != self.start.chunk_count
            || self.decoded_bytes != self.start.encoded_bytes
            || digest.as_slice() != self.start.sha256.0
        {
            return Err(FxProviderError::InvalidReplacement(
                "replacement commit does not match transaction",
            ));
        }
        self.scratch.seek(SeekFrom::Start(0))?;
        inspect_json_reader(&mut *self.scratch, limits)?;
        self.scratch.seek(SeekFrom::Start(0))?;
        let decoded: CanonicalState = serde_json::from_reader(&mut *self.scratch)?;
        crate::limits::validate_canonical_state(&decoded, limits)?;
        if decoded.id != prior.id
            || decoded.created_at_ms != prior.created_at_ms
            || decoded.origin_workspace_root != prior.origin_workspace_root
            || decoded.workspace_root != prior.workspace_root
            || decoded.updated_at_ms != timestamp_ms
            || (self.start.reason == ReplacementReason::LogCompaction
                && timestamp_ms != prior.updated_at_ms)
        {
            return Err(FxProviderError::InvalidReplacement(
                "replacement changes immutable session identity",
            ));
        }
        Ok(decoded)
    }
}
