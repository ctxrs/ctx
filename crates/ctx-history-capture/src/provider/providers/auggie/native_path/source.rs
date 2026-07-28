use super::*;

#[derive(Debug, Clone)]
pub(super) struct AuggieFileStamp {
    pub(super) canonical_path: PathBuf,
    pub(super) len: u64,
    pub(super) modified: SystemTime,
    pub(super) readonly: bool,
    pub(super) device: Option<u64>,
    pub(super) inode: Option<u64>,
    opened: Arc<OpenedProviderSourceFile>,
}

impl PartialEq for AuggieFileStamp {
    fn eq(&self, other: &Self) -> bool {
        self.canonical_path == other.canonical_path
            && self.len == other.len
            && self.modified == other.modified
            && self.readonly == other.readonly
            && self.device == other.device
            && self.inode == other.inode
    }
}

impl Eq for AuggieFileStamp {}

impl AuggieFileStamp {
    pub(super) fn from_opened(path: PathBuf, opened: OpenedProviderSourceFile) -> Result<Self> {
        let metadata = opened.metadata();
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
        #[cfg(not(unix))]
        let (device, inode) = (None, None);

        Ok(Self {
            canonical_path: path,
            len: metadata.len(),
            modified: metadata.modified()?,
            readonly: metadata.permissions().readonly(),
            device,
            inode,
            opened: Arc::new(opened),
        })
    }

    pub(super) fn observe(path: &Path) -> Result<Self> {
        let path = normalized_auggie_authority_path(path)?;
        let opened = match open_provider_source_path(&path)? {
            OpenedProviderSourcePath::File(opened) => opened,
            OpenedProviderSourcePath::Directory(_) => {
                return Err(invalid_source_path(
                    &path,
                    "Auggie transcript paths must be regular files",
                ));
            }
        };
        Self::from_opened(path, opened)
    }

    pub(super) fn revalidate(&self) -> Result<bool> {
        match self.opened.revalidate() {
            Ok(()) => Ok(true),
            Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(CaptureError::InvalidProviderTranscriptPath { .. })
            | Err(CaptureError::SourceChangedDuringCapture) => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub(super) fn read_all_bounded(&self, maximum: usize) -> Result<Vec<u8>> {
        self.opened.read_all_bounded(maximum)
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
    pub(super) authority: Option<ProviderSourceRoot>,
}

impl AuggieInventory {
    pub(super) fn open_source(&self, path: &Path) -> Result<AuggieFileStamp> {
        let authority = self
            .authority
            .as_ref()
            .ok_or(CaptureError::SystemInvariant(
                "Auggie inventory has no retained source authority",
            ))?;
        let relative = path.strip_prefix(authority.named_path()).map_err(|_| {
            invalid_source_path(path, "Auggie source escaped its retained authority root")
        })?;
        AuggieFileStamp::from_opened(path.to_path_buf(), authority.open_file(relative)?)
    }

    pub(super) fn revalidate(&self) -> Result<()> {
        match self.authority.as_ref() {
            Some(root) => root.revalidate(),
            None if self.root_missing => Ok(()),
            None => Err(CaptureError::SystemInvariant(
                "Auggie complete inventory has no retained authority",
            )),
        }
    }
}

pub(super) fn discover_auggie_sources(root: &Path) -> Result<AuggieInventory> {
    let root = normalized_auggie_authority_path(root)?;
    let opened_root = match open_provider_source_path(&root) {
        Ok(opened) => opened,
        Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(AuggieInventory {
                paths: BTreeSet::new(),
                root_missing: true,
                authority: None,
            });
        }
        Err(error) => return Err(error),
    };
    if let OpenedProviderSourcePath::File(opened) = opened_root {
        let file_name = root.file_name().ok_or_else(|| {
            invalid_source_path(&root, "Auggie transcript file has no final path component")
        })?;
        let parent = root.parent().ok_or_else(|| {
            invalid_source_path(&root, "Auggie transcript file has no authority parent")
        })?;
        let authority = ProviderSourceRoot::open(parent)?;
        let path = authority.named_path().join(file_name);
        let retained = authority.open_file(Path::new(file_name))?;
        let mut paths = BTreeSet::new();
        if root.extension().and_then(|extension| extension.to_str()) == Some("json") {
            paths.insert(path.clone());
        }
        opened.revalidate()?;
        retained.revalidate()?;
        return Ok(AuggieInventory {
            paths,
            root_missing: false,
            authority: Some(authority),
        });
    }
    let OpenedProviderSourcePath::Directory(root_directory) = opened_root else {
        return Err(CaptureError::SystemInvariant(
            "Auggie source root classification is incomplete",
        ));
    };
    let authority = root_directory.authority_root();

    let mut paths = BTreeSet::new();
    let mut stack = vec![(PathBuf::new(), 0_usize)];
    let mut directories = 0_usize;
    while let Some((relative_directory, depth)) = stack.pop() {
        directories = directories.saturating_add(1);
        if directories > AUGGIE_MAX_DISCOVERED_DIRECTORIES {
            return Err(invalid_source_path(
                authority.named_path(),
                "Auggie transcript discovery exceeds the directory bound",
            ));
        }
        if depth > AUGGIE_MAX_DISCOVERY_DEPTH {
            return Err(invalid_source_path(
                authority.named_path(),
                "Auggie transcript discovery exceeds the depth bound",
            ));
        }
        let directory = authority.open_directory(&relative_directory)?;
        let names = directory.entries(
            AUGGIE_MAX_DISCOVERED_FILES.saturating_add(AUGGIE_MAX_DISCOVERED_DIRECTORIES),
        )?;
        for name in names.into_iter().rev() {
            let relative_path = relative_directory.join(&name);
            let entry_path = authority.named_path().join(&relative_path);
            match directory.open_child(&name)? {
                OpenedProviderSourcePath::Directory(_) => {
                    stack.push((relative_path, depth.saturating_add(1)));
                }
                OpenedProviderSourcePath::File(opened)
                    if entry_path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        == Some("json") =>
                {
                    opened.revalidate()?;
                    paths.insert(entry_path);
                    if paths.len() > AUGGIE_MAX_DISCOVERED_FILES {
                        return Err(invalid_source_path(
                            authority.named_path(),
                            "Auggie transcript discovery exceeds the file bound",
                        ));
                    }
                }
                OpenedProviderSourcePath::File(_) => {}
            }
        }
        directory.revalidate()?;
    }
    authority.revalidate()?;
    Ok(AuggieInventory {
        paths,
        root_missing: false,
        authority: Some(authority),
    })
}

pub(super) fn invalid_source_path(path: &Path, reason: &'static str) -> CaptureError {
    CaptureError::InvalidProviderTranscriptPath {
        path: path.to_path_buf(),
        reason,
    }
}
