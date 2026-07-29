use std::path::Path;

use ctx_history_core::CaptureProvider;

use crate::Result;

mod dialect;
pub(crate) mod native_path;
mod normalization;
pub(crate) mod result_content;
mod traversal;

pub(crate) use dialect::native_jsonl_missing_reason;
pub(crate) use native_path::{
    direct_jsonl_complete_message_provider_event_hash, qoder_complete_content_message_record,
};
pub(crate) use normalization::{
    native_jsonl_entry_type, native_jsonl_event_id, native_jsonl_event_text,
    native_jsonl_event_type, native_jsonl_normalized_payload, native_jsonl_timestamp,
};

pub(crate) fn visit_native_jsonl_files(
    root: &Path,
    provider: CaptureProvider,
    visit: &mut dyn FnMut(&Path) -> Result<()>,
) -> Result<usize> {
    traversal::visit_jsonl_tree_files(provider, root, &mut |source_file| visit(source_file.path()))
}
#[cfg(test)]
mod tests;
