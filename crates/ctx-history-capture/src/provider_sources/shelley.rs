#[allow(unused_imports)]
use super::*;

pub(crate) const SHELLEY_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".config", "shelley", "shelley.db"],
    source_format: "shelley_sqlite",
    source_kind: ProviderSourceKind::NativeHistory,
}];
