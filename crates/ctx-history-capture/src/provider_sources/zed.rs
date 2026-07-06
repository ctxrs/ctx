#[allow(unused_imports)]
use super::*;

pub(crate) const ZED_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".local", "share", "zed", "threads", "threads.db"],
    source_format: "zed_threads_sqlite",
    source_kind: ProviderSourceKind::NativeHistory,
}];
