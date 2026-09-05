use ctx_history_core::CaptureProvider;

use crate::{NativeJsonlRuntime, ANTIGRAVITY_CLI_SOURCE_FORMAT};

const PARSER_REVISION: &str = "direct-native-jsonl-parser-v7-record-admission-order";

pub const fn antigravity_source_backed_adapter<R: NativeJsonlRuntime>(
) -> super::DirectJsonlFamilyAdapter<R> {
    super::DirectJsonlFamilyAdapter::new(
        CaptureProvider::Antigravity,
        ANTIGRAVITY_CLI_SOURCE_FORMAT,
        "antigravity-direct-native-jsonl-v1",
        PARSER_REVISION,
    )
}
