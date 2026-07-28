use std::path::Path;

use ctx_history_core::EventType;
use ctx_history_store::Store;

use crate::{ProviderAdapterContext, ProviderImportOptions, ProviderImportSummary, Result};

mod nativepath;
mod production;
mod schema;
mod source_backed;
mod wire;

#[allow(unused_imports)]
// Provider-local source-backed seam, intentionally awaiting central registration.
pub(crate) use source_backed::{
    project_selected_warp_sources_v0, project_warp_source_backed_v0, resolve_warp_locator_v0,
    WarpHydratedRecordV0, WarpSourceBackedErrorV0, WarpSourceBackedSnapshotV0,
    WarpSourceSelectionV0,
};

pub(crate) struct WarpTaskContent {
    pub(crate) event_type: EventType,
    pub(crate) native_record_id: String,
    pub(crate) text: String,
    pub(crate) normalized_payload_hash: Option<String>,
}

pub(crate) fn warp_message_content_at(
    task_bytes: &[u8],
    conversation_id: &str,
    fallback_task_id: &str,
    message_index: usize,
) -> Result<Option<WarpTaskContent>> {
    nativepath::resolve_warp_task_message(
        task_bytes,
        conversation_id,
        fallback_task_id,
        message_index,
    )
}

pub(crate) fn import_warp_nativepath(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    production::import_warp_nativepath(path, store, context, import_options)
}

#[cfg(test)]
#[path = "warp/production_tests.rs"]
mod production_tests;
#[cfg(test)]
#[path = "warp/source_backed_tests.rs"]
mod source_backed_tests;
