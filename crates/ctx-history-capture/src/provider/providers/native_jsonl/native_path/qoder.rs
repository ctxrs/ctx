use ctx_history_core::CaptureProvider;

use crate::QODER_SOURCE_FORMAT;

const PARSER_REVISION: &str = "direct-native-jsonl-parser-v4";

pub(crate) const fn qoder_source_backed_adapter() -> super::DirectJsonlFamilyAdapter {
    super::DirectJsonlFamilyAdapter::new(
        CaptureProvider::Qoder,
        QODER_SOURCE_FORMAT,
        "qoder-direct-native-jsonl-v1",
        PARSER_REVISION,
    )
}
