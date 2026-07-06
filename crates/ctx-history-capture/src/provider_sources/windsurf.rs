#[allow(unused_imports)]
use super::*;

pub(crate) const WINDSURF_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".windsurf", "transcripts"],
    source_format: "windsurf_cascade_hook_transcript_jsonl_tree",
    source_kind: ProviderSourceKind::NativeHistory,
}];
