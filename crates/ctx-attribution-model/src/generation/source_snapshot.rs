use std::io::{self, Write};

use sha2::{Digest, Sha256};

use super::CoreSourceState;
use super::validation::hex_sha256;
use crate::{ErrorClass, ProtocolError};

/// Validated inventory identity, independent of how its ordered sources are stored.
pub struct CoreSourceSnapshot {
    pub(super) sha256: String,
    pub(super) source_count: u32,
    pub(super) event_count: u64,
}

impl CoreSourceSnapshot {
    pub fn from_sources<'a>(
        sources: impl IntoIterator<Item = &'a CoreSourceState>,
    ) -> Result<Self, ProtocolError> {
        let mut builder = CoreSourceSnapshotBuilder::default();
        for source in sources {
            builder.push(source)?;
        }
        Ok(builder.finish())
    }
}

/// Hashes the exact compact JSON array used by the original slice contract.
/// Only the current source is encoded; neither JSON nor descriptors accumulate.
#[derive(Clone)]
pub struct CoreSourceSnapshotBuilder {
    digest: Sha256,
    prior: Option<[u8; 32]>,
    source_count: u32,
    event_count: u64,
}

impl Default for CoreSourceSnapshotBuilder {
    fn default() -> Self {
        let mut digest = Sha256::new();
        digest.update(b"[");
        Self {
            digest,
            prior: None,
            source_count: 0,
            event_count: 0,
        }
    }
}

impl CoreSourceSnapshotBuilder {
    pub fn push(&mut self, source: &CoreSourceState) -> Result<(), ProtocolError> {
        source.validate()?;
        let identity = source.source.identity().digest();
        if self.prior.is_some_and(|prior| prior >= identity) {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "Core source snapshot must be strictly ordered by stable source identity",
            ));
        }
        let source_count = self.source_count.checked_add(1).ok_or_else(|| {
            ProtocolError::new(ErrorClass::Bounds, "Core source count overflowed")
        })?;
        let event_count = self
            .event_count
            .checked_add(source.event_count)
            .ok_or_else(|| ProtocolError::new(ErrorClass::Bounds, "Core event count overflowed"))?;
        if self.prior.is_some() {
            self.digest.update(b",");
        }
        serde_json::to_writer(DigestWriter(&mut self.digest), source).map_err(|_| {
            ProtocolError::new(ErrorClass::Internal, "Core source snapshot encoding failed")
        })?;
        self.prior = Some(identity);
        self.source_count = source_count;
        self.event_count = event_count;
        Ok(())
    }

    pub fn finish(mut self) -> CoreSourceSnapshot {
        self.digest.update(b"]");
        CoreSourceSnapshot {
            sha256: hex_sha256(self.digest.finalize()),
            source_count: self.source_count,
            event_count: self.event_count,
        }
    }
}

struct DigestWriter<'a>(&'a mut Sha256);
impl Write for DigestWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
