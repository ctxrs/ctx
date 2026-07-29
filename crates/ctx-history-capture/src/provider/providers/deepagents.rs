use std::path::Path;

use ctx_history_store::Store;

use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportOptions, ProviderImportSummary, Result,
};

mod complete_content;
mod message;
pub(crate) mod native_path;
mod source;

pub(crate) use complete_content::{
    decode_deepagents_content_address, resolve_deepagents_content,
    validate_deepagents_content_schema, DeepAgentsContentAddress, DEEPAGENTS_CONTENT_LOCATOR_KIND,
};

pub(crate) fn import_deepagents_nativepath(
    _path: &Path,
    _store: &mut Store,
    _context: ProviderAdapterContext,
    _import_options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    Err(CaptureError::UnsupportedSchema(
        "Deep Agents Store ingestion was removed; use source-backed ingestion".to_owned(),
    ))
}

#[cfg(test)]
#[path = "deepagents/tests.rs"]
mod tests;
