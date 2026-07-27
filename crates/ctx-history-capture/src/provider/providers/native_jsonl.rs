use std::path::Path;

use ctx_history_core::CaptureProvider;

use crate::Result;

mod dialect;
mod native_path;
mod normalization;
pub(crate) mod result_content;
mod traversal;

pub(crate) use dialect::native_jsonl_missing_reason;
pub(crate) use native_path::{
    direct_jsonl_complete_message_provider_event_hash, import_antigravity_nativepath_tree,
    import_copilot_nativepath_tree, import_cursor_nativepath_tree,
    import_factory_ai_droid_nativepath_tree, import_gemini_nativepath_tree,
    import_qoder_nativepath_tree, import_qwen_code_nativepath_tree, import_tabnine_nativepath_tree,
    import_windsurf_nativepath_tree, qoder_complete_content_message_record,
    NativePathJsonlTreeImport,
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
    traversal::visit_jsonl_tree_files(
        root,
        &|path| dialect::native_jsonl_file_is_selected(provider, path),
        visit,
    )
}
#[cfg(test)]
mod tests;
