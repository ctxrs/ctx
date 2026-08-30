//! Capability-bound access to ordinary provider files and trees.
//!
//! A [`ProviderSourceRoot`] retains the exact directory opened as source
//! authority. Every descendant is opened one component at a time relative to
//! that handle. Callers may retain the root and deterministically reopen a
//! relative path without returning to an ancestor pathname.
//!
//! Migration pattern for ordinary provider callsites:
//!
//! 1. open the discovered tree once with [`ProviderSourceRoot::open`];
//! 2. enumerate with [`ProviderSourceDirectory::visit_entries`] or
//!    [`ProviderSourceDirectory::entries`], then use
//!    [`ProviderSourceDirectory::open_child`];
//! 3. parse through [`OpenedProviderSourceFile::bounded_reader`] or one of the
//!    bounded read helpers;
//! 4. call [`OpenedProviderSourceFile::revalidate`] after streaming parse, and
//!    [`ProviderSourceRoot::revalidate`] before publishing the inventory.
//!
//! The handles intentionally remain live for the lifetime of these values.
//! No provider body is copied merely to establish authority.

use std::{
    ffi::{OsStr, OsString},
    fs::{File, Metadata},
    io::{self, Read, Take},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use sha2::{Digest, Sha256};

use crate::ordinary_file::ORDINARY_FILE_V2_TOKEN_DOMAIN;
use crate::{Result, SourceIoError};

#[path = "root_handle/diagnostics.rs"]
mod diagnostics;
use diagnostics::{
    changed_path, ensure_absolute_traversal_free, invalid_path, map_changed_open_error,
    map_open_error, provider_source_io_result, validate_child_name, validate_relative_path,
};

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
#[path = "root_handle/unix.rs"]
mod platform;
#[cfg(target_os = "windows")]
#[path = "root_handle/windows.rs"]
mod platform;
#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "windows"
)))]
#[path = "root_handle/unsupported.rs"]
mod platform;

#[derive(Debug)]
pub(super) enum AuthorityOpenError {
    Io(io::Error),
    #[cfg_attr(
        not(any(target_os = "windows", test, feature = "test-support")),
        allow(
            dead_code,
            reason = "Windows source authority reports exact system operations"
        )
    )]
    SystemIo {
        operation: &'static str,
        source: io::Error,
    },
    Rejected(&'static str),
}

impl From<io::Error> for AuthorityOpenError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub(super) enum DirectoryEntryVisitError<E> {
    Authority(AuthorityOpenError),
    Visitor(E),
}

#[derive(Debug)]
struct ProviderSourceRootInner {
    named_path: PathBuf,
    directory: File,
    opened: platform::ObjectStamp,
    filesystem: platform::FilesystemIdentity,
}

/// Retained authority for one provider-owned directory tree.
///
/// Clones share the same opened directory handle. Dropping the final clone
/// releases the authority.
#[derive(Debug, Clone)]
pub struct ProviderSourceRoot {
    inner: Arc<ProviderSourceRootInner>,
}

/// One opened directory below a retained provider source root.
#[derive(Debug)]
pub struct ProviderSourceDirectory {
    root: ProviderSourceRoot,
    relative_path: PathBuf,
    directory: File,
    opened: platform::ObjectStamp,
}

/// One opened ordinary object beneath a provider source authority.
#[derive(Debug)]
pub enum OpenedProviderSourcePath {
    File(OpenedProviderSourceFile),
    Directory(ProviderSourceDirectory),
}

impl OpenedProviderSourcePath {
    /// Fixed-width identity for comparing two no-follow opens of one named
    /// selector entry without retaining every child capability concurrently.
    pub fn authority_fingerprint(&self) -> [u8; 32] {
        match self {
            Self::File(file) => platform::object_fingerprint(&file.opened),
            Self::Directory(directory) => directory.authority_fingerprint(),
        }
    }
}

/// An ordinary provider file bound to the handle that was actually opened.
///
/// The route is retained only for final same-object revalidation. Reads always
/// use `file`, never the route pathname.
#[derive(Debug)]
pub struct OpenedProviderSourceFile {
    route: ProviderSourceFileRoute,
    file: File,
    metadata: Metadata,
    opened: platform::ObjectStamp,
}

