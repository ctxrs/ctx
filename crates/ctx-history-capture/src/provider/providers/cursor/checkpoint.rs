use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::parser::{CursorRejectionKind, CursorSanitizedRecord};

const CURSOR_PREFIX_PROOF_DOMAIN: &[u8] = b"ctx:cursor:classified-prefix:v1\0";
const CURSOR_CONTENT_PREFIX_PROOF_DOMAIN: &[u8] = b"ctx:cursor:content-prefix:v1\0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CursorPrefixProof {
    pub(crate) sha256: [u8; 32],
    /// Private control-plane proof that also covers deliberately excluded bytes.
    pub(crate) content_sha256: [u8; 32],
    pub(crate) complete_bytes: u64,
    pub(crate) physical_lines: u64,
    pub(crate) semantic_records: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CursorSessionCheckpoint {
    pub(crate) started_at: Option<DateTime<Utc>>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CursorCheckpoint {
    pub(crate) schema_version: u32,
    pub(crate) parser_revision: u32,
    pub(crate) next_byte_offset: u64,
    pub(crate) next_physical_line: u64,
    pub(crate) next_semantic_ordinal: u64,
    pub(crate) prefix: CursorPrefixProof,
    pub(crate) session: CursorSessionCheckpoint,
    pub(crate) terminal: bool,
}

impl CursorCheckpoint {
    pub(crate) const SCHEMA_VERSION: u32 = 1;
    pub(crate) const PARSER_REVISION: u32 = 2;

    pub(super) fn new(
        prefix: CursorPrefixProof,
        session: CursorSessionCheckpoint,
        terminal: bool,
    ) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            parser_revision: Self::PARSER_REVISION,
            next_byte_offset: prefix.complete_bytes,
            next_physical_line: prefix.physical_lines,
            next_semantic_ordinal: prefix.semantic_records,
            prefix,
            session,
            terminal,
        }
    }
}

pub(super) struct CursorPrefixBuilder {
    hasher: Sha256,
    content_hasher: Sha256,
    complete_bytes: u64,
    physical_lines: u64,
    semantic_records: u64,
}

impl CursorPrefixBuilder {
    pub(super) fn new() -> Self {
        let mut hasher = Sha256::new();
        hasher.update(CURSOR_PREFIX_PROOF_DOMAIN);
        let mut content_hasher = Sha256::new();
        content_hasher.update(CURSOR_CONTENT_PREFIX_PROOF_DOMAIN);
        Self {
            hasher,
            content_hasher,
            complete_bytes: 0,
            physical_lines: 0,
            semantic_records: 0,
        }
    }

    pub(super) fn record_blank(&mut self, consumed_bytes: u64, content_sha256: [u8; 32]) {
        self.record_physical(b"blank", consumed_bytes, content_sha256);
    }

    pub(super) fn record_rejection(
        &mut self,
        kind: CursorRejectionKind,
        consumed_bytes: u64,
        content_sha256: [u8; 32],
    ) {
        self.record_physical(kind.proof_marker(), consumed_bytes, content_sha256);
    }

    pub(super) fn record_semantic(
        &mut self,
        consumed_bytes: u64,
        content_sha256: [u8; 32],
        record: &CursorSanitizedRecord,
    ) -> serde_json::Result<()> {
        self.record_physical(b"semantic", consumed_bytes, content_sha256);
        self.hasher.update(self.semantic_records.to_be_bytes());
        let encoded = serde_json::to_vec(record)?;
        self.hasher.update((encoded.len() as u64).to_be_bytes());
        self.hasher.update(encoded);
        self.semantic_records = self.semantic_records.saturating_add(1);
        Ok(())
    }

    fn record_physical(&mut self, marker: &[u8], consumed_bytes: u64, content_sha256: [u8; 32]) {
        self.hasher.update((marker.len() as u64).to_be_bytes());
        self.hasher.update(marker);
        self.hasher.update(consumed_bytes.to_be_bytes());
        self.content_hasher.update(consumed_bytes.to_be_bytes());
        self.content_hasher.update(content_sha256);
        self.complete_bytes = self.complete_bytes.saturating_add(consumed_bytes);
        self.physical_lines = self.physical_lines.saturating_add(1);
    }

    pub(super) fn complete_bytes(&self) -> u64 {
        self.complete_bytes
    }

    pub(super) fn physical_lines(&self) -> u64 {
        self.physical_lines
    }

    pub(super) fn semantic_records(&self) -> u64 {
        self.semantic_records
    }

    pub(super) fn finish(self) -> CursorPrefixProof {
        CursorPrefixProof {
            sha256: self.hasher.finalize().into(),
            content_sha256: self.content_hasher.finalize().into(),
            complete_bytes: self.complete_bytes,
            physical_lines: self.physical_lines,
            semantic_records: self.semantic_records,
        }
    }
}
