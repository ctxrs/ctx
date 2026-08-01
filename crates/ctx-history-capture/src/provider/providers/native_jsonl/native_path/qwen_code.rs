use std::path::Path;

use ctx_history_core::{CaptureProvider, EventRole, EventType};
use serde_json::Value;

use crate::{
    provider::normalization::{
        provider_output_event_is_failure, provider_role, provider_value_text,
    },
    OutputOutcome, OutputOutcomeMetadata, QWEN_CODE_SOURCE_FORMAT,
};

use super::super::result_content::{
    extract_direct_result_content, NativeJsonlResultExtractionError, NativeJsonlResultSubrecord,
};

pub(crate) const fn qwen_code_source_backed_adapter() -> super::DirectJsonlFamilyAdapter {
    super::DirectJsonlFamilyAdapter::new(
        CaptureProvider::QwenCode,
        QWEN_CODE_SOURCE_FORMAT,
        "qwen-code-direct-native-jsonl-v1",
    )
}

pub(crate) fn qwen_code_event_identity(value: &Value) -> Option<&str> {
    value
        .get("id")
        .or_else(|| value.get("uuid"))
        .and_then(Value::as_str)
        .filter(|event_id| !event_id.trim().is_empty())
}

pub(crate) fn qwen_code_file_is_selected(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("jsonl")
        && path
            .components()
            .any(|component| component.as_os_str() == "chats")
}

#[path = "qwen_code_records.rs"]
mod records;
pub(super) use records::{
    enumerate_qwen_code_results, qwen_code_event_text, qwen_code_event_type, qwen_code_header_cwd,
    qwen_code_header_session_id, qwen_code_model, qwen_code_role,
};
