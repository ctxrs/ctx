//! Stable complete-content locator mechanics shared by the NativePath reader
//! and the released SQLite resolver.

use crate::native_source::NativeLocator;
use crate::{CaptureError, Result};

use super::schema::OpenCodeCapturedShape;

pub(crate) const OPENCODE_LOCATOR_KIND: &str = "opencode-sqlite-logical-row-v1";
const OPENCODE_MESSAGE_PHASE: u8 = 2;

pub(crate) fn decode_opencode_message_locator(
    locator: &NativeLocator,
) -> Result<(OpenCodeCapturedShape, i64)> {
    if locator.kind() != OPENCODE_LOCATOR_KIND
        || locator.value().len() != 10
        || locator.value()[9] != OPENCODE_MESSAGE_PHASE
    {
        return Err(CaptureError::InvalidPayload(
            "OpenCode complete-content locator has an invalid shape".into(),
        ));
    }
    let shape = OpenCodeCapturedShape::from_tag(locator.value()[0])?;
    let bytes: [u8; 8] = locator.value()[1..9].try_into().map_err(|_| {
        CaptureError::InvalidPayload(
            "OpenCode complete-content locator rowid has an invalid width".to_owned(),
        )
    })?;
    Ok((shape, unordered_i64(u64::from_be_bytes(bytes))))
}

fn unordered_i64(value: u64) -> i64 {
    (value ^ (1_u64 << 63)) as i64
}
