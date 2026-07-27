//! Decoder for the JSONL position embedded in released pre-NativePath cursors.
//!
//! This module is migration-only. NativePath providers own their production
//! frontiers and never encode this position format.

use std::mem::size_of;

use thiserror::Error;

use crate::native_source::NativePosition;

const POSITION_KIND: &str = "jsonl-byte-boundary-v1";
const POSITION_MAGIC: &[u8; 8] = b"CTXJLBP\0";
const POSITION_ENCODING_VERSION: u8 = 1;
const POSITION_HASH_SHA256: u8 = 1;
const POSITION_RESERVED_BYTES: usize = 2;
const POSITION_OFFSET_START: usize =
    POSITION_MAGIC.len() + size_of::<u8>() + size_of::<u8>() + POSITION_RESERVED_BYTES;
const POSITION_PROOF_LENGTH_START: usize = POSITION_OFFSET_START + size_of::<u64>();
const POSITION_DIGEST_START: usize = POSITION_PROOF_LENGTH_START + size_of::<u32>();
const POSITION_ENCODED_BYTES: usize = POSITION_DIGEST_START + 32;
const BOUNDARY_MAX_BYTES: u64 = 64 * 1024;

pub(crate) fn released_jsonl_position_offset(
    position: &NativePosition,
) -> Result<u64, ReleasedJsonlCursorError> {
    if position.kind() != POSITION_KIND {
        return Err(ReleasedJsonlCursorError::UnknownPositionKind {
            kind: position.kind().to_owned(),
        });
    }
    let value = position.value();
    if value.len() != POSITION_ENCODED_BYTES {
        return Err(ReleasedJsonlCursorError::MalformedPosition {
            reason: "invalid encoded length",
        });
    }
    if &value[..POSITION_MAGIC.len()] != POSITION_MAGIC {
        return Err(ReleasedJsonlCursorError::MalformedPosition {
            reason: "invalid encoding domain",
        });
    }
    let version = value[POSITION_MAGIC.len()];
    if version != POSITION_ENCODING_VERSION {
        return Err(ReleasedJsonlCursorError::UnknownPositionVersion { version });
    }
    let hash_algorithm = value[POSITION_MAGIC.len() + size_of::<u8>()];
    if hash_algorithm != POSITION_HASH_SHA256 {
        return Err(ReleasedJsonlCursorError::UnknownPositionHashAlgorithm { hash_algorithm });
    }
    let reserved_start = POSITION_MAGIC.len() + size_of::<u8>() + size_of::<u8>();
    if value[reserved_start..POSITION_OFFSET_START]
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(ReleasedJsonlCursorError::MalformedPosition {
            reason: "nonzero reserved bytes",
        });
    }
    let offset = u64::from_be_bytes(
        value[POSITION_OFFSET_START..POSITION_PROOF_LENGTH_START]
            .try_into()
            .map_err(|_| ReleasedJsonlCursorError::MalformedPosition {
                reason: "invalid offset encoding",
            })?,
    );
    let proof_length = u32::from_be_bytes(
        value[POSITION_PROOF_LENGTH_START..POSITION_DIGEST_START]
            .try_into()
            .map_err(|_| ReleasedJsonlCursorError::MalformedPosition {
                reason: "invalid proof length encoding",
            })?,
    );
    if u64::from(proof_length) != offset.min(BOUNDARY_MAX_BYTES) {
        return Err(ReleasedJsonlCursorError::MalformedPosition {
            reason: "noncanonical proof length",
        });
    }
    Ok(offset)
}

#[derive(Debug, Error)]
pub(crate) enum ReleasedJsonlCursorError {
    #[error("unknown released JSONL position kind {kind}")]
    UnknownPositionKind { kind: String },
    #[error("unknown released JSONL position encoding version {version}")]
    UnknownPositionVersion { version: u8 },
    #[error("unknown released JSONL position hash algorithm {hash_algorithm}")]
    UnknownPositionHashAlgorithm { hash_algorithm: u8 },
    #[error("malformed released JSONL position: {reason}")]
    MalformedPosition { reason: &'static str },
}
