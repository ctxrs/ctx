use super::*;

pub(super) use crate::fingerprint::{hash_optional_text, hash_text};

impl crate::fingerprint::CountOverflowError for WarpSourceBackedErrorV0 {
    fn count_overflow() -> Self {
        Self::CountOverflow
    }
}

pub(super) fn missing_tree_fingerprint(source: &SourceKey) -> [u8; 32] {
    crate::fingerprint::missing_tree_fingerprint(WARP_MISSING_TREE_DOMAIN, source)
}

/// Pins the shared counter to this provider's error taxonomy so call sites do
/// not each have to name it.
pub(super) fn checked_add(left: u64, right: u64) -> WarpSourceBackedResultV0<u64> {
    crate::fingerprint::checked_add(left, right)
}

/// Warp's caller threads a `Result` through its digest builders, so the shared
/// infallible helper keeps that shape here.
pub(super) fn hash_bytes(digest: &mut Sha256, value: &[u8]) -> WarpSourceBackedResultV0<()> {
    crate::fingerprint::hash_bytes(digest, value);
    Ok(())
}

pub(super) fn parse_hex_digest(value: &str) -> WarpSourceBackedResultV0<[u8; 32]> {
    digest_bytes(value)
}

pub(super) fn digest_bytes(value: &str) -> WarpSourceBackedResultV0<[u8; 32]> {
    if value.len() != 64
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
    {
        return Err(WarpSourceBackedErrorV0::InvalidDigest);
    }
    let mut digest = [0_u8; 32];
    for (index, slot) in digest.iter_mut().enumerate() {
        let offset = index * 2;
        *slot = u8::from_str_radix(&value[offset..offset + 2], 16)
            .map_err(|_| WarpSourceBackedErrorV0::InvalidDigest)?;
    }
    Ok(digest)
}
