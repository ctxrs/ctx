use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AuggieFileStamp {
    pub(super) canonical_path: PathBuf,
    pub(super) len: u64,
    pub(super) modified: SystemTime,
    pub(super) readonly: bool,
    pub(super) device: Option<u64>,
    pub(super) inode: Option<u64>,
}

impl AuggieFileStamp {
    pub(super) fn observe(path: &Path) -> Result<Self> {
        ensure_regular_provider_transcript_file(path)?;
        let metadata = fs::symlink_metadata(path)?;
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
        #[cfg(not(unix))]
        let (device, inode) = (None, None);

        Ok(Self {
            canonical_path: fs::canonicalize(path)?,
            len: metadata.len(),
            modified: metadata.modified()?,
            readonly: metadata.permissions().readonly(),
            device,
            inode,
        })
    }

    pub(super) fn revalidate(&self) -> Result<bool> {
        match Self::observe(&self.canonical_path) {
            Ok(current) => Ok(&current == self),
            Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub(super) fn revision_material(&self, digest: &mut Sha256) {
        digest.update(self.canonical_path.as_os_str().as_encoded_bytes());
        digest.update(self.len.to_be_bytes());
        let (sign, seconds, nanos) = match self.modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => (1_u8, duration.as_secs(), duration.subsec_nanos()),
            Err(error) => {
                let duration = error.duration();
                (0_u8, duration.as_secs(), duration.subsec_nanos())
            }
        };
        digest.update([sign]);
        digest.update(seconds.to_be_bytes());
        digest.update(nanos.to_be_bytes());
        digest.update([u8::from(self.readonly)]);
        digest.update(self.device.unwrap_or_default().to_be_bytes());
        digest.update(self.inode.unwrap_or_default().to_be_bytes());
    }
}

pub(super) struct AuggieInventory {
    pub(super) paths: BTreeSet<PathBuf>,
    pub(super) root_missing: bool,
}

pub(super) fn discover_auggie_sources(root: &Path) -> Result<AuggieInventory> {
    let root_metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(AuggieInventory {
                paths: BTreeSet::new(),
                root_missing: true,
            });
        }
        Err(error) => return Err(error.into()),
    };
    if root_metadata.file_type().is_symlink() {
        return Err(invalid_source_path(
            root,
            "symlinked provider transcript roots are rejected",
        ));
    }
    ensure_provider_path_parents_are_not_symlinks(root)?;
    if root_metadata.is_file() {
        ensure_regular_provider_transcript_file(root)?;
        let mut paths = BTreeSet::new();
        if root.extension().and_then(|extension| extension.to_str()) == Some("json") {
            paths.insert(fs::canonicalize(root)?);
        }
        return Ok(AuggieInventory {
            paths,
            root_missing: false,
        });
    }
    if !root_metadata.is_dir() {
        return Err(invalid_source_path(
            root,
            "Auggie transcript root is neither a file nor a directory",
        ));
    }

    let mut paths = BTreeSet::new();
    let mut stack = vec![(root.to_path_buf(), 0_usize)];
    let mut directories = 0_usize;
    while let Some((directory, depth)) = stack.pop() {
        directories = directories.saturating_add(1);
        if directories > AUGGIE_MAX_DISCOVERED_DIRECTORIES {
            return Err(invalid_source_path(
                root,
                "Auggie transcript discovery exceeds the directory bound",
            ));
        }
        if depth > AUGGIE_MAX_DISCOVERY_DEPTH {
            return Err(invalid_source_path(
                root,
                "Auggie transcript discovery exceeds the depth bound",
            ));
        }
        let mut entries = fs::read_dir(&directory)?.collect::<io::Result<Vec<_>>>()?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries.into_iter().rev() {
            let entry_path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                return Err(invalid_source_path(
                    &entry_path,
                    "symlinked Auggie transcript entries are rejected",
                ));
            }
            if file_type.is_dir() {
                stack.push((entry_path, depth.saturating_add(1)));
            } else if file_type.is_file()
                && entry_path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    == Some("json")
            {
                ensure_regular_provider_transcript_file(&entry_path)?;
                paths.insert(fs::canonicalize(entry_path)?);
                if paths.len() > AUGGIE_MAX_DISCOVERED_FILES {
                    return Err(invalid_source_path(
                        root,
                        "Auggie transcript discovery exceeds the file bound",
                    ));
                }
            }
        }
    }
    Ok(AuggieInventory {
        paths,
        root_missing: false,
    })
}

pub(super) fn invalid_source_path(path: &Path, reason: &'static str) -> CaptureError {
    CaptureError::InvalidProviderTranscriptPath {
        path: path.to_path_buf(),
        reason,
    }
}
