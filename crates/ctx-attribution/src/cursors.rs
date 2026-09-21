use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};

use crate::graph::segment_graph::{SegmentGraph, SegmentGraphError};
use crate::protocol::{MAX_BLAME_CURSOR_BYTES, ResolvedBlameTarget};
use crate::query::{BLAME_ORDERING_VERSION, BlameFactFamily, BlamePosition, FileBlamePosition};

const CURSOR_VERSION: u8 = 2;
const GENERATION_ID_BYTES: usize = 32;
const TARGET_DIGEST_BYTES: usize = 32;
const STABLE_FACT_BYTES: usize = 16;

impl SegmentGraph {
    #[doc(hidden)]
    pub fn encode_blame_cursor(
        &self,
        resolved: &ResolvedBlameTarget,
        graph_generation: u64,
        position: &BlamePosition,
    ) -> Result<String, SegmentGraphError> {
        let mut payload = Vec::new();
        payload.push(CURSOR_VERSION);
        payload.push(BLAME_ORDERING_VERSION);
        payload.push(position_kind(position));
        payload.extend_from_slice(&graph_generation.to_be_bytes());
        payload.extend_from_slice(
            &hex::decode(self.generation_id()).map_err(|_| SegmentGraphError::InvalidCursor)?,
        );
        payload.extend_from_slice(&target_fingerprint(resolved)?);
        match position {
            BlamePosition::File(position) => encode_file(&mut payload, position)?,
            BlamePosition::Commit(position) | BlamePosition::PullRequest(position) => {
                payload.extend_from_slice(&compact_fact_id(&position.fact_id)?);
            }
        }
        let encoded = URL_SAFE_NO_PAD.encode(payload);
        if encoded.len() > MAX_BLAME_CURSOR_BYTES {
            return Err(SegmentGraphError::QueryRecordTooLarge);
        }
        Ok(encoded)
    }

    #[doc(hidden)]
    pub fn decode_blame_cursor(
        &self,
        encoded: &str,
        resolved: &ResolvedBlameTarget,
        graph_generation: u64,
    ) -> Result<BlamePosition, SegmentGraphError> {
        if encoded.len() > MAX_BLAME_CURSOR_BYTES {
            return Err(SegmentGraphError::InvalidCursor);
        }
        let decoded = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| SegmentGraphError::InvalidCursor)?;
        if URL_SAFE_NO_PAD.encode(&decoded) != encoded {
            return Err(SegmentGraphError::InvalidCursor);
        }
        let mut reader = CursorReader::new(&decoded);
        if reader.byte()? != CURSOR_VERSION {
            return Err(SegmentGraphError::InvalidCursor);
        }
        if reader.byte()? != BLAME_ORDERING_VERSION {
            return Err(SegmentGraphError::StaleCursor);
        }
        let kind = reader.byte()?;
        if kind != resolved_kind(resolved) {
            return Err(SegmentGraphError::InvalidCursor);
        }
        if reader.u64()? != graph_generation {
            return Err(SegmentGraphError::StaleCursor);
        }
        if hex::encode(reader.fixed::<GENERATION_ID_BYTES>()?) != self.generation_id() {
            return Err(SegmentGraphError::StaleCursor);
        }
        if reader.fixed::<TARGET_DIGEST_BYTES>()? != target_fingerprint(resolved)? {
            return Err(SegmentGraphError::InvalidCursor);
        }
        let position = match kind {
            0 => BlamePosition::File(decode_file(&mut reader)?),
            1 | 2 => {
                let fact_id = expand_fact_id(reader.fixed::<STABLE_FACT_BYTES>()?);
                let family = if kind == 1 {
                    BlameFactFamily::Commit
                } else {
                    BlameFactFamily::PullRequest
                };
                let position = self
                    .blame_position(&fact_id, family, &resolved_resource_id(resolved)?)?
                    .ok_or(SegmentGraphError::InvalidCursor)?;
                if kind == 1 {
                    BlamePosition::Commit(position)
                } else {
                    BlamePosition::PullRequest(position)
                }
            }
            _ => return Err(SegmentGraphError::InvalidCursor),
        };
        if !reader.is_empty() {
            return Err(SegmentGraphError::InvalidCursor);
        }
        Ok(position)
    }
}

fn resolved_resource_id(
    target: &ResolvedBlameTarget,
) -> Result<crate::query::ResourceId, SegmentGraphError> {
    match target {
        ResolvedBlameTarget::Commit { commit, .. } => {
            Ok(crate::query::ResourceId(commit.id.clone()))
        }
        ResolvedBlameTarget::PullRequest { pull_request, .. } => {
            Ok(crate::query::ResourceId(pull_request.id.clone()))
        }
        ResolvedBlameTarget::File { .. } => Err(SegmentGraphError::InvalidCursor),
    }
}

