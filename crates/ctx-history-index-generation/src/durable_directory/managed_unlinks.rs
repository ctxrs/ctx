use super::*;

impl DurableMmapDirectory {
    /// Only the authenticated clone owner attaches candidate unlink authority.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn with_managed_unlinks(
        mut self,
        root: &Path,
        pointer: &crate::ActiveGenerationPointer,
        candidate: &str,
    ) -> Self {
        self.managed_unlinks = Some(Arc::new(
            crate::certification::managed_links::CandidateUnlinks::new(root, pointer, candidate),
        ));
        self
    }

    pub(super) fn delete_with_certifications(&self, path: &Path) -> Result<(), DeleteError> {
        match &self.inner {
            DurableDirectoryBackend::Mmap(inner) => match &self.managed_unlinks {
                Some(owner) => owner.delete(path, || inner.delete(path)),
                None => inner.delete(path),
            },
            DurableDirectoryBackend::Anchored(_) => Err(DeleteError::IoError {
                io_error: Arc::new(read_only_directory_error()),
                filepath: path.to_path_buf(),
            }),
        }
    }
}
