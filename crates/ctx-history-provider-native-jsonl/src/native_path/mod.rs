//! Source-backed discovery, parsing, and complete Core projection for native JSONL providers.

pub(crate) mod antigravity;
mod copilot;
mod factory_ai_droid;
mod grok_build;
mod model;
mod qoder;
mod qoder_parser;
mod qwen_code;
mod reader;
mod source_backed;
mod tabnine;

pub use antigravity::antigravity_source_backed_adapter;
pub use copilot::copilot_source_backed_adapter;
pub use factory_ai_droid::factory_droid_source_backed_adapter;
pub(crate) use factory_ai_droid::{
    enumerate_factory_droid_results, factory_droid_event_identity, factory_droid_event_text,
    factory_droid_event_type, factory_droid_file_is_selected, factory_droid_header_cwd,
    factory_droid_header_session_id, factory_droid_model, factory_droid_retry_discriminator,
    factory_droid_role, factory_droid_session_relationships,
};
pub use grok_build::grok_build_source_backed_adapter;
pub(crate) use model::{
    DirectJsonlEvent, DirectJsonlRejection, DirectJsonlRetryDiscriminator, DirectJsonlSession,
    DirectJsonlSourceRecord,
};
pub use qoder::qoder_source_backed_adapter;
pub(crate) use qwen_code::qwen_code_file_is_selected;
pub use qwen_code::qwen_code_source_backed_adapter;
pub use source_backed::DirectJsonlFamilyAdapter;
pub use tabnine::tabnine_source_backed_adapter;

pub(super) use grok_build::grok_build_file_is_selected;
