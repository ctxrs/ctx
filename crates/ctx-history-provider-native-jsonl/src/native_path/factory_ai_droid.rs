use std::path::Path;

use ctx_history_core::{
    AgentScope, CaptureProvider, EventRole, EventType, ProviderNativeSessionRelationship,
};
use serde_json::Value;

use ctx_history_capture_model::normalization::{provider_role, provider_value_text};

use crate::{NativeJsonlRuntime, FACTORY_DROID_SOURCE_FORMAT};

use super::super::result_content::{
    extract_direct_result_content, NativeJsonlResultExtractionError, NativeJsonlResultSubrecord,
};

const PARSER_REVISION: &str = "direct-native-jsonl-parser-v7-record-admission-order";

pub const fn factory_droid_source_backed_adapter<R: NativeJsonlRuntime>(
) -> super::DirectJsonlFamilyAdapter<R> {
    super::DirectJsonlFamilyAdapter::new(
        CaptureProvider::FactoryAiDroid,
        FACTORY_DROID_SOURCE_FORMAT,
        "factory-droid-direct-native-jsonl-v1",
        PARSER_REVISION,
    )
}

#[path = "factory_ai_droid_records.rs"]
mod records;
pub(crate) use records::{
    enumerate_factory_droid_results, factory_droid_event_identity, factory_droid_event_text,
    factory_droid_event_type, factory_droid_file_is_selected, factory_droid_header_cwd,
    factory_droid_header_session_id, factory_droid_model, factory_droid_retry_discriminator,
    factory_droid_role, factory_droid_session_relationships,
};
