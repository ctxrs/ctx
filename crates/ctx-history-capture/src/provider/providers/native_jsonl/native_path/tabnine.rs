use ctx_history_core::CaptureProvider;
use serde_json::Value;

use crate::TABNINE_CLI_SOURCE_FORMAT;

pub(crate) const fn tabnine_source_backed_adapter() -> super::DirectJsonlSourceAdapter {
    super::DirectJsonlSourceAdapter::new(
        CaptureProvider::Tabnine,
        TABNINE_CLI_SOURCE_FORMAT,
        "tabnine-direct-native-jsonl-v1",
    )
}

pub(super) fn tabnine_event_identity(value: &Value) -> Option<&str> {
    value
        .get("id")
        .and_then(Value::as_str)
        .filter(|event_id| !event_id.trim().is_empty())
}

#[cfg(test)]
#[path = "tabnine_tests.rs"]
mod tests;
