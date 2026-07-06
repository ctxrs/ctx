#[allow(unused_imports)]
use super::*;

pub(crate) const OPENHANDS_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".openhands"],
    source_format: "openhands_file_events",
    source_kind: ProviderSourceKind::NativeHistory,
}];

pub(crate) fn has_openhands_event_json(root: &Path, max_entries: usize) -> BoundedProbe {
    has_json_file_under_matching(root, max_entries, |path| {
        path_has_component(path, "v1_conversations")
    })
}
