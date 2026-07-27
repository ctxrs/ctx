use std::path::Path;

use ctx_history_store::Store;

use crate::{ProviderAdapterContext, ProviderImportOptions, ProviderImportSummary, Result};

mod native_path;
mod position;
mod project;
mod projection;
mod rows;
mod source;

mod complete_content;
pub(crate) use complete_content::{selected_component_addresses, NanoClawCompleteProject};
pub(crate) use position::decode_nanoclaw_message_locator;

#[cfg(test)]
#[path = "nanoclaw/tests.rs"]
mod tests;

// These revisions remain the released NanoClaw semantic contract. NativePath
// owns a distinct cursor version and accepts old pre-NativePath cursors only as
// migration input.
const NANOCLAW_CAPTURE_REVISION: u32 = 2;
const NANOCLAW_POLICY_REVISION: u32 = 4;
pub(crate) const NANOCLAW_MESSAGE_LOCATOR_KIND: &str = "nanoclaw-project-message-v1";

pub(crate) fn import_nanoclaw_nativepath(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    native_path::import_nanoclaw_project(path, store, context, import_options)
}
