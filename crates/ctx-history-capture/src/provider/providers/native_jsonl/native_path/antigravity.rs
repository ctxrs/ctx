use ctx_history_core::CaptureProvider;

use crate::ANTIGRAVITY_CLI_SOURCE_FORMAT;

const PARSER_REVISION: &str = "direct-native-jsonl-parser-v4";

pub(crate) const fn antigravity_source_backed_adapter() -> super::DirectJsonlFamilyAdapter {
    super::DirectJsonlFamilyAdapter::new(
        CaptureProvider::Antigravity,
        ANTIGRAVITY_CLI_SOURCE_FORMAT,
        "antigravity-direct-native-jsonl-v1",
        PARSER_REVISION,
    )
}
