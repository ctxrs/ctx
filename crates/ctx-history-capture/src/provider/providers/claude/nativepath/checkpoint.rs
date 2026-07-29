use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::source::{ClaudePhysicalFileId, ClaudeSessionKey};

pub(super) const CLAUDE_NATIVEPATH_PARSER_REVISION: u32 = 5;
pub(super) const CLAUDE_NATIVEPATH_POLICY_REVISION: u32 = 5;
const CLAUDE_LANE_OBSERVATION_BINDING_DOMAIN: &[u8] =
    b"ctx-claude-nativepath-lane-observation-binding-v1\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChangeSignal {
    Fresh,
    Unchanged,
    Append,
    Rewrite,
    Truncation,
    Replacement,
    Relocation,
    LiveCopy,
    ConflictingLiveCopy,
    Reparse,
}

/// A content-free provider cursor at a structurally complete JSONL boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaudeNativeFrontier {
    pub(crate) complete_offset: u64,
    pub(crate) next_raw_ordinal: u64,
    pub(crate) complete_record_chain_sha256: [u8; 32],
    pub(crate) boundary_proof_len: u32,
    pub(crate) boundary_proof_sha256: [u8; 32],
    pub(crate) native_identity_chain_sha256: [u8; 32],
    pub(crate) native_identity_records: u64,
    pub(crate) appendable_boundary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ParseCheckpoint {
    pub(crate) parser_revision: u32,
    pub(crate) policy_revision: u32,
    pub(crate) session_key: ClaudeSessionKey,
    pub(crate) canonical_route: PathBuf,
    pub(crate) physical_file_id: Option<ClaudePhysicalFileId>,
    /// The Core lane observation under which its frontier and terminal flag
    /// were accepted.
    pub(crate) observed_file_len: u64,
    pub(crate) observation_sha256: [u8; 32],
    #[serde(default)]
    pub(crate) core_observation_binding_sha256: [u8; 32],
    pub(crate) complete_offset: u64,
    pub(crate) next_raw_ordinal: u64,
    pub(crate) complete_record_chain_sha256: [u8; 32],
    pub(crate) boundary_proof_len: u32,
    pub(crate) boundary_proof_sha256: [u8; 32],
    pub(crate) native_identity_chain_sha256: [u8; 32],
    pub(crate) native_identity_records: u64,
    pub(crate) terminal: bool,
    pub(crate) appendable_boundary: bool,
}

impl ParseCheckpoint {
    pub(super) fn core_revisions_match(&self) -> bool {
        self.parser_revision == CLAUDE_NATIVEPATH_PARSER_REVISION
            && self.policy_revision == CLAUDE_NATIVEPATH_POLICY_REVISION
    }

    pub(super) fn core_observation_binding_matches(&self) -> bool {
        self.core_observation_binding_sha256
            == lane_observation_binding(
                self.observed_file_len,
                &self.observation_sha256,
                &self.core_frontier(),
                self.terminal,
            )
    }

    pub(crate) fn core_frontier(&self) -> ClaudeNativeFrontier {
        ClaudeNativeFrontier {
            complete_offset: self.complete_offset,
            next_raw_ordinal: self.next_raw_ordinal,
            complete_record_chain_sha256: self.complete_record_chain_sha256,
            boundary_proof_len: self.boundary_proof_len,
            boundary_proof_sha256: self.boundary_proof_sha256,
            native_identity_chain_sha256: self.native_identity_chain_sha256,
            native_identity_records: self.native_identity_records,
            appendable_boundary: self.appendable_boundary,
        }
    }
}

pub(super) fn lane_observation_binding(
    observed_file_len: u64,
    observation_sha256: &[u8; 32],
    frontier: &ClaudeNativeFrontier,
    terminal: bool,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(CLAUDE_LANE_OBSERVATION_BINDING_DOMAIN);
    hasher.update(observed_file_len.to_be_bytes());
    hasher.update(observation_sha256);
    hasher.update(frontier.complete_offset.to_be_bytes());
    hasher.update(frontier.next_raw_ordinal.to_be_bytes());
    hasher.update(frontier.complete_record_chain_sha256);
    hasher.update(frontier.boundary_proof_len.to_be_bytes());
    hasher.update(frontier.boundary_proof_sha256);
    hasher.update(frontier.native_identity_chain_sha256);
    hasher.update(frontier.native_identity_records.to_be_bytes());
    hasher.update([u8::from(frontier.appendable_boundary), u8::from(terminal)]);
    hasher.finalize().into()
}
