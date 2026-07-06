#[allow(unused_imports)]
use super::*;

pub(crate) const GOOSE_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[".local", "share", "goose", "sessions", "sessions.db"],
        source_format: "goose_sessions_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            ".local",
            "share",
            "Block",
            "goose",
            "sessions",
            "sessions.db",
        ],
        source_format: "goose_sessions_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
];

pub(crate) fn goose_db_source(spec: &ProviderSourceSpec, path: PathBuf) -> ProviderSource {
    provider_source_from_parts(
        spec,
        path,
        "goose_sessions_sqlite",
        ProviderSourceKind::NativeHistory,
    )
}