fn encode_file(
    output: &mut Vec<u8>,
    position: &FileBlamePosition,
) -> Result<(), SegmentGraphError> {
    let oid = hex::decode(&position.head_oid).map_err(|_| SegmentGraphError::InvalidCursor)?;
    let oid_length = u8::try_from(oid.len()).map_err(|_| SegmentGraphError::InvalidCursor)?;
    if !matches!(oid_length, 20 | 32)
        || position
            .head_oid
            .bytes()
            .any(|byte| byte.is_ascii_uppercase())
    {
        return Err(SegmentGraphError::InvalidCursor);
    }
    output.push(oid_length);
    output.extend_from_slice(&oid);
    output.extend_from_slice(&position.requested_start.to_be_bytes());
    output.extend_from_slice(&position.requested_end.unwrap_or_default().to_be_bytes());
    output.extend_from_slice(&position.window_start.to_be_bytes());
    output.extend_from_slice(&position.window_end.to_be_bytes());
    output.extend_from_slice(&position.next_line.to_be_bytes());
    Ok(())
}

fn decode_file(reader: &mut CursorReader<'_>) -> Result<FileBlamePosition, SegmentGraphError> {
    let oid_length = reader.byte()?;
    if !matches!(oid_length, 20 | 32) {
        return Err(SegmentGraphError::InvalidCursor);
    }
    let head_oid = hex::encode(reader.bytes(usize::from(oid_length))?);
    let requested_start = reader.u32()?;
    let requested_end = match reader.u32()? {
        0 => None,
        value => Some(value),
    };
    Ok(FileBlamePosition {
        head_oid,
        requested_start,
        requested_end,
        window_start: reader.u32()?,
        window_end: reader.u32()?,
        next_line: reader.u32()?,
    })
}

fn target_fingerprint(resolved: &ResolvedBlameTarget) -> Result<[u8; 32], SegmentGraphError> {
    let encoded = serde_json::to_vec(resolved).map_err(|_| SegmentGraphError::InvalidCursor)?;
    let mut digest = Sha256::new();
    digest.update(b"ctx-pro-blame-resolved-target-v1\0");
    digest.update(encoded);
    Ok(digest.finalize().into())
}

fn compact_fact_id(fact_id: &str) -> Result<[u8; STABLE_FACT_BYTES], SegmentGraphError> {
    let suffix = fact_id
        .strip_prefix("fact_")
        .filter(|suffix| suffix.len() == STABLE_FACT_BYTES * 2)
        .ok_or(SegmentGraphError::InvalidCursor)?;
    if suffix
        .bytes()
        .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
    {
        return Err(SegmentGraphError::InvalidCursor);
    }
    hex::decode(suffix)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(SegmentGraphError::InvalidCursor)
}

fn expand_fact_id(bytes: [u8; STABLE_FACT_BYTES]) -> String {
    format!("fact_{}", hex::encode(bytes))
}

const fn position_kind(position: &BlamePosition) -> u8 {
    match position {
        BlamePosition::File(_) => 0,
        BlamePosition::Commit(_) => 1,
        BlamePosition::PullRequest(_) => 2,
    }
}

const fn resolved_kind(target: &ResolvedBlameTarget) -> u8 {
    match target {
        ResolvedBlameTarget::File { .. } => 0,
        ResolvedBlameTarget::Commit { .. } => 1,
        ResolvedBlameTarget::PullRequest { .. } => 2,
    }
}

struct CursorReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> CursorReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn byte(&mut self) -> Result<u8, SegmentGraphError> {
        let value = *self
            .bytes
            .get(self.offset)
            .ok_or(SegmentGraphError::InvalidCursor)?;
        self.offset += 1;
        Ok(value)
    }

    fn bytes(&mut self, length: usize) -> Result<&'a [u8], SegmentGraphError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(SegmentGraphError::InvalidCursor)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(SegmentGraphError::InvalidCursor)?;
        self.offset = end;
        Ok(value)
    }

    fn fixed<const N: usize>(&mut self) -> Result<[u8; N], SegmentGraphError> {
        self.bytes(N)?
            .try_into()
            .map_err(|_| SegmentGraphError::InvalidCursor)
    }

    fn u32(&mut self) -> Result<u32, SegmentGraphError> {
        self.fixed().map(u32::from_be_bytes)
    }

    fn u64(&mut self) -> Result<u64, SegmentGraphError> {
        self.fixed().map(u64::from_be_bytes)
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

#[cfg(test)]
#[path = "cursors_tests.rs"]
mod tests;
