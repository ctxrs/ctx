#[allow(unused_imports)]
use super::*;

pub(crate) const KIRO_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[".local", "share", "kiro-cli", "data.sqlite3"],
        source_format: "kiro_cli_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &["Library", "Application Support", "kiro-cli", "data.sqlite3"],
        source_format: "kiro_cli_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
];
