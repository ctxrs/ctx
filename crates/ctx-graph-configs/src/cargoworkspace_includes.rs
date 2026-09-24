use super::*;

impl CargoWorkspace {
    pub(super) fn includes(&self, workspace: &str, manifest: &str) -> bool {
        workspace == manifest
            || (self.members.iter().any(|g| g.is_match(directory(manifest)))
                && !self.exclude.iter().any(|g| g.is_match(directory(manifest))))
    }
}
