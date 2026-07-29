use crate::Result;

mod event;
pub(crate) mod nativepath;
mod source;

#[allow(unused_imports)]
pub(crate) use event::{decode_openhands_event, decode_openhands_event_value};
#[allow(unused_imports)]
pub(crate) use nativepath::{
    project_openhands_source_backed_v1, OpenHandsHydratedRecordV1, OpenHandsLocatorResolverV1,
    OpenHandsRejectedEventV1, OpenHandsSourceBackedAdapterV1, OpenHandsSourceBackedErrorV1,
    OpenHandsSourceBackedProjectionV1, OpenHandsSourceBackedResultV1,
};

fn openhands_bounded_derived_text(value: String, field: &str) -> Result<String> {
    const MAX_DERIVED_TEXT_BYTES: usize = 16 * 1024;
    if value.len() > MAX_DERIVED_TEXT_BYTES {
        return Err(crate::CaptureError::InvalidPayload(format!(
            "OpenHands {field} exceeds {MAX_DERIVED_TEXT_BYTES} bytes"
        )));
    }
    Ok(value)
}
