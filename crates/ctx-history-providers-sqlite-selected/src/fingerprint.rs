//! Logical-fingerprint primitives shared by the selected SQLite providers.
//!
//! Every source-backed provider folds its scanned rows into one logical
//! fingerprint so a refresh that observes an unchanged source can publish a
//! no-op generation. The fold has the same shape everywhere — a domain-tagged
//! SHA-256 seeded with the schema capability digest, one relation tag plus one
//! 32-byte evidence value per row, and the row count mixed in at the end — and
//! the length-prefixed field helpers underneath it were duplicated per
//! provider.
//!
//! # Byte order
//!
//! These helpers write **big-endian** length prefixes, matching the Goose and
//! Warp source-backed digests. Warp's NativePath page identity uses its own
//! little-endian helpers and deliberately does not share this module: swapping
//! its byte order would rotate every persisted NativePath page identity.

use sha2::{Digest, Sha256};

use ctx_history_core::SourceKey;

/// Implemented by provider error types that can report a counter overflow, so
/// the shared counters can fail into each provider's own taxonomy.
pub(crate) trait CountOverflowError {
    fn count_overflow() -> Self;
}

/// Adds to a saturating-free row counter, failing into the provider's taxonomy.
pub(crate) fn checked_add<E: CountOverflowError>(left: u64, right: u64) -> Result<u64, E> {
    left.checked_add(right).ok_or_else(E::count_overflow)
}

/// The fingerprint published for a source whose logical tree is absent.
///
/// Keyed by the exact source descriptor so two different absent sources do not
/// collide on one no-op generation.
pub(crate) fn missing_tree_fingerprint(domain: &[u8], source: &SourceKey) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(source.exact_descriptor_digest());
    digest.finalize().into()
}

/// A rolling, relation-tagged fold over the rows a provider scanned.
pub(crate) struct RelationFingerprint {
    digest: Sha256,
    rows: u64,
}

impl RelationFingerprint {
    /// Seeds the fold with the provider's domain and its schema capability
    /// digest, so a schema change cannot produce a matching fingerprint.
    pub(crate) fn new(domain: &[u8], capability_digest: &str) -> Self {
        let mut digest = Sha256::new();
        digest.update(domain);
        hash_bytes(&mut digest, capability_digest.as_bytes());
        Self { digest, rows: 0 }
    }

    /// Folds in one row: its relation tag, then its 32-byte evidence.
    pub(crate) fn record<E: CountOverflowError>(
        &mut self,
        relation: u8,
        evidence: [u8; 32],
    ) -> Result<(), E> {
        self.rows = checked_add(self.rows, 1)?;
        self.digest.update([relation]);
        self.digest.update(evidence);
        Ok(())
    }

    /// Mixes in the row count so a truncated scan cannot match a full one.
    pub(crate) fn finish(mut self) -> [u8; 32] {
        self.digest.update(self.rows.to_be_bytes());
        self.digest.finalize().into()
    }
}

/// Length-prefixes `value` so adjacent fields cannot be confused for one
/// another by shifting a boundary.
pub(crate) fn hash_bytes(digest: &mut Sha256, value: &[u8]) {
    digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(value);
}

pub(crate) fn hash_text(digest: &mut Sha256, value: &str) {
    hash_bytes(digest, value.as_bytes());
}

pub(crate) fn hash_optional_text(digest: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            digest.update([1]);
            hash_text(digest, value);
        }
        None => digest.update([0]),
    }
}

pub(crate) fn hash_optional_i64(digest: &mut Sha256, value: Option<i64>) {
    match value {
        Some(value) => {
            digest.update([1]);
            digest.update(value.to_be_bytes());
        }
        None => digest.update([0]),
    }
}
