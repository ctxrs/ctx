#[allow(unused_imports)]
use super::*;

pub const CAPTURE_SCHEMA_VERSION: u32 = 1;

pub(crate) fn validate_envelope(envelope: &CaptureEnvelope) -> Result<()> {
    if envelope.schema_version == CAPTURE_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(CaptureError::UnsupportedSchemaVersion(
            envelope.schema_version,
        ))
    }
}
