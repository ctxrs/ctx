use ctx_history_core::CaptureProvider;

use crate::ROO_TASK_JSON_SOURCE_FORMAT;

#[derive(Debug, Clone, Copy)]
pub(crate) struct TaskJsonProviderSpec {
    pub(crate) provider: CaptureProvider,
    pub(crate) source_format: &'static str,
    pub(crate) api_file: &'static str,
    pub(crate) ui_file: &'static str,
    pub(crate) fallback_api_file: Option<&'static str>,
}

/// Returns the read-only descriptor used to hydrate historical Roo locators.
///
/// Production capture does not use this descriptor.
pub(crate) fn task_json_provider(_provider: CaptureProvider) -> TaskJsonProviderSpec {
    TaskJsonProviderSpec {
        provider: CaptureProvider::RooCode,
        source_format: ROO_TASK_JSON_SOURCE_FORMAT,
        api_file: "api_conversation_history.json",
        ui_file: "ui_messages.json",
        fallback_api_file: Some("claude_messages.json"),
    }
}
