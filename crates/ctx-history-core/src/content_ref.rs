use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

/// Provider-independent identity of one complete normalized result body.
///
/// The reference deliberately carries no storage location. It is the stable
/// seam for resolving source bytes today and an optional content-addressed
/// store in the future.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContentRef {
    sha256: String,
    byte_len: u64,
}

impl ContentRef {
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        Some(Self {
            sha256: format!("{:x}", Sha256::digest(bytes)),
            byte_len: u64::try_from(bytes.len()).ok()?,
        })
    }

    #[must_use]
    pub fn new(sha256: impl Into<String>, byte_len: u64) -> Option<Self> {
        let sha256 = sha256.into();
        valid_sha256(&sha256).then_some(Self { sha256, byte_len })
    }

    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    #[must_use]
    pub const fn byte_len(&self) -> u64 {
        self.byte_len
    }

    #[must_use]
    pub fn verifies(&self, bytes: &[u8]) -> bool {
        u64::try_from(bytes.len()).ok() == Some(self.byte_len)
            && format!("{:x}", Sha256::digest(bytes)) == self.sha256
    }
}

impl<'de> Deserialize<'de> for ContentRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            sha256: String,
            byte_len: u64,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.sha256, wire.byte_len)
            .ok_or_else(|| D::Error::custom("content SHA-256 must be lowercase hexadecimal"))
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_ref_is_stable_and_checks_exact_bytes() {
        let content_ref = ContentRef::from_bytes(b"normalized output").unwrap();
        assert_eq!(content_ref.byte_len(), 17);
        assert_eq!(
            content_ref.sha256(),
            "c279e5a970930cc6deca8b60ca3a48c2a1c9ef4b5be874400eb9fb8009e216a1"
        );
        assert!(content_ref.verifies(b"normalized output"));
        assert!(!content_ref.verifies(b"normalized output!"));
    }

    #[test]
    fn content_ref_deserialization_is_strict() {
        assert!(serde_json::from_value::<ContentRef>(serde_json::json!({
            "sha256": "a".repeat(64),
            "byte_len": 0
        }))
        .is_ok());
        assert!(serde_json::from_value::<ContentRef>(serde_json::json!({
            "sha256": "A".repeat(64),
            "byte_len": 0
        }))
        .is_err());
    }
}
