#[allow(unused_imports)]
use super::*;

pub(crate) const MISTRAL_VIBE_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".vibe", "logs", "session"],
    source_format: "mistral_vibe_session_jsonl_tree",
    source_kind: ProviderSourceKind::NativeHistory,
}];
