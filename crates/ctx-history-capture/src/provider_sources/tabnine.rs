#[allow(unused_imports)]
use super::*;

pub(crate) const TABNINE_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".tabnine", "agent"],
    source_format: "tabnine_cli_chat_recording_jsonl",
    source_kind: ProviderSourceKind::NativeHistory,
}];
