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
mod tabnine;
mod windsurf;

pub(crate) use antigravity::import_antigravity_nativepath_tree;
pub(crate) use copilot::import_copilot_nativepath_tree;
pub(crate) use cursor::{
    committed_direct_jsonl_replay_authority, decode_direct_jsonl_cursor,
    decode_direct_jsonl_native_cursor, direct_jsonl_checkpoint_is_covered_by,
    direct_jsonl_cursor_matches_publication, encode_direct_jsonl_cursor, DirectJsonlCursorDecode,
};
pub(crate) use cursor_provider::import_cursor_nativepath_tree;
use driver::import_direct_native_jsonl_tree_core;
pub(crate) use driver::NativePathJsonlTreeImport;
pub(crate) use factory_ai_droid::{
    enumerate_factory_droid_results, factory_droid_event_identity, factory_droid_event_text,
    factory_droid_event_type, factory_droid_file_is_selected, factory_droid_header_cwd,
    factory_droid_header_session_id, factory_droid_model, factory_droid_role,
    factory_droid_session_relationships, import_factory_ai_droid_nativepath_tree,
};
pub(crate) use gemini::import_gemini_nativepath_tree;
pub(crate) use model::{
    DirectJsonlCheckpoint, DirectJsonlEvent, DirectJsonlFileObservation, DirectJsonlObservedTime,
    DirectJsonlOutput, DirectJsonlPage, DirectJsonlRejection, DirectJsonlScanOutcome,
    DirectJsonlSession, DirectJsonlSourceChange, DirectJsonlTouch,
    DIRECT_JSONL_NATIVEPATH_PARSER_REVISION, DIRECT_JSONL_NATIVEPATH_POLICY_REVISION,
};
pub(crate) use publication::{
    publish_direct_jsonl_group, DirectJsonlPendingPage, DirectJsonlPublicationContext,
};
pub(crate) use qoder::import_qoder_nativepath_tree;
pub(crate) use qoder_parser::qoder_complete_content_message_record;
pub(crate) use qwen_code::{
    import_qwen_code_nativepath_tree, qwen_code_event_identity, qwen_code_file_is_selected,
};
pub(crate) use reader::open_direct_jsonl_pages;
pub(crate) use tabnine::import_tabnine_nativepath_tree;
pub(crate) use windsurf::{
    import_windsurf_nativepath_tree, windsurf_event_role, windsurf_event_text, windsurf_event_type,
};

#[cfg(test)]
mod tests;
