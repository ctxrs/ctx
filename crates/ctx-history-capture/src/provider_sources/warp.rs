#[allow(unused_imports)]
use super::*;

pub(crate) const WARP_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[".local", "state", "warp-terminal", "warp.sqlite"],
        source_format: "warp_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            "Library",
            "Group Containers",
            "2BBY89MBSN.dev.warp",
            "Library",
            "Application Support",
            "dev.warp.Warp-Stable",
            "warp.sqlite",
        ],
        source_format: "warp_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
];
