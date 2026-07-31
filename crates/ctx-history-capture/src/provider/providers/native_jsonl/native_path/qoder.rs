use ctx_history_core::CaptureProvider;

use crate::QODER_SOURCE_FORMAT;

pub(crate) const fn qoder_source_backed_adapter() -> super::DirectJsonlFamilyAdapter {
    super::DirectJsonlFamilyAdapter::new(
        CaptureProvider::Qoder,
        QODER_SOURCE_FORMAT,
        "qoder-direct-native-jsonl-v1",
    )
}
