use std::path::Path;

use ctx_history_store::Store;

use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportOptions, ProviderImportSummary, Result,
};

pub(crate) mod source_backed;

/// Rejects the released Store-ingestion entrypoint while shared dispatch still
/// carries its historical signature. AstrBot ingestion is source-backed.
pub(super) fn import_astrbot_native_path(
    _path: &Path,
    _store: &mut Store,
    _context: ProviderAdapterContext,
    _options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    Err(CaptureError::InvalidPayload(
        "AstrBot Store ingestion was removed; use source-backed ingestion".to_owned(),
    ))
}
