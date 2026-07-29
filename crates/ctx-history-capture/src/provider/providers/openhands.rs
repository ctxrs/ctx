use std::path::Path;

use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportOptions, ProviderImportSummary, Result,
};

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

/// Rejects the historical Store publisher while shared v0.25 dispatch still
/// carries its signature. OpenHands production ingestion is source-backed.
pub(crate) fn import_openhands_nativepath(
    _path: &Path,
    _store: &mut ctx_history_store::Store,
    _context: ProviderAdapterContext,
    _options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    Err(CaptureError::UnsupportedSchema(
        "OpenHands Store ingestion was removed; use source-backed ingestion".to_owned(),
    ))
}

fn openhands_bounded_derived_text(value: String, field: &str) -> Result<String> {
    const MAX_DERIVED_TEXT_BYTES: usize = 16 * 1024;
    if value.len() > MAX_DERIVED_TEXT_BYTES {
        return Err(crate::CaptureError::InvalidPayload(format!(
            "OpenHands {field} exceeds {MAX_DERIVED_TEXT_BYTES} bytes"
        )));
    }
    Ok(value)
}
