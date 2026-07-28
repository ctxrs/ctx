use std::path::Path;

use ctx_history_store::Store;

use crate::{ProviderAdapterContext, ProviderImportOptions, ProviderImportSummary, Result};

mod complete_content;
mod message;
pub(crate) mod native_path;
mod source;

pub(crate) use complete_content::{
    decode_deepagents_content_address, resolve_deepagents_content,
    validate_deepagents_content_schema, DeepAgentsContentAddress, DEEPAGENTS_CONTENT_LOCATOR_KIND,
};

pub(crate) fn import_deepagents_nativepath(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    native_path::import_deepagents_sqlite_nativepath(path, store, context, import_options)
}

#[cfg(test)]
#[path = "deepagents/tests.rs"]
mod tests;
