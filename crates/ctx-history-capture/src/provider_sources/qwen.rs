#[allow(unused_imports)]
use super::*;

pub(crate) const QWEN_CODE_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".qwen", "projects"],
    source_format: "qwen_code_chat_jsonl_tree",
    source_kind: ProviderSourceKind::NativeHistory,
}];
