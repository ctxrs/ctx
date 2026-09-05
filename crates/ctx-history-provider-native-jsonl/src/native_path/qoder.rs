use ctx_history_core::CaptureProvider;

use crate::{NativeJsonlRuntime, QODER_SOURCE_FORMAT};

const PARSER_REVISION: &str = "direct-native-jsonl-parser-v7-record-admission-order";

pub const fn qoder_source_backed_adapter<R: NativeJsonlRuntime>(
) -> super::DirectJsonlFamilyAdapter<R> {
    super::DirectJsonlFamilyAdapter::new(
        CaptureProvider::Qoder,
        QODER_SOURCE_FORMAT,
        "qoder-direct-native-jsonl-v1",
        PARSER_REVISION,
    )
}
