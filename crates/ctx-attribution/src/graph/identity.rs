//! Frozen identities for the production work graph and Flat/FST projection.

use std::fmt;

/// Identity of the shared Core-derived graph record contract.
pub const GRAPH_SCHEMA_FINGERPRINT: &str =
    "sha256:104d751cd5eecb4aa2f14f7158ece5e723aad3c9f9e86b6b03e2c514ca56276c";
/// Identity of the Core-derived Flat materialization semantics.
pub const GRAPH_SEMANTICS_FINGERPRINT: &str =
    "sha256:ba41de4c74166c6c85975ef75d81dd40e97f121bc172ef6bdc2e3085ea0f2c56";
/// Identity of the exact Core citation contract retained by Flat records.
pub const GRAPH_EVIDENCE_FINGERPRINT: &str =
    "sha256:340b10d708ce362687dc19a96322867575bee9fe551fec7cdca92252c0a43416";

/// Stable, length-delimited identity for a private graph record.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub(crate) struct GraphRecordId(String);

impl GraphRecordId {
    pub(crate) fn from_parts<'a>(
        namespace: &str,
        parts: impl IntoIterator<Item = &'a [u8]>,
    ) -> Self {
        let mut high = 0xcbf2_9ce4_8422_2325_u64;
        let mut low = 0x8422_2325_cbf2_9ce4_u64;
        mix(&mut high, namespace.as_bytes(), 0x100_0000_01b3);
        mix(&mut low, namespace.as_bytes(), 0x100_0000_01d5);
        for part in parts {
            let length = (part.len() as u64).to_be_bytes();
            mix(&mut high, &length, 0x100_0000_01b3);
            mix(&mut low, &length, 0x100_0000_01d5);
            mix(&mut high, part, 0x100_0000_01b3);
            mix(&mut low, part, 0x100_0000_01d5);
        }
        Self(format!("{namespace}_{high:016x}{low:016x}"))
    }
}

impl fmt::Display for GraphRecordId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn mix(state: &mut u64, bytes: &[u8], prime: u64) {
    for byte in bytes {
        *state ^= u64::from(*byte);
        *state = state.wrapping_mul(prime);
    }
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;
