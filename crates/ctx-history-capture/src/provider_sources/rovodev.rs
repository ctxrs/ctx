#[allow(unused_imports)]
use super::*;

pub(crate) const ROVODEV_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".rovodev", "sessions"],
    source_format: "rovodev_session_json_tree",
    source_kind: ProviderSourceKind::NativeHistory,
}];
