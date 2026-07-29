//! Source-backed discovery, parsing, and exact hydration for native JSONL providers.

mod antigravity;
mod copilot;
mod factory_ai_droid;
mod model;
mod qoder;
mod qoder_parser;
mod qwen_code;
mod reader;
mod source_backed;
mod tabnine;
mod windsurf;

#[allow(unused_imports)]
pub(crate) use antigravity::antigravity_source_backed_adapter;
#[allow(unused_imports)]
pub(crate) use copilot::copilot_source_backed_adapter;
#[allow(unused_imports)]
pub(crate) use factory_ai_droid::{
    enumerate_factory_droid_results, factory_droid_event_identity, factory_droid_event_text,
    factory_droid_event_type, factory_droid_file_is_selected, factory_droid_header_cwd,
    factory_droid_header_session_id, factory_droid_model, factory_droid_role,
    factory_droid_session_relationships, factory_droid_source_backed_adapter,
};
pub(crate) use model::{
    DirectJsonlCheckpoint, DirectJsonlEvent, DirectJsonlFileObservation, DirectJsonlObservedTime,
    DirectJsonlPage, DirectJsonlRejection, DirectJsonlScanOutcome, DirectJsonlSession,
    DirectJsonlSourceChange, DirectJsonlSourceRecord, DirectJsonlTouch,
    DIRECT_JSONL_NATIVEPATH_PARSER_REVISION, DIRECT_JSONL_NATIVEPATH_POLICY_REVISION,
};
#[allow(unused_imports)]
pub(crate) use qoder::qoder_source_backed_adapter;
pub(crate) use qoder_parser::qoder_complete_content_message_record;
#[allow(unused_imports)]
pub(crate) use qwen_code::{qwen_code_file_is_selected, qwen_code_source_backed_adapter};
pub(crate) use reader::direct_jsonl_complete_message_provider_event_hash;
pub(crate) use source_backed::registration::register as register_source_backed_route;
#[allow(unused_imports)]
pub(crate) use source_backed::{
    DirectJsonlCertifiedLeaf, DirectJsonlInventoryFailure, DirectJsonlInventoryLeaf,
    DirectJsonlSourceAdapter, DirectJsonlSourceBackedError, DirectJsonlSourceBackedResult,
    DirectJsonlSourceInventory, DirectJsonlSourcePage, DirectJsonlSourceReader,
};
#[allow(unused_imports)]
pub(crate) use tabnine::tabnine_source_backed_adapter;
#[allow(unused_imports)]
pub(crate) use windsurf::{
    windsurf_event_role, windsurf_event_text, windsurf_event_type, windsurf_source_backed_adapter,
};
