use std::path::Path;

use ctx_history_store::Store;

use crate::{ProviderAdapterContext, ProviderImportOptions, ProviderImportSummary, Result};

mod complete_content;
mod event;
mod nativepath;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) fn forgecode_text_message_text(
    body: &serde_json::Value,
    event_type: ctx_history_core::EventType,
) -> String {
    event::forgecode_text_message_text(body, event_type)
}

pub(crate) fn import_forgecode_nativepath(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    nativepath::import_forgecode_nativepath(path, store, context, import_options)
}
pub(crate) use complete_content::{forgecode_complete_message, load_forgecode_conversation_values};
