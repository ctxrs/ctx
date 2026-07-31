use std::path::Path;

use ctx_history_core::{AgentType, CaptureProvider, EventRole, EventType};
use serde_json::Value;

use crate::{
    provider::normalization::{
        provider_output_event_is_failure, provider_role, provider_value_text,
    },
    OutputOutcome, OutputOutcomeMetadata, FACTORY_DROID_SOURCE_FORMAT,
};

use super::super::result_content::{NativeJsonlResultExtractionError, NativeJsonlResultSubrecord};

pub(crate) const fn factory_droid_source_backed_adapter() -> super::DirectJsonlFamilyAdapter {
    super::DirectJsonlFamilyAdapter::new(
        CaptureProvider::FactoryAiDroid,
        FACTORY_DROID_SOURCE_FORMAT,
        "factory-droid-direct-native-jsonl-v1",
    )
}

#[path = "factory_ai_droid_records.rs"]
mod records;
pub(in crate::provider::providers::native_jsonl) use records::{
    enumerate_factory_droid_results, factory_droid_event_identity, factory_droid_event_text,
    factory_droid_event_type, factory_droid_file_is_selected, factory_droid_header_cwd,
    factory_droid_header_session_id, factory_droid_model, factory_droid_role,
    factory_droid_session_relationships,
};

#[cfg(test)]
#[path = "factory_ai_droid_tests.rs"]
mod tests;
