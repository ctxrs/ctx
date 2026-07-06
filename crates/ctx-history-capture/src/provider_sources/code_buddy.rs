#[allow(unused_imports)]
use super::*;

pub(crate) const CODEBUDDY_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[".codebuddy"],
        source_format: "codebuddy_history_json",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            "Library",
            "Application Support",
            "CodeBuddyExtension",
            "Data",
        ],
        source_format: "codebuddy_history_json",
        source_kind: ProviderSourceKind::NativeHistory,
    },
];

pub(crate) fn has_codebuddy_history_json(root: &Path, max_entries: usize) -> BoundedProbe {
    has_json_file_under_matching(root, max_entries, |path| {
        path.file_name().and_then(|name| name.to_str()) == Some("index.json")
            && path_has_component(path, "history")
    })
}
