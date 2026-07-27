use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::provider::native_ingestion::{NativeIngestionPageError, NativeSafeFrontier};

use super::source::PiPhysicalFileId;

pub(super) const PI_NATIVEPATH_FRONTIER_VERSION: u32 = 1;
pub(super) const PI_NATIVEPATH_PARSER_REVISION: u32 = 1;
pub(super) const PI_NATIVEPATH_POLICY_REVISION: u32 = 1;
const PI_INITIAL_PREFIX_DOMAIN: &[u8] = b"ctx-pi-nativepath-prefix-v1\0";

/// Content-free authority for one exact complete JSONL prefix.
///
/// It contains no session IDs, paths, message text, command text, output text,
/// previews, or diagnostics. The route and complete source bytes are represented
/// only by fixed-size digests.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PiNativeCheckpoint {
    pub(crate) parser_revision: u32,
    pub(crate) policy_revision: u32,
    pub(crate) route_sha256: [u8; 32],
    pub(crate) physical_file_id: Option<PiPhysicalFileId>,
    pub(crate) observed_file_len: u64,
    pub(crate) complete_offset: u64,
    pub(crate) next_ordinal: u64,
    pub(crate) committed_prefix_sha256: [u8; 32],
    pub(crate) terminal: bool,
}

impl PiNativeCheckpoint {
    pub(super) fn initial(
        route_sha256: [u8; 32],
        physical_file_id: Option<PiPhysicalFileId>,
        observed_file_len: u64,
    ) -> Self {
        Self {
            parser_revision: PI_NATIVEPATH_PARSER_REVISION,
            policy_revision: PI_NATIVEPATH_POLICY_REVISION,
            route_sha256,
            physical_file_id,
            observed_file_len,
            complete_offset: 0,
            next_ordinal: 0,
            committed_prefix_sha256: initial_prefix_sha256(),
            terminal: false,
        }
    }

    pub(super) fn revisions_match(&self) -> bool {
        self.parser_revision == PI_NATIVEPATH_PARSER_REVISION
            && self.policy_revision == PI_NATIVEPATH_POLICY_REVISION
    }

    pub(super) fn decode_frontier(
        frontier: &NativeSafeFrontier,
    ) -> Result<Self, super::source::PiNativePathError> {
        if frontier.version != PI_NATIVEPATH_FRONTIER_VERSION {
            return Err(super::source::PiNativePathError::Page(
                "Pi NativePath frontier version is unsupported".to_owned(),
            ));
        }
        let checkpoint: Self = serde_json::from_slice(&frontier.bytes)?;
        if !checkpoint.revisions_match() {
            return Err(super::source::PiNativePathError::Page(
                "Pi NativePath frontier revisions are unsupported".to_owned(),
            ));
        }
        Ok(checkpoint)
    }

    pub(super) fn safe_frontier(&self) -> Result<NativeSafeFrontier, NativeIngestionPageError> {
        let bytes = serde_json::to_vec(self)
            .map_err(|_| NativeIngestionPageError::FrontierTooLarge { bytes: usize::MAX })?;
        NativeSafeFrontier::new(PI_NATIVEPATH_FRONTIER_VERSION, bytes)
    }
}

pub(super) fn initial_prefix_hasher() -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(PI_INITIAL_PREFIX_DOMAIN);
    hasher
}

pub(super) fn initial_prefix_sha256() -> [u8; 32] {
    initial_prefix_hasher().finalize().into()
}

pub(super) fn prefix_digest(hasher: &Sha256) -> [u8; 32] {
    hasher.clone().finalize().into()
}
