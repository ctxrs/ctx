use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use ctx_history_core::TypedKey;
use serde::{de::DeserializeOwned, Serialize};

use crate::{CaptureError, Result};

pub(crate) fn bounded_checkpoint_fits(checkpoint: &impl Serialize, maximum_bytes: usize) -> bool {
    serde_json::to_vec(checkpoint).is_ok_and(|bytes| bytes.len() <= maximum_bytes)
}

pub(crate) fn encode_bounded_checkpoint(
    prefix: &str,
    checkpoint: &impl Serialize,
    maximum_bytes: usize,
    provider: &str,
) -> Result<TypedKey> {
    let bytes = serde_json::to_vec(checkpoint)?;
    if bytes.len() > maximum_bytes {
        return Err(too_large(provider));
    }
    TypedKey::utf8(format!("{prefix}{}", BASE64_STANDARD.encode(bytes)))
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

pub(crate) fn decode_bounded_checkpoint<T: DeserializeOwned>(
    checkpoint: &TypedKey,
    prefix: &str,
    maximum_bytes: usize,
    provider: &str,
) -> Result<T> {
    let TypedKey::Utf8(encoded) = checkpoint else {
        return Err(CaptureError::InvalidPayload(format!(
            "{provider} projector checkpoint is not an opaque string"
        )));
    };
    let encoded = encoded.strip_prefix(prefix).ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "{provider} projector checkpoint version is unsupported"
        ))
    })?;
    if encoded.len() > maximum_bytes.div_ceil(3) * 4 {
        return Err(too_large(provider));
    }
    let bytes = BASE64_STANDARD.decode(encoded).map_err(|_| {
        CaptureError::InvalidPayload(format!("{provider} projector checkpoint is malformed"))
    })?;
    if bytes.len() > maximum_bytes {
        return Err(too_large(provider));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn too_large(provider: &str) -> CaptureError {
    CaptureError::InvalidPayload(format!(
        "{provider} projector checkpoint exceeds its bounded encoding"
    ))
}
