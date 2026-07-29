use std::path::Path;

use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportOptions, ProviderImportSummary, Result,
};

mod event;
pub(crate) mod native_path;
mod source;

/// Rejects the historical Store publisher while shared v0.25 dispatch still
/// carries its signature. RovoDev production ingestion is source-backed.
pub(crate) fn import_rovodev_native_path(
    _path: &Path,
    _store: &mut ctx_history_store::Store,
    _context: ProviderAdapterContext,
    _options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    Err(CaptureError::UnsupportedSchema(
        "RovoDev Store ingestion was removed; use source-backed ingestion".to_owned(),
    ))
}
