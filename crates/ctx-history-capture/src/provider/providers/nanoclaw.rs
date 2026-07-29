use std::path::Path;

use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportOptions, ProviderImportSummary, Result,
};

pub(crate) mod native_path;
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

// These revisions remain the released NanoClaw semantic contract.
const NANOCLAW_CAPTURE_REVISION: u32 = 2;
const NANOCLAW_POLICY_REVISION: u32 = 4;
pub(crate) const NANOCLAW_MESSAGE_LOCATOR_KIND: &str = "nanoclaw-project-message-v1";

/// Temporary compatibility entry point for shared v0.25 import APIs.
///
/// NanoClaw production ingestion is source-backed. The legacy Store publisher
/// was deleted provider-locally and must not be reintroduced behind this symbol.
pub(crate) fn import_nanoclaw_nativepath(
    _path: &Path,
    _store: &mut ctx_history_store::Store,
    _context: ProviderAdapterContext,
    _import_options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    Err(CaptureError::UnsupportedSchema(
        "NanoClaw legacy Store publication is unavailable; use source-backed ingestion".to_owned(),
    ))
}
