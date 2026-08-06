use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::rows::CodexSessionRow;
use super::source::CodexFileObservation;

const CODEX_NATIVE_CHECKPOINT_VERSION: u8 = 10;
const CODEX_PENDING_CALL_ID_DOMAIN: &[u8] = b"ctx/codex-nativepath/pending-call-id/v1\0";
const MAX_CODEX_PENDING_TOOL_RECORD_BYTES: u64 = 16 * 1024 * 1024 + 1;
pub(crate) const MAX_CODEX_TOOL_CONTEXTS: usize = 24;
pub(super) const MAX_CODEX_TOOL_CALL_ID_BYTES: usize = 1024;
pub(super) const MAX_CODEX_CONTINUATION_CELL_ID_BYTES: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodexPendingToolAuthority {
    call_id_sha256: [u8; 32],
    pub(super) record_start: u64,
    pub(super) record_end: u64,
    pub(super) raw_ordinal: u64,
    continuation_cell_id: Option<String>,
    continuation_conflicted: bool,
    continuation_call_id_sha256: Vec<[u8; 32]>,
    continuation_capacity_exceeded: bool,
    correlation_ambiguous: bool,
}

impl CodexPendingToolAuthority {
    pub(super) fn new(call_id: &str, record_start: u64, record_end: u64, raw_ordinal: u64) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(CODEX_PENDING_CALL_ID_DOMAIN);
        hasher.update(call_id.as_bytes());
        Self {
            call_id_sha256: hasher.finalize().into(),
            record_start,
            record_end,
            raw_ordinal,
            continuation_cell_id: None,
            continuation_conflicted: false,
            continuation_call_id_sha256: Vec::new(),
            continuation_capacity_exceeded: false,
            correlation_ambiguous: false,
        }
    }

    pub(super) fn matches_call_id(&self, call_id: &str) -> bool {
        Self::new(
            call_id,
            self.record_start,
            self.record_end,
            self.raw_ordinal,
        )
        .call_id_sha256
            == self.call_id_sha256
    }

    pub(super) fn assign_continuation(&mut self, cell_id: &str) -> bool {
        if cell_id.is_empty()
            || cell_id.len() > MAX_CODEX_CONTINUATION_CELL_ID_BYTES
            || !cell_id.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            })
            || self
                .continuation_cell_id
                .as_deref()
                .is_some_and(|existing| existing != cell_id)
        {
            return false;
        }
        self.continuation_cell_id = Some(cell_id.to_owned());
        self.continuation_conflicted = false;
        true
    }

    pub(super) fn mark_continuation_conflict(&mut self, cell_id: &str) -> bool {
        if !self.assign_continuation(cell_id) {
            return false;
        }
        self.continuation_conflicted = true;
        true
    }

    pub(super) fn clear_continuation(&mut self) {
        self.continuation_cell_id = None;
        self.continuation_conflicted = false;
        self.continuation_call_id_sha256.clear();
        self.continuation_capacity_exceeded = false;
    }

    pub(super) fn continuation_cell_id(&self) -> Option<&str> {
        self.continuation_cell_id.as_deref()
    }

    pub(super) fn continuation_conflicted(&self) -> bool {
        self.continuation_conflicted
    }

    pub(super) fn record_continuation_call(&mut self, digest: [u8; 32]) {
        if digest == [0; 32] || self.continuation_call_id_sha256.contains(&digest) {
            return;
        }
        if self.continuation_call_id_sha256.len() >= MAX_CODEX_TOOL_CONTEXTS {
            self.continuation_capacity_exceeded = true;
        } else {
            self.continuation_call_id_sha256.push(digest);
        }
    }

    pub(super) fn continuation_call_id_sha256(&self) -> &[[u8; 32]] {
        &self.continuation_call_id_sha256
    }

    pub(super) fn continuation_capacity_exceeded(&self) -> bool {
        self.continuation_capacity_exceeded
    }

    pub(super) fn mark_correlation_ambiguous(&mut self) {
        self.correlation_ambiguous = true;
    }

    pub(super) fn correlation_ambiguous(&self) -> bool {
        self.correlation_ambiguous
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum CodexCheckpointBoundary {
    Terminal {
        complete_eof: u64,
    },
    Incomplete {
        complete_prefix_end: u64,
        incomplete_tail_len: u64,
        incomplete_tail_sha256: [u8; 32],
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodexNativeCheckpoint {
    version: u8,
    pub(crate) observation: CodexFileObservation,
    pub(crate) full_revision_sha256: [u8; 32],
    pub(crate) complete_prefix_sha256: [u8; 32],
    boundary: CodexCheckpointBoundary,
    complete_record_count: u64,
    pending_tool_authorities: Vec<CodexPendingToolAuthority>,
    pub(crate) owner: CodexSessionRow,
    pub(crate) lineage_dependency_sha256: [u8; 32],
}

impl CodexNativeCheckpoint {
    #[allow(
        clippy::too_many_arguments,
        reason = "the checkpoint constructor mirrors its fixed, versioned wire fields"
    )]
    pub(super) fn new(
        observation: CodexFileObservation,
        full_revision_sha256: [u8; 32],
        complete_prefix_sha256: [u8; 32],
        complete_prefix_end: u64,
        complete_record_count: u64,
        incomplete_tail: Option<(u64, [u8; 32])>,
        pending_tool_authorities: &[CodexPendingToolAuthority],
        owner: CodexSessionRow,
        lineage_dependency_sha256: [u8; 32],
    ) -> Self {
        let boundary = match incomplete_tail {
            Some((incomplete_tail_len, incomplete_tail_sha256)) => {
                CodexCheckpointBoundary::Incomplete {
                    complete_prefix_end,
                    incomplete_tail_len,
                    incomplete_tail_sha256,
                }
            }
            None => CodexCheckpointBoundary::Terminal {
                complete_eof: complete_prefix_end,
            },
        };
        Self {
            version: CODEX_NATIVE_CHECKPOINT_VERSION,
            observation,
            full_revision_sha256,
            complete_prefix_sha256,
            boundary,
            complete_record_count,
            pending_tool_authorities: pending_tool_authorities.to_vec(),
            owner,
            lineage_dependency_sha256,
        }
    }

    pub(crate) fn encode(&self) -> serde_json::Result<Vec<u8>> {
        serde_json::to_vec(self)
    }

    pub(crate) fn decode(bytes: &[u8]) -> serde_json::Result<Self> {
        let checkpoint = serde_json::from_slice::<Self>(bytes)?;
        checkpoint.validate_wire_state()?;
        Ok(checkpoint)
    }

    pub(crate) fn complete_prefix_end(&self) -> u64 {
        match self.boundary {
            CodexCheckpointBoundary::Terminal { complete_eof } => complete_eof,
            CodexCheckpointBoundary::Incomplete {
                complete_prefix_end,
                ..
            } => complete_prefix_end,
        }
    }

    pub(crate) fn next_raw_ordinal(&self) -> u64 {
        self.complete_record_count
    }

    pub(crate) fn incomplete_tail(&self) -> Option<(u64, [u8; 32])> {
        match self.boundary {
            CodexCheckpointBoundary::Terminal { .. } => None,
            CodexCheckpointBoundary::Incomplete {
                incomplete_tail_len,
                incomplete_tail_sha256,
                ..
            } => Some((incomplete_tail_len, incomplete_tail_sha256)),
        }
    }

    pub(super) fn pending_tool_authorities(&self) -> &[CodexPendingToolAuthority] {
        &self.pending_tool_authorities
    }

    fn validate_wire_state(&self) -> serde_json::Result<()> {
        if self.version != CODEX_NATIVE_CHECKPOINT_VERSION {
            return Err(serde::de::Error::custom(format!(
                "unsupported Codex NativePath checkpoint version {}",
                self.version
            )));
        }
        match self.boundary {
            CodexCheckpointBoundary::Terminal { complete_eof }
                if complete_eof == self.observation.len => {}
            CodexCheckpointBoundary::Incomplete {
                complete_prefix_end,
                incomplete_tail_len,
                ..
            } if incomplete_tail_len != 0
                && complete_prefix_end
                    .checked_add(incomplete_tail_len)
                    .is_some_and(|end| end == self.observation.len) => {}
            _ => {
                return Err(serde::de::Error::custom(
                    "invalid Codex NativePath checkpoint boundary state",
                ));
            }
        }
        let mut call_ids = BTreeSet::new();
        let mut record_spans = BTreeSet::new();
        let mut raw_ordinals = BTreeSet::new();
        let mut continuation_cells = BTreeSet::new();
        if self.pending_tool_authorities.len() > MAX_CODEX_TOOL_CONTEXTS
            || self.pending_tool_authorities.iter().any(|authority| {
                authority.record_start >= authority.record_end
                    || authority.record_end > self.complete_prefix_end()
                    || authority.record_end.saturating_sub(authority.record_start)
                        > MAX_CODEX_PENDING_TOOL_RECORD_BYTES
                    || authority.raw_ordinal >= self.complete_record_count
                    || !call_ids.insert(authority.call_id_sha256)
                    || !record_spans.insert((authority.record_start, authority.record_end))
                    || !raw_ordinals.insert(authority.raw_ordinal)
                    || authority
                        .continuation_cell_id
                        .as_ref()
                        .is_some_and(|cell_id| {
                            cell_id.is_empty()
                                || cell_id.len() > MAX_CODEX_CONTINUATION_CELL_ID_BYTES
                                || !cell_id.bytes().all(|byte| {
                                    byte.is_ascii_alphanumeric()
                                        || matches!(byte, b'-' | b'_' | b'.' | b':')
                                })
                                || !continuation_cells.insert(cell_id.clone())
                        })
                    || authority.continuation_call_id_sha256.len() > MAX_CODEX_TOOL_CONTEXTS
                    || (authority.continuation_capacity_exceeded
                        && authority.continuation_call_id_sha256.len() != MAX_CODEX_TOOL_CONTEXTS)
                    || (authority.continuation_conflicted
                        && authority.continuation_cell_id.is_none())
                    || authority.continuation_call_id_sha256.contains(&[0; 32])
                    || authority
                        .continuation_call_id_sha256
                        .iter()
                        .collect::<BTreeSet<_>>()
                        .len()
                        != authority.continuation_call_id_sha256.len()
            })
        {
            return Err(serde::de::Error::custom(
                "Codex NativePath checkpoint pending-tool authority is invalid",
            ));
        }
        Ok(())
    }
}