#[derive(Debug)]
enum ProviderSourceFileRoute {
    Absolute(PathBuf),
    Relative {
        root: ProviderSourceRoot,
        relative_path: PathBuf,
    },
}

#[allow(
    dead_code,
    reason = "provider adapters migrate to this shared authority API in follow-up slices"
)]
impl ProviderSourceRoot {
    /// Opens and retains an absolute, local, ordinary directory root.
    pub fn open(path: &Path) -> Result<Self> {
        match open_provider_source_path(path)? {
            OpenedProviderSourcePath::Directory(directory) => Ok(directory.root),
            OpenedProviderSourcePath::File(_) => Err(invalid_path(
                path,
                "provider source authority roots must be directories",
            )),
        }
    }

    pub fn named_path(&self) -> &Path {
        &self.inner.named_path
    }

    /// Fixed-width observation hint for the exact directory handle retained at
    /// construction. Callers must still use [`Self::revalidate`] as their
    /// terminal authority fence.
    pub fn authority_fingerprint(&self) -> [u8; 32] {
        platform::object_fingerprint(&self.inner.opened)
    }

    /// Compares the immutable object identity of two retained directory
    /// authorities while ignoring child-driven timestamp changes.
    pub fn same_object_as(&self, other: &Self) -> bool {
        platform::same_object(&self.inner.opened, &other.inner.opened)
    }

    pub fn directory(&self) -> Result<ProviderSourceDirectory> {
        let directory = provider_source_io_result(
            self.named_path(),
            "provider source directory-handle clone",
            self.inner.directory.try_clone(),
        )?;
        Ok(ProviderSourceDirectory {
            root: self.clone(),
            relative_path: PathBuf::new(),
            directory,
            opened: self.inner.opened.clone(),
        })
    }

    pub fn open_path(&self, relative_path: &Path) -> Result<OpenedProviderSourcePath> {
        validate_relative_path(relative_path)?;
        let mut directory = self.directory()?;
        let mut components = relative_path.components().peekable();
        while let Some(component) = components.next() {
            let Component::Normal(name) = component else {
                return Err(invalid_path(
                    relative_path,
                    "provider source descendants must use normal relative components",
                ));
            };
            let child = directory.open_child(name)?;
            if components.peek().is_none() {
                return Ok(child);
            }
            let OpenedProviderSourcePath::Directory(child_directory) = child else {
                return Err(invalid_path(
                    relative_path,
                    "provider source ancestor components must be directories",
                ));
            };
            directory = child_directory;
        }
        Ok(OpenedProviderSourcePath::Directory(directory))
    }

    pub fn open_file(&self, relative_path: &Path) -> Result<OpenedProviderSourceFile> {
        match self.open_path(relative_path)? {
            OpenedProviderSourcePath::File(file) => Ok(file),
            OpenedProviderSourcePath::Directory(_) => Err(invalid_path(
                relative_path,
                "provider transcript paths must be regular files",
            )),
        }
    }

    pub fn open_directory(&self, relative_path: &Path) -> Result<ProviderSourceDirectory> {
        match self.open_path(relative_path)? {
            OpenedProviderSourcePath::Directory(directory) => Ok(directory),
            OpenedProviderSourcePath::File(_) => Err(invalid_path(
                relative_path,
                "provider source tree components must be directories",
            )),
        }
    }

    /// Confirms both the retained directory and its current named route still
    /// identify the exact root admitted at construction.
    pub fn revalidate(&self) -> Result<()> {
        let current_metadata = provider_source_io_result(
            &self.inner.named_path,
            "provider source retained-directory metadata query",
            self.inner.directory.metadata(),
        )?;
        let current = provider_source_io_result(
            &self.inner.named_path,
            "provider source retained-directory identity query",
            platform::object_stamp(&self.inner.directory, &current_metadata),
        )?;
        if current != self.inner.opened {
            return Err(changed_path(&self.inner.named_path));
        }
        let reopened = platform::open_absolute(&self.inner.named_path)
            .map_err(|error| map_changed_open_error(&self.inner.named_path, error))?;
        let platform::OpenedPath::Directory { file, metadata, .. } = reopened else {
            return Err(changed_path(&self.inner.named_path));
        };
        let named = provider_source_io_result(
            &self.inner.named_path,
            "provider source reopened-directory identity query",
            platform::object_stamp(&file, &metadata),
        )?;
        if named != self.inner.opened {
            return Err(changed_path(&self.inner.named_path));
        }
        Ok(())
    }

