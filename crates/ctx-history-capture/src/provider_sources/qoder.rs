#[allow(unused_imports)]
use super::*;

pub(crate) const QODER_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".qoder", "projects"],
    source_format: "qoder_transcript_jsonl_tree",
    source_kind: ProviderSourceKind::NativeHistory,
}];
