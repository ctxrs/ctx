use std::path::Path;

use ctx_history_core::EventType;
use ctx_history_store::Store;

use crate::{ProviderAdapterContext, ProviderImportOptions, ProviderImportSummary, Result};

mod nativepath;
mod production;
mod proto;
mod schema;
mod wire;

pub(crate) struct WarpTaskContent {
    pub(crate) event_type: EventType,
    pub(crate) native_record_id: String,
    pub(crate) text: String,
}

/// Pure provider-local reopening boundary shared by capture and SQLite
/// resolution. It never treats Warp's synthetic tool labels as source-backed
/// content.
pub(crate) fn warp_task_content_at(
    task_bytes: &[u8],
    fallback_task_id: &str,
    message_index: usize,
) -> Result<Option<WarpTaskContent>> {
    let task = proto::warp_decode_task(task_bytes)?;
    let task_id = if task.id.is_empty() {
        fallback_task_id
    } else {
        &task.id
    };
    let Some(message) = task.messages.get(message_index) else {
        return Ok(None);
    };
    let Some(text) = message.complete_text.clone() else {
        return Ok(None);
    };
    let native_record_id = if message.id.is_empty() {
        format!("{task_id}:{message_index}")
    } else {
        message.id.clone()
    };
    Ok(Some(WarpTaskContent {
        event_type: message.event_type,
        native_record_id,
        text,
    }))
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
