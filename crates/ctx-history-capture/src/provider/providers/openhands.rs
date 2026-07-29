use crate::Result;

mod event;
pub(crate) mod nativepath;
mod source;

fn openhands_bounded_derived_text(value: String, field: &str) -> Result<String> {
    const MAX_DERIVED_TEXT_BYTES: usize = 16 * 1024;
    if value.len() > MAX_DERIVED_TEXT_BYTES {
        return Err(crate::CaptureError::InvalidPayload(format!(
            "OpenHands {field} exceeds {MAX_DERIVED_TEXT_BYTES} bytes"
        )));
    }
    Ok(value)
}
