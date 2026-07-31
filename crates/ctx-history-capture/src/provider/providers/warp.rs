use ctx_history_core::EventType;

use crate::Result;

mod nativepath;
mod schema;
mod source_backed;
mod wire;

pub(crate) use source_backed::{project_warp_source_backed_v0, WarpSourceSelectionV0};

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

#[cfg(test)]
#[path = "warp/source_backed_tests.rs"]
mod source_backed_tests;
