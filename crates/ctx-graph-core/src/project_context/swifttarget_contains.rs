use super::*;

impl SwiftTarget {
    pub(super) fn contains(&self, path: &str) -> bool {
        let name = path.rsplit('/').next().unwrap();
        within(path, &self.root)
            && name != "Package.swift"
            && !name.starts_with("Package@swift-")
            && !path.split('/').any(|part| part.starts_with('.'))
            && self
                .sources
                .as_ref()
                .is_none_or(|sources| sources.iter().any(|s| path == s || within(path, s)))
            && !self.exclude.iter().any(|s| path == s || within(path, s))
    }
}
