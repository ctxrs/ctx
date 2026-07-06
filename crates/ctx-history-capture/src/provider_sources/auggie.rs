#[allow(unused_imports)]
use super::*;

pub(crate) const AUGGIE_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".augment", "sessions"],
    source_format: "auggie_session_json",
    source_kind: ProviderSourceKind::NativeHistory,
}];