    /// Confirms that both the retained directory handle and its named route
    /// still identify the same root while allowing metadata changes caused by
    /// children being added, removed, or updated. Inventory owners use
    /// [`Self::revalidate`] separately when they require an exact tree fence.
    pub fn revalidate_same_object(&self) -> Result<()> {
        let current_metadata = provider_source_io_result(
            &self.inner.named_path,
            "provider source retained-directory metadata query",
            self.inner.directory.metadata(),
        )?;
        let current = provider_source_io_result(
            &self.inner.named_path,
            "provider source retained-directory identity query",
            platform::object_stamp(&self.inner.directory, &current_metadata),
        )?;
        if !platform::same_object(&current, &self.inner.opened) {
            return Err(changed_path(&self.inner.named_path));
        }
        let reopened = platform::open_absolute(&self.inner.named_path)
            .map_err(|error| map_changed_open_error(&self.inner.named_path, error))?;
        let platform::OpenedPath::Directory { file, metadata, .. } = reopened else {
            return Err(changed_path(&self.inner.named_path));
        };
        let named = provider_source_io_result(
            &self.inner.named_path,
            "provider source reopened-directory identity query",
            platform::object_stamp(&file, &metadata),
        )?;
        if !platform::same_object(&named, &self.inner.opened) {
            return Err(changed_path(&self.inner.named_path));
        }
        Ok(())
    }
}

