use ctx_history_core::CaptureProvider;

use crate::ANTIGRAVITY_CLI_SOURCE_FORMAT;

pub(crate) const fn antigravity_source_backed_adapter() -> super::DirectJsonlFamilyAdapter {
    super::DirectJsonlFamilyAdapter::new(
        CaptureProvider::Antigravity,
        ANTIGRAVITY_CLI_SOURCE_FORMAT,
        "antigravity-direct-native-jsonl-v1",
    )
}

#[cfg(test)]
#[path = "antigravity_tests.rs"]
mod tests;
