use ctx_history_core::SourceKey;

/// Stable storage identity for a public Core source.
#[must_use]
pub fn core_source_storage_id(source: &SourceKey) -> String {
    format!("core_source_{}", hex::encode(source.identity().digest()))
}

/// A stable non-cryptographic identity for derived storage rows.
#[must_use]
pub fn stable_id(prefix: &str, value: &str) -> String {
    stable_id_bytes(prefix, value.as_bytes())
}

/// Byte-oriented form used by persisted identities with binary components.
#[must_use]
pub fn stable_id_bytes(prefix: &str, value: &[u8]) -> String {
    fn fnv64(seed: u64, bytes: &[u8]) -> u64 {
        let mut hash = seed;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(1_099_511_628_211);
        }
        hash
    }

    let first = fnv64(14_695_981_039_346_656_037, value);
    let second = fnv64(7_809_847_782_465_536_322, value);
    format!("{prefix}_{first:016x}{second:016x}")
}