#[allow(
    dead_code,
    reason = "provider adapters migrate to this shared authority API in follow-up slices"
)]
impl ProviderSourceDirectory {
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
        let named_path = self.root.named_path().join(&relative_path);
        let opened = platform::open_child(&self.directory, name, &self.root.inner.filesystem)
            .map_err(|error| map_open_error(&named_path, error))?;
        match opened {
            platform::OpenedPath::File {
                file,
                metadata,
                filesystem: _,
            } => {
                let stamp = provider_source_io_result(
                    &named_path,
                    "provider source opened-file identity query",
                    platform::object_stamp(&file, &metadata),
                )?;
                Ok(OpenedProviderSourcePath::File(OpenedProviderSourceFile {
                    route: ProviderSourceFileRoute::Relative {
                        root: self.root.clone(),
                        relative_path,
                    },
                    file,
                    metadata,
                    opened: stamp,
                }))
            }
            platform::OpenedPath::Directory {
                file,
                metadata,
                filesystem: _,
            } => {
                let stamp = provider_source_io_result(
                    &named_path,
                    "provider source opened-directory identity query",
                    platform::object_stamp(&file, &metadata),
                )?;
                Ok(OpenedProviderSourcePath::Directory(
                    ProviderSourceDirectory {
                        root: self.root.clone(),
                        relative_path,
                        directory: file,
                        opened: stamp,
                    },
                ))
            }
        }
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

#[allow(
    dead_code,
    reason = "provider adapters migrate to this shared authority API in follow-up slices"
)]
impl OpenedProviderSourceFile {
    pub fn len(&self) -> u64 {
        self.metadata.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn modified(&self) -> io::Result<SystemTime> {
        self.metadata.modified()
    }

    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// Fixed-width observation hint for the exact file handle opened by the
    /// authority walk. This is not a substitute for [`Self::revalidate`].
    pub fn authority_fingerprint(&self) -> [u8; 32] {
        platform::object_fingerprint(&self.opened)
    }

    /// Stable ordinary-file token derived from the retained object stamp.
    ///
    /// This performs no second filesystem observation; [`Self::revalidate_leaf`]
    /// is the proof that the stamp still describes the opened object and route.
    pub fn ordinary_file_token(&self) -> [u8; 32] {
        ordinary_file_token(&self.opened)
    }

    /// Strong token for the retained file's current metadata observation.
    pub fn current_ordinary_file_token(&self) -> Result<[u8; 32]> {
        let metadata = provider_source_io_result(
            self.display_path(),
            "provider source retained-file metadata query",
            self.file.metadata(),
        )?;
        let current = provider_source_io_result(
            self.display_path(),
            "provider source retained-file identity query",
            platform::object_stamp(&self.file, &metadata),
        )?;
        Ok(ordinary_file_token(&current))
    }

    pub fn file(&self) -> &File {
        &self.file
    }

    /// Reopens this file through its retained path authority and verifies that
    /// the new handle names the same ordinary object admitted originally.
    ///
    /// Unlike [`File::try_clone`], the returned handle has an independent file
    /// cursor. Callers that seek or stream concurrently must use this operation
    /// so one reader cannot move another reader's position.
    pub fn reopen_same_object(&self) -> Result<File> {
        match &self.route {
            ProviderSourceFileRoute::Absolute(path) => {
                let reopened = platform::open_absolute(path)
                    .map_err(|error| map_changed_open_error(path, error))?;
                let platform::OpenedPath::File { file, metadata, .. } = reopened else {
                    return Err(changed_path(path));
                };
                let opened = provider_source_io_result(
                    path,
                    "provider source reopened-file identity query",
                    platform::object_stamp(&file, &metadata),
                )?;
                if !platform::same_object(&opened, &self.opened) {
                    return Err(changed_path(path));
                }
                Ok(file)
            }
            ProviderSourceFileRoute::Relative {
                root,
                relative_path,
            } => match root.open_path(relative_path)? {
                OpenedProviderSourcePath::File(reopened)
                    if platform::same_object(&reopened.opened, &self.opened) =>
                {
                    Ok(reopened.file)
                }
                _ => Err(changed_path(self.display_path())),
            },
        }
    }

    pub fn bounded_reader(&self, maximum_bytes: u64) -> Result<Take<File>> {
        if self.len() > maximum_bytes {
            return Err(SourceIoError::InvalidPayload(format!(
                "provider source file exceeds {maximum_bytes} bytes"
            )));
        }
        Ok(provider_source_io_result(
            self.display_path(),
            "provider source file-handle clone",
            self.file.try_clone(),
        )?
        .take(self.len()))
    }

    pub fn read_all_bounded(&self, maximum_bytes: usize) -> Result<Vec<u8>> {
        let maximum_bytes_u64 = u64::try_from(maximum_bytes)
            .map_err(|_| SourceIoError::SystemInvariant("bounded read size exceeds u64"))?;
        let mut reader = self.bounded_reader(maximum_bytes_u64)?;
        let capacity = usize::try_from(self.len()).map_err(|_| {
            SourceIoError::InvalidPayload("provider source file is too large".into())
        })?;
        let mut bytes = Vec::with_capacity(capacity);
        provider_source_io_result(
            self.display_path(),
            "provider source bounded file read",
            reader.read_to_end(&mut bytes),
        )?;
        if bytes.len() != capacity {
            return Err(changed_path(self.display_path()));
        }
        self.revalidate()?;
        Ok(bytes)
    }

    pub fn read_exact_range(
        &self,
        offset: u64,
        length: usize,
        maximum_bytes: usize,
    ) -> Result<Vec<u8>> {
        if length > maximum_bytes {
            return Err(SourceIoError::InvalidPayload(format!(
                "provider source range exceeds {maximum_bytes} bytes"
            )));
        }
        let length_u64 = u64::try_from(length)
            .map_err(|_| SourceIoError::SystemInvariant("range length exceeds u64"))?;
        let end = offset.checked_add(length_u64).ok_or_else(|| {
            SourceIoError::InvalidPayload("provider source range overflows".into())
        })?;
        if end > self.len() {
            return Err(SourceIoError::InvalidPayload(
                "provider source range exceeds the opened file".into(),
            ));
        }
        let mut bytes = vec![0_u8; length];
        provider_source_io_result(
            self.display_path(),
            "provider source exact range read",
            platform::read_exact_at(&self.file, &mut bytes, offset),
        )?;
        self.revalidate()?;
        Ok(bytes)
    }

    /// Reads an exact range from an append-friendly source and permits only a
    /// same-object metadata change while the range is read. Callers must bind
    /// the returned bytes to their own digest and frozen-prefix evidence.
    pub fn read_exact_range_allow_append(
        &self,
        offset: u64,
        length: usize,
        maximum_bytes: usize,
    ) -> Result<Vec<u8>> {
        if length > maximum_bytes {
            return Err(SourceIoError::InvalidPayload(format!(
                "provider source range exceeds {maximum_bytes} bytes"
            )));
        }
        let length_u64 = u64::try_from(length)
            .map_err(|_| SourceIoError::SystemInvariant("range length exceeds u64"))?;
        let end = offset.checked_add(length_u64).ok_or_else(|| {
            SourceIoError::InvalidPayload("provider source range overflows".into())
        })?;
        let current_len = provider_source_io_result(
            self.display_path(),
            "provider source retained-file metadata query",
            self.file.metadata(),
        )?
        .len();
        if end > current_len {
            return Err(SourceIoError::InvalidPayload(
                "provider source range exceeds the opened file".into(),
            ));
        }
        let mut bytes = vec![0_u8; length];
        provider_source_io_result(
            self.display_path(),
            "provider source exact range read",
            platform::read_exact_at(&self.file, &mut bytes, offset),
        )?;
        self.revalidate_same_object()?;
        Ok(bytes)
    }

    /// Confirms the open handle did not change while read and its route beneath
    /// the retained authority still names the same object.
    ///
    /// Relative callers must perform one terminal [`ProviderSourceRoot::revalidate`]
    /// after all leaf checks before publishing aggregate evidence.
    pub fn revalidate_leaf(&self) -> Result<()> {
        let current_metadata = provider_source_io_result(
            self.display_path(),
            "provider source retained-file metadata query",
            self.file.metadata(),
        )?;
        let current = provider_source_io_result(
            self.display_path(),
            "provider source retained-file identity query",
            platform::object_stamp(&self.file, &current_metadata),
        )?;
        if current != self.opened {
            return Err(changed_path(self.display_path()));
        }
        let reopened = match &self.route {
            ProviderSourceFileRoute::Absolute(path) => platform::open_absolute(path)
                .map_err(|error| map_changed_open_error(path, error))?,
            ProviderSourceFileRoute::Relative {
                root,
                relative_path,
            } => {
                let reopened = root.open_path(relative_path)?;
                return match reopened {
                    OpenedProviderSourcePath::File(reopened) if reopened.opened == self.opened => {
                        Ok(())
                    }
                    _ => Err(changed_path(self.display_path())),
                };
            }
        };
        let platform::OpenedPath::File { file, metadata, .. } = reopened else {
            return Err(changed_path(self.display_path()));
        };
        let named = provider_source_io_result(
            self.display_path(),
            "provider source reopened-file identity query",
            platform::object_stamp(&file, &metadata),
        )?;
        if named != self.opened {
            return Err(changed_path(self.display_path()));
        }
        Ok(())
    }

    /// Confirms the route still names the same ordinary file while allowing
    /// append-only metadata changes on that object.
    pub fn revalidate_same_object_leaf(&self) -> Result<()> {
        let current_metadata = provider_source_io_result(
            self.display_path(),
            "provider source retained-file metadata query",
            self.file.metadata(),
        )?;
        let current = provider_source_io_result(
            self.display_path(),
            "provider source retained-file identity query",
            platform::object_stamp(&self.file, &current_metadata),
        )?;
        if !platform::same_object(&current, &self.opened) {
            return Err(changed_path(self.display_path()));
        }
        let reopened = match &self.route {
            ProviderSourceFileRoute::Absolute(path) => platform::open_absolute(path)
                .map_err(|error| map_changed_open_error(path, error))?,
            ProviderSourceFileRoute::Relative {
                root,
                relative_path,
            } => {
                let reopened = root.open_path(relative_path)?;
                return match reopened {
                    OpenedProviderSourcePath::File(reopened)
                        if platform::same_object(&reopened.opened, &self.opened) =>
                    {
                        Ok(())
                    }
                    _ => Err(changed_path(self.display_path())),
                };
            }
        };
        let platform::OpenedPath::File { file, metadata, .. } = reopened else {
            return Err(changed_path(self.display_path()));
        };
        let named = provider_source_io_result(
            self.display_path(),
            "provider source reopened-file identity query",
            platform::object_stamp(&file, &metadata),
        )?;
        if !platform::same_object(&named, &self.opened) {
            return Err(changed_path(self.display_path()));
        }
        Ok(())
    }

    /// Confirms same-object leaf identity and the retained root route. This is
    /// used only by append-friendly providers that separately freeze and hash
    /// the admitted byte prefix.
    pub fn revalidate_same_object(&self) -> Result<()> {
        self.revalidate_same_object_leaf()?;
        if let ProviderSourceFileRoute::Relative { root, .. } = &self.route {
            root.revalidate_same_object()?;
        }
        Ok(())
    }

    /// Confirms the leaf proof and, for a relative route, the current named root.
    pub fn revalidate(&self) -> Result<()> {
        self.revalidate_leaf()?;
        if let ProviderSourceFileRoute::Relative { root, .. } = &self.route {
            root.revalidate()?;
        }
        Ok(())
    }

    fn display_path(&self) -> &Path {
        match &self.route {
            ProviderSourceFileRoute::Absolute(path) => path,
            ProviderSourceFileRoute::Relative { relative_path, .. } => relative_path,
        }
    }
}

fn ordinary_file_token(stamp: &platform::ObjectStamp) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(ORDINARY_FILE_V2_TOKEN_DOMAIN);
    digest.update(b"platform\0");
    digest.update(platform::object_change_token(stamp));
    digest.finalize().into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RetainedFileIdentityVersion {
    SharedJsonlV1,
    OrdinaryFileV2,
}

#[cfg(unix)]
fn retained_file_identity(
    _path: &Path,
    _file: &File,
    metadata: &Metadata,
    version: RetainedFileIdentityVersion,
) -> Result<Option<([u8; 32], [u8; 32])>> {
    use std::os::unix::fs::MetadataExt;

    let mut stable = Sha256::new();
    let mut change = Sha256::new();
    match version {
        RetainedFileIdentityVersion::SharedJsonlV1 => {
            stable.update(b"ctx-jsonl-retained-file-identity-v1\0unix-stable\0");
            change.update(b"ctx-jsonl-retained-file-identity-v1\0unix-change\0");
        }
        RetainedFileIdentityVersion::OrdinaryFileV2 => {
            stable.update(b"ctx-ordinary-file-observation-v2\0unix-stable\0");
            change.update(b"ctx-ordinary-file-observation-v2\0unix-change\0");
        }
    }
    stable.update(metadata.dev().to_le_bytes());
    stable.update(metadata.ino().to_le_bytes());
    if version == RetainedFileIdentityVersion::OrdinaryFileV2 {
        stable.update(metadata.mode().to_le_bytes());
        change.update(metadata.dev().to_le_bytes());
        change.update(metadata.ino().to_le_bytes());
    }
    change.update(metadata.ctime().to_le_bytes());
    change.update(metadata.ctime_nsec().to_le_bytes());
    Ok(Some((stable.finalize().into(), change.finalize().into())))
}

#[cfg(not(unix))]
fn retained_file_identity(
    path: &Path,
    file: &File,
    metadata: &Metadata,
    version: RetainedFileIdentityVersion,
) -> Result<Option<([u8; 32], [u8; 32])>> {
    platform::retained_file_identity(file, metadata, version)
        .map_err(|error| map_open_error(path, error))
}

pub(crate) fn retained_jsonl_file_v1_identity(
    path: &Path,
    file: &File,
    metadata: &Metadata,
) -> Result<Option<([u8; 32], [u8; 32])>> {
    retained_file_identity(
        path,
        file,
        metadata,
        RetainedFileIdentityVersion::SharedJsonlV1,
    )
}

pub(crate) fn retained_ordinary_file_v2_identity(
    path: &Path,
    file: &File,
    metadata: &Metadata,
) -> Result<Option<([u8; 32], [u8; 32])>> {
    retained_file_identity(
        path,
        file,
        metadata,
        RetainedFileIdentityVersion::OrdinaryFileV2,
    )
}

/// Opens an ordinary provider file with a no-follow component walk and retains
/// the exact opened handle for reads and final revalidation.
pub fn open_provider_source_file(path: &Path) -> Result<OpenedProviderSourceFile> {
    match open_provider_source_path(path)? {
        OpenedProviderSourcePath::File(file) => Ok(file),
        OpenedProviderSourcePath::Directory(_) => Err(invalid_path(
            path,
            "provider transcript paths must be regular files",
        )),
    }
}

pub fn open_provider_source_path(path: &Path) -> Result<OpenedProviderSourcePath> {
    let path = platform::normalize_authority_path(path);
    ensure_absolute_traversal_free(&path)?;
    let opened = platform::open_absolute(&path).map_err(|error| map_open_error(&path, error))?;
    match opened {
        platform::OpenedPath::File {
            file,
            metadata,
            filesystem: _,
        } => {
            let stamp = provider_source_io_result(
                &path,
                "provider source opened-file identity query",
                platform::object_stamp(&file, &metadata),
            )?;
            Ok(OpenedProviderSourcePath::File(OpenedProviderSourceFile {
                route: ProviderSourceFileRoute::Absolute(path),
                file,
                metadata,
                opened: stamp,
            }))
        }
        platform::OpenedPath::Directory {
            file,
            metadata,
            filesystem,
        } => {
            let stamp = provider_source_io_result(
                &path,
                "provider source opened-directory identity query",
                platform::object_stamp(&file, &metadata),
            )?;
            let root = ProviderSourceRoot {
                inner: Arc::new(ProviderSourceRootInner {
                    named_path: path,
                    directory: file,
                    opened: stamp,
                    filesystem,
                }),
            };
            Ok(OpenedProviderSourcePath::Directory(root.directory()?))
        }
    }
}

/// Reason recorded when a provider source path component is neither a regular
/// file nor a directory (for example a Unix-domain socket, FIFO, or device
/// node). Traversal callers can skip such entries safely without treating the
/// enclosing provider source as unreadable.
pub const NON_REGULAR_PROVIDER_SOURCE_REASON: &str =
    "provider source paths must be regular files or directories";

/// True when `error` is the safe rejection of a non-regular special-file entry
/// (see [`NON_REGULAR_PROVIDER_SOURCE_REASON`]), as opposed to a symlink
/// rejection or a genuine IO failure that must fail the enclosing traversal.
pub fn is_non_regular_source_rejection(error: &SourceIoError) -> bool {
    matches!(
        error,
        SourceIoError::InvalidProviderTranscriptPath { reason, .. }
            if *reason == NON_REGULAR_PROVIDER_SOURCE_REASON
    )
}

/// Reason recorded when a provider source path component is a symlink (Unix)
/// or a reparse, offline, or cloud-placeholder entry (Windows). Provider
/// layouts that store non-transcript working files beside transcripts (for
/// example Copilot CLI `session-state/<id>/files/` checkouts containing
/// `CLAUDE.md -> AGENTS.md` links) can skip such entries safely: the link is
/// never followed, so the no-follow security boundary is preserved.
pub const SYMLINK_PROVIDER_SOURCE_REASON: &str =
    "symlinked provider source path components are rejected";

/// Windows counterpart of [`SYMLINK_PROVIDER_SOURCE_REASON`].
pub const REPARSE_PROVIDER_SOURCE_REASON: &str =
    "reparse, offline, and cloud-placeholder provider sources are rejected";

/// True when `error` is the safe rejection of a link-like entry that a
/// traversal can skip without following it. The entry itself is never opened,
/// so skipping it does not weaken the symlink boundary; transcript-shaped
/// selections must still treat this rejection as fatal.
pub fn is_symlink_source_rejection(error: &SourceIoError) -> bool {
    matches!(
        error,
        SourceIoError::InvalidProviderTranscriptPath { reason, .. }
            if *reason == SYMLINK_PROVIDER_SOURCE_REASON
                || *reason == REPARSE_PROVIDER_SOURCE_REASON
    )
}

#[cfg(any(test, feature = "test-support"))]
mod tests;
