use super::*;
use ctx_history_provider_codex::codex::catalog::is_codex_session_rollout_path;

impl SourceBackedWatchCatalog {
    /// Proves that an existing unaliased file cannot be a member of one known
    /// Codex session-tree route. Callers must retain control, rescan and rearm
    /// events: this path observation alone cannot classify topology changes.
    pub fn event_is_known_non_source_file(
        &self,
        route: &SourceRouteIdentity,
        event: &Path,
    ) -> bool {
        let Some(targets) = self.routes.get(route) else {
            return false;
        };
        if targets.kind != Some(SourceBackedWatchTargetKind::Path)
            || targets.control.is_some()
            || is_codex_session_rollout_path(event)
        {
            return false;
        }
        let Some(registrations) = targets.registration_sources.as_ref() else {
            return false;
        };
        let mut roots = registrations
            .iter()
            .filter(|source| member_belongs_to_root(event, &source.source.path));
        let Some(root) = roots.next() else {
            return false;
        };
        if roots.next().is_some()
            || root.path_kind != RegisteredPathKind::Directory
            || root.source.provider != CaptureProvider::Codex
            || root.source.source_format != "codex_session_jsonl_tree"
            || !registration_source_is_available(root)
        {
            return false;
        }
        fs::symlink_metadata(event).is_ok_and(|metadata| is_unaliased_file(&metadata))
            && event
                .ancestors()
                .skip(1)
                .take_while(|parent| parent.starts_with(&root.source.path))
                .all(|parent| registered_path_kind(parent) == Some(RegisteredPathKind::Directory))
    }
}

#[cfg(unix)]
fn is_unaliased_file(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    // A non-rollout name can still be a hard link to an eligible rollout.
    metadata.file_type().is_file() && metadata.nlink() == 1
}

#[cfg(not(unix))]
fn is_unaliased_file(_metadata: &fs::Metadata) -> bool {
    // Without a metadata-only link-count proof, retain the event. In particular,
    // the Windows handle-based checks are not available to this path observer.
    false
}
