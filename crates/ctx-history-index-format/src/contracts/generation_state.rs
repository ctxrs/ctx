use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};

use super::{IndexError, Result};

pub const MAX_GENERATION_STATE_BYTES: usize = 48 * 1024;
pub const MAX_GENERATION_STATE_FORMAT_BYTES: usize = 128;

/// Bounded generation-owned logical source state.
///
/// Core commits these opaque canonical bytes into generation identity while
/// the format identifier names the producer-owned decoding contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationStateEnvelope {
    format: String,
    #[serde(with = "generation_state_bytes")]
    bytes: Vec<u8>,
}

impl GenerationStateEnvelope {
    pub fn new(format: impl Into<String>, bytes: Vec<u8>) -> Result<Self> {
        let envelope = Self {
            format: format.into(),
            bytes,
        };
        envelope.validate_contract()?;
        Ok(envelope)
    }

    pub fn format(&self) -> &str {
        &self.format
    }

    pub fn canonical_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn empty() -> Self {
        Self {
            format: "ctx.generation-state.empty.v1".to_owned(),
            bytes: Vec::new(),
        }
    }

    pub(super) fn validate_contract(&self) -> Result<()> {
        if self.format.is_empty()
            || self.format.len() > MAX_GENERATION_STATE_FORMAT_BYTES
            || !self.format.is_ascii()
            || !self.format.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
            })
            || !self
                .format
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            || !self
                .format
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
        {
            return Err(IndexError::InvalidGenerationStateEnvelope);
        }
        if self.bytes.len() > MAX_GENERATION_STATE_BYTES {
            return Err(IndexError::GenerationStateTooLarge {
                actual: self.bytes.len(),
                maximum: MAX_GENERATION_STATE_BYTES,
            });
        }
        Ok(())
    }
}

mod generation_state_bytes {
    use super::*;
    use serde::{de::Error as _, Deserializer, Serializer};

    pub(super) fn serialize<S>(bytes: &[u8], serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD_NO_PAD.encode(bytes))
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> std::result::Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        if encoded.len() > MAX_GENERATION_STATE_BYTES.div_ceil(3) * 4 {
            return Err(D::Error::custom(
                "generation-owned state exceeds its encoded bound",
            ));
        }
        let bytes = STANDARD_NO_PAD
            .decode(&encoded)
            .map_err(|_| D::Error::custom("generation-owned state is not canonical base64"))?;
        if bytes.len() > MAX_GENERATION_STATE_BYTES || STANDARD_NO_PAD.encode(&bytes) != encoded {
            return Err(D::Error::custom(
                "generation-owned state is not canonical base64",
            ));
        }
        Ok(bytes)
    }
}
