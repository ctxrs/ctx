use super::*;

#[allow(
    dead_code,
    reason = "provider adapters migrate to this shared authority API in follow-up slices"
)]
impl ProviderSourceDirectory {
    /// Reads one bounded name snapshot. Later independent additions may be
    /// deferred by inventory owners, but mutation during enumeration is fatal.
    pub fn visit_entries_snapshot<E>(
        &self,
        maximum_entries: usize,
        visit: impl FnMut(OsString) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E>
    where
        E: From<SourceIoError>,
    {
        self.revalidate_same_object().map_err(E::from)?;
        let stamp = || -> Result<_> {
            let metadata = provider_source_io_result(
                self.display_path(),
                "provider source retained-directory metadata query",
                self.directory.metadata(),
            )?;
            provider_source_io_result(
                self.display_path(),
                "provider source retained-directory identity query",
                platform::object_stamp(&self.directory, &metadata),
            )
        };
        let before = stamp().map_err(E::from)?;
        self.visit_entries(maximum_entries, visit)?;
        if before != stamp().map_err(E::from)? {
            return Err(E::from(changed_path(self.display_path())));
        }
        self.revalidate_same_object().map_err(E::from)
    }

    pub fn entries_snapshot(&self, maximum_entries: usize) -> Result<Vec<OsString>> {
        let mut entries = Vec::new();
        self.visit_entries_snapshot(maximum_entries, |name| {
            entries.push(name);
            Ok::<_, SourceIoError>(())
        })?;
        entries.sort();
        Ok(entries)
    }

    /// Keeps both the retained directory and its named route bound to the
    /// admitted object without treating child additions as replacement.
    pub fn revalidate_same_object(&self) -> Result<()> {
        self.root.revalidate_same_object()?;
        let metadata = provider_source_io_result(
            self.display_path(),
            "provider source retained-directory metadata query",
            self.directory.metadata(),
        )?;
        let current = provider_source_io_result(
            self.display_path(),
            "provider source retained-directory identity query",
            platform::object_stamp(&self.directory, &metadata),
        )?;
        let reopened = self.root.open_directory(&self.relative_path)?;
        if !platform::same_object(&current, &self.opened)
            || !platform::same_object(&reopened.opened, &self.opened)
        {
            return Err(changed_path(self.display_path()));
        }
        Ok(())
    }

    pub fn authority_root(&self) -> ProviderSourceRoot {
        self.root.clone()
    }

    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    /// Fixed-width observation hint for this exact retained directory.
    pub fn authority_fingerprint(&self) -> [u8; 32] {
        platform::object_fingerprint(&self.opened)
    }

    /// Duplicates this exact retained directory capability without consulting
    /// its pathname. Consumers such as the SQLite source VFS use the duplicate
    /// only to open admitted leaf names relative to the already-authorized
    /// directory.
    pub fn try_clone_authority_handle(&self) -> io::Result<File> {
        self.directory.try_clone()
    }

    /// Streams at most `maximum_entries` child names from the retained
    /// directory handle in its native enumeration order.
    ///
    /// Unlike [`Self::entries`], this does not retain or sort the directory's
    /// complete fanout. Consumers that need deterministic order can build
    /// bounded sorted runs in the callback without reopening the directory.
    pub fn visit_entries<E>(
        &self,
        maximum_entries: usize,
        mut visit: impl FnMut(OsString) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E>
    where
        E: From<SourceIoError>,
    {
        let mut observed = 0_usize;
        let result = platform::visit_directory_entries(&self.directory, &mut |name| {
            if observed >= maximum_entries {
                return Err(E::from(invalid_path(
                    self.display_path(),
                    "provider source directory exceeds its bounded entry budget",
                )));
            }
            observed = observed.saturating_add(1);
            visit(name)
        });
        match result {
            Ok(()) => Ok(()),
            Err(DirectoryEntryVisitError::Authority(error)) => {
                Err(E::from(map_open_error(self.display_path(), error)))
            }
            Err(DirectoryEntryVisitError::Visitor(error)) => Err(error),
        }
    }

    /// Returns at most `maximum_entries` sorted child names from the retained
    /// directory handle.
    pub fn entries(&self, maximum_entries: usize) -> Result<Vec<OsString>> {
        platform::directory_entries(&self.directory, maximum_entries)
            .map_err(|error| map_open_error(self.display_path(), error))
    }

    /// Opens one child relative to this exact directory handle.
    pub fn open_child(&self, name: &OsStr) -> Result<OpenedProviderSourcePath> {
        validate_child_name(name, self.display_path())?;
        let relative_path = self.relative_path.join(name);
        let opened = platform::open_child(&self.directory, name, &self.root.inner.filesystem)
            .map_err(|error| map_open_error(&self.root.named_path().join(&relative_path), error))?;
        self.root.bind_relative_path(relative_path, opened)
    }

    /// Detects mutation of the directory while its children were enumerated
    /// and opened.
    pub fn revalidate(&self) -> Result<()> {
        let metadata = provider_source_io_result(
            self.display_path(),
            "provider source retained-directory metadata query",
            self.directory.metadata(),
        )?;
        let current = provider_source_io_result(
            self.display_path(),
            "provider source retained-directory identity query",
            platform::object_stamp(&self.directory, &metadata),
        )?;
        if current != self.opened {
            return Err(changed_path(self.display_path()));
        }
        Ok(())
    }

    fn display_path(&self) -> &Path {
        if self.relative_path.as_os_str().is_empty() {
            self.root.named_path()
        } else {
            &self.relative_path
        }
    }
}
