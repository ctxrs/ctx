//! Provider-family NativePath ingestion for direct native JSONL sources.
//!
//! The types in this module are provider-certified facts and page frontiers.
//! They publish directly without a normalized provider-import envelope.

mod antigravity;
mod copilot;
mod cursor;
mod cursor_provider;
mod driver;
mod factory_ai_droid;
mod gemini;
mod model;
mod publication;
mod qoder;
mod qoder_parser;
mod qwen_code;
mod reader;
mod source_backed;
mod tabnine;
mod windsurf;

// Registration seam for the central coordinator; provider modules do not own
// lifecycle or publication and therefore do not consume these exports here.
#[allow(unused_imports)]
pub(crate) use antigravity::{
    antigravity_source_backed_adapter, import_antigravity_nativepath_tree,
};
#[allow(unused_imports)]
pub(crate) use copilot::{copilot_source_backed_adapter, import_copilot_nativepath_tree};
pub(crate) use cursor::{
    committed_direct_jsonl_replay_authority, decode_direct_jsonl_cursor,
    decode_direct_jsonl_cursor_from_opened, decode_direct_jsonl_native_cursor,
    direct_jsonl_checkpoint_is_covered_by, direct_jsonl_cursor_matches_publication,
    encode_direct_jsonl_cursor, DirectJsonlCursorDecode,
};
pub(crate) use cursor_provider::import_cursor_nativepath_tree;
use driver::import_direct_native_jsonl_tree_core;
pub(crate) use driver::NativePathJsonlTreeImport;
#[allow(unused_imports)]
pub(crate) use factory_ai_droid::{
    enumerate_factory_droid_results, factory_droid_event_identity, factory_droid_event_text,
    factory_droid_event_type, factory_droid_file_is_selected, factory_droid_header_cwd,
    factory_droid_header_session_id, factory_droid_model, factory_droid_role,
    factory_droid_session_relationships, factory_droid_source_backed_adapter,
    import_factory_ai_droid_nativepath_tree,
};
pub(crate) use gemini::import_gemini_nativepath_tree;
pub(crate) use model::{
    DirectJsonlCheckpoint, DirectJsonlEvent, DirectJsonlFileObservation, DirectJsonlObservedTime,
    DirectJsonlOutput, DirectJsonlPage, DirectJsonlRejection, DirectJsonlScanOutcome,
    DirectJsonlSession, DirectJsonlSourceChange, DirectJsonlSourceRecord, DirectJsonlTouch,
    DIRECT_JSONL_NATIVEPATH_PARSER_REVISION, DIRECT_JSONL_NATIVEPATH_POLICY_REVISION,
};
pub(crate) use publication::{
    publish_direct_jsonl_group, DirectJsonlPendingPage, DirectJsonlPublicationContext,
};
#[allow(unused_imports)]
pub(crate) use qoder::{import_qoder_nativepath_tree, qoder_source_backed_adapter};
pub(crate) use qoder_parser::qoder_complete_content_message_record;
#[allow(unused_imports)]
pub(crate) use qwen_code::{
    import_qwen_code_nativepath_tree, qwen_code_file_is_selected, qwen_code_source_backed_adapter,
};
pub(crate) use reader::{
    direct_jsonl_complete_message_provider_event_hash, open_direct_jsonl_pages,
};
#[allow(unused_imports)]
pub(crate) use source_backed::{
    DirectJsonlCertifiedLeaf, DirectJsonlInventoryFailure, DirectJsonlInventoryLeaf,
    DirectJsonlSourceAdapter, DirectJsonlSourceBackedError, DirectJsonlSourceBackedResult,
    DirectJsonlSourceInventory, DirectJsonlSourcePage, DirectJsonlSourceReader,
};
#[allow(unused_imports)]
pub(crate) use tabnine::{import_tabnine_nativepath_tree, tabnine_source_backed_adapter};
#[allow(unused_imports)]
pub(crate) use windsurf::{
    import_windsurf_nativepath_tree, windsurf_event_role, windsurf_event_text, windsurf_event_type,
    windsurf_source_backed_adapter,
};

#[cfg(test)]
mod tests;
