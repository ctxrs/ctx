#[allow(unused_imports)]
use super::*;

pub(crate) const ANTIGRAVITY_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[".gemini", "antigravity-cli", "brain"],
        source_format: "antigravity_cli_transcript_jsonl_tree",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[".gemini", "antigravity-ide", "brain"],
        source_format: "antigravity_cli_transcript_jsonl_tree",
        source_kind: ProviderSourceKind::NativeHistory,
    },
];
