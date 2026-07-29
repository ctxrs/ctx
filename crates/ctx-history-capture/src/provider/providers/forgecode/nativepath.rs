use std::path::Path;

use ctx_history_store::Store;

use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportOptions, ProviderImportSummary, Result,
};

pub(super) mod source;
pub(crate) mod source_backed;

/// Rejects the released Store-ingestion entrypoint while shared dispatch still
/// carries its historical signature. ForgeCode ingestion is source-backed.
pub(super) fn import_forgecode_nativepath(
    _path: &Path,
    _store: &mut Store,
    _context: ProviderAdapterContext,
    _import_options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    Err(CaptureError::InvalidPayload(
        "ForgeCode Store ingestion was removed; use source-backed ingestion".to_owned(),
    ))
}
