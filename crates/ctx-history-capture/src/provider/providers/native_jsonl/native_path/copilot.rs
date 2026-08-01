use ctx_history_core::CaptureProvider;
use serde_json::Value;

use crate::COPILOT_CLI_SOURCE_FORMAT;

pub(crate) const fn copilot_source_backed_adapter() -> super::DirectJsonlFamilyAdapter {
    super::DirectJsonlFamilyAdapter::new(
        CaptureProvider::CopilotCli,
        COPILOT_CLI_SOURCE_FORMAT,
        "copilot-cli-direct-native-jsonl-v1",
    )
}

pub(super) fn copilot_event_identity(value: &Value) -> Option<&str> {
    value
        .get("id")
        .and_then(Value::as_str)
        .filter(|event_id| !event_id.trim().is_empty())
}
