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
//! 2. enumerate with [`ProviderSourceDirectory::entries`] and
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

use crate::{CaptureError, Result};

const ORDINARY_FILE_TOKEN_DOMAIN: &[u8] = b"ctx-ordinary-file-observation-v2\0";

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
    Rejected(&'static str),
}

impl From<io::Error> for AuthorityOpenError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
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
pub(crate) struct ProviderSourceRoot {
    inner: Arc<ProviderSourceRootInner>,
}

/// One opened directory below a retained provider source root.
#[derive(Debug)]
pub(crate) struct ProviderSourceDirectory {
    root: ProviderSourceRoot,
    relative_path: PathBuf,
    directory: File,
    opened: platform::ObjectStamp,
}

/// One opened ordinary object beneath a provider source authority.
#[derive(Debug)]
pub(crate) enum OpenedProviderSourcePath {
    File(OpenedProviderSourceFile),
    Directory(ProviderSourceDirectory),
}

/// An ordinary provider file bound to the handle that was actually opened.
///
/// The route is retained only for final same-object revalidation. Reads always
/// use `file`, never the route pathname.
#[derive(Debug)]
pub(crate) struct OpenedProviderSourceFile {
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
    pub(crate) fn open(path: &Path) -> Result<Self> {
        match open_provider_source_path(path)? {
            OpenedProviderSourcePath::Directory(directory) => Ok(directory.root),
            OpenedProviderSourcePath::File(_) => Err(invalid_path(
                path,
                "provider source authority roots must be directories",
            )),
        }
    }

    pub(crate) fn named_path(&self) -> &Path {
        &self.inner.named_path
    }

    /// Fixed-width observation hint for the exact directory handle retained at
    /// construction. Callers must still use [`Self::revalidate`] as their
    /// terminal authority fence.
    pub(crate) fn authority_fingerprint(&self) -> [u8; 32] {
        platform::object_fingerprint(&self.inner.opened)
    }

    pub(crate) fn directory(&self) -> Result<ProviderSourceDirectory> {
        let directory = self.inner.directory.try_clone()?;
        Ok(ProviderSourceDirectory {
            root: self.clone(),
            relative_path: PathBuf::new(),
            directory,
            opened: self.inner.opened.clone(),
        })
    }

    pub(crate) fn open_path(&self, relative_path: &Path) -> Result<OpenedProviderSourcePath> {
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

    pub(crate) fn open_file(&self, relative_path: &Path) -> Result<OpenedProviderSourceFile> {
        match self.open_path(relative_path)? {
            OpenedProviderSourcePath::File(file) => Ok(file),
            OpenedProviderSourcePath::Directory(_) => Err(invalid_path(
                relative_path,
                "provider transcript paths must be regular files",
            )),
        }
    }

    pub(crate) fn open_directory(&self, relative_path: &Path) -> Result<ProviderSourceDirectory> {
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
    pub(crate) fn revalidate(&self) -> Result<()> {
        let current_metadata = self.inner.directory.metadata()?;
        let current = platform::object_stamp(&self.inner.directory, &current_metadata)?;
        if current != self.inner.opened {
            return Err(changed_path(&self.inner.named_path));
        }
        let reopened = platform::open_absolute(&self.inner.named_path)
            .map_err(|error| map_changed_open_error(&self.inner.named_path, error))?;
        let platform::OpenedPath::Directory { file, metadata, .. } = reopened else {
            return Err(changed_path(&self.inner.named_path));
        };
        let named = platform::object_stamp(&file, &metadata)?;
        if named != self.inner.opened {
            return Err(changed_path(&self.inner.named_path));
        }
        Ok(())
    }

    /// Confirms that both the retained directory handle and its named route
    /// still identify the same root while allowing metadata changes caused by
    /// children being added, removed, or updated. Inventory owners use
    /// [`Self::revalidate`] separately when they require an exact tree fence.
    pub(crate) fn revalidate_same_object(&self) -> Result<()> {
        let current_metadata = self.inner.directory.metadata()?;
        let current = platform::object_stamp(&self.inner.directory, &current_metadata)?;
        if !platform::same_object(&current, &self.inner.opened) {
            return Err(changed_path(&self.inner.named_path));
        }
        let reopened = platform::open_absolute(&self.inner.named_path)
            .map_err(|error| map_changed_open_error(&self.inner.named_path, error))?;
        let platform::OpenedPath::Directory { file, metadata, .. } = reopened else {
            return Err(changed_path(&self.inner.named_path));
        };
        let named = platform::object_stamp(&file, &metadata)?;
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
    pub(crate) fn authority_root(&self) -> ProviderSourceRoot {
        self.root.clone()
    }

    pub(crate) fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    /// Fixed-width observation hint for this exact retained directory.
    pub(crate) fn authority_fingerprint(&self) -> [u8; 32] {
        platform::object_fingerprint(&self.opened)
    }

    /// Duplicates this exact retained directory capability without consulting
    /// its pathname. Consumers such as the SQLite source VFS use the duplicate
    /// only to open admitted leaf names relative to the already-authorized
    /// directory.
    pub(crate) fn try_clone_authority_handle(&self) -> io::Result<File> {
        self.directory.try_clone()
    }

    /// Returns at most `maximum_entries` sorted child names from the retained
    /// directory handle.
    pub(crate) fn entries(&self, maximum_entries: usize) -> Result<Vec<OsString>> {
        platform::directory_entries(&self.directory, maximum_entries)
            .map_err(|error| map_open_error(self.display_path(), error))
    }

    /// Opens one child relative to this exact directory handle.
    pub(crate) fn open_child(&self, name: &OsStr) -> Result<OpenedProviderSourcePath> {
        validate_child_name(name, self.display_path())?;
        let relative_path = self.relative_path.join(name);
        let opened = platform::open_child(&self.directory, name, &self.root.inner.filesystem)
            .map_err(|error| map_open_error(&self.root.named_path().join(&relative_path), error))?;
        match opened {
            platform::OpenedPath::File {
                file,
                metadata,
                filesystem: _,
            } => {
                let stamp = platform::object_stamp(&file, &metadata)?;
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
                let stamp = platform::object_stamp(&file, &metadata)?;
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
    pub(crate) fn revalidate(&self) -> Result<()> {
        let metadata = self.directory.metadata()?;
        let current = platform::object_stamp(&self.directory, &metadata)?;
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
    pub(crate) fn len(&self) -> u64 {
        self.metadata.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn modified(&self) -> io::Result<SystemTime> {
        self.metadata.modified()
    }

    pub(crate) fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// Fixed-width observation hint for the exact file handle opened by the
    /// authority walk. This is not a substitute for [`Self::revalidate`].
    pub(crate) fn authority_fingerprint(&self) -> [u8; 32] {
        platform::object_fingerprint(&self.opened)
    }

    /// Stable ordinary-file token derived from the retained object stamp.
    ///
    /// This performs no second filesystem observation; [`Self::revalidate_leaf`]
    /// is the proof that the stamp still describes the opened object and route.
    pub(crate) fn ordinary_file_token(&self) -> [u8; 32] {
        ordinary_file_token(&self.opened)
    }

    /// Strong token for the retained file's current metadata observation.
    pub(crate) fn current_ordinary_file_token(&self) -> Result<[u8; 32]> {
        let metadata = self.file.metadata()?;
        let current = platform::object_stamp(&self.file, &metadata)?;
        Ok(ordinary_file_token(&current))
    }

    pub(crate) fn file(&self) -> &File {
        &self.file
    }

    pub(crate) fn bounded_reader(&self, maximum_bytes: u64) -> Result<Take<File>> {
        if self.len() > maximum_bytes {
            return Err(CaptureError::InvalidPayload(format!(
                "provider source file exceeds {maximum_bytes} bytes"
            )));
        }
        Ok(self.file.try_clone()?.take(self.len()))
    }

    pub(crate) fn read_all_bounded(&self, maximum_bytes: usize) -> Result<Vec<u8>> {
        let maximum_bytes_u64 = u64::try_from(maximum_bytes)
            .map_err(|_| CaptureError::SystemInvariant("bounded read size exceeds u64"))?;
        let mut reader = self.bounded_reader(maximum_bytes_u64)?;
        let capacity = usize::try_from(self.len()).map_err(|_| {
            CaptureError::InvalidPayload("provider source file is too large".into())
        })?;
        let mut bytes = Vec::with_capacity(capacity);
        reader.read_to_end(&mut bytes)?;
        if bytes.len() != capacity {
            return Err(changed_path(self.display_path()));
        }
        self.revalidate()?;
        Ok(bytes)
    }

    pub(crate) fn read_exact_range(
        &self,
        offset: u64,
        length: usize,
        maximum_bytes: usize,
    ) -> Result<Vec<u8>> {
        if length > maximum_bytes {
            return Err(CaptureError::InvalidPayload(format!(
                "provider source range exceeds {maximum_bytes} bytes"
            )));
        }
        let length_u64 = u64::try_from(length)
            .map_err(|_| CaptureError::SystemInvariant("range length exceeds u64"))?;
        let end = offset.checked_add(length_u64).ok_or_else(|| {
            CaptureError::InvalidPayload("provider source range overflows".into())
        })?;
        if end > self.len() {
            return Err(CaptureError::InvalidPayload(
                "provider source range exceeds the opened file".into(),
            ));
        }
        let mut bytes = vec![0_u8; length];
        platform::read_exact_at(&self.file, &mut bytes, offset)?;
        self.revalidate()?;
        Ok(bytes)
    }

    /// Reads an exact range from an append-friendly source and permits only a
    /// same-object metadata change while the range is read. Callers must bind
    /// the returned bytes to their own digest and frozen-prefix evidence.
    pub(crate) fn read_exact_range_allow_append(
        &self,
        offset: u64,
        length: usize,
        maximum_bytes: usize,
    ) -> Result<Vec<u8>> {
        if length > maximum_bytes {
            return Err(CaptureError::InvalidPayload(format!(
                "provider source range exceeds {maximum_bytes} bytes"
            )));
        }
        let length_u64 = u64::try_from(length)
            .map_err(|_| CaptureError::SystemInvariant("range length exceeds u64"))?;
        let end = offset.checked_add(length_u64).ok_or_else(|| {
            CaptureError::InvalidPayload("provider source range overflows".into())
        })?;
        let current_len = self.file.metadata()?.len();
        if end > current_len {
            return Err(CaptureError::InvalidPayload(
                "provider source range exceeds the opened file".into(),
            ));
        }
        let mut bytes = vec![0_u8; length];
        platform::read_exact_at(&self.file, &mut bytes, offset)?;
        self.revalidate_same_object()?;
        Ok(bytes)
    }

    /// Confirms the open handle did not change while read and its route beneath
    /// the retained authority still names the same object.
    ///
    /// Relative callers must perform one terminal [`ProviderSourceRoot::revalidate`]
    /// after all leaf checks before publishing aggregate evidence.
    pub(crate) fn revalidate_leaf(&self) -> Result<()> {
        let current_metadata = self.file.metadata()?;
        let current = platform::object_stamp(&self.file, &current_metadata)?;
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
        let named = platform::object_stamp(&file, &metadata)?;
        if named != self.opened {
            return Err(changed_path(self.display_path()));
        }
        Ok(())
    }

    /// Confirms the route still names the same ordinary file while allowing
    /// append-only metadata changes on that object.
    pub(crate) fn revalidate_same_object_leaf(&self) -> Result<()> {
        let current_metadata = self.file.metadata()?;
        let current = platform::object_stamp(&self.file, &current_metadata)?;
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
        let named = platform::object_stamp(&file, &metadata)?;
        if !platform::same_object(&named, &self.opened) {
            return Err(changed_path(self.display_path()));
        }
        Ok(())
    }

    /// Confirms same-object leaf identity and the retained root route. This is
    /// used only by append-friendly providers that separately freeze and hash
    /// the admitted byte prefix.
    pub(crate) fn revalidate_same_object(&self) -> Result<()> {
        self.revalidate_same_object_leaf()?;
        if let ProviderSourceFileRoute::Relative { root, .. } = &self.route {
            root.revalidate_same_object()?;
        }
        Ok(())
    }

    /// Confirms the leaf proof and, for a relative route, the current named root.
    pub(crate) fn revalidate(&self) -> Result<()> {
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
    digest.update(ORDINARY_FILE_TOKEN_DOMAIN);
    digest.update(b"platform\0");
    digest.update(platform::object_change_token(stamp));
    digest.finalize().into()
}

/// Opens an ordinary provider file with a no-follow component walk and retains
/// the exact opened handle for reads and final revalidation.
pub(crate) fn open_provider_source_file(path: &Path) -> Result<OpenedProviderSourceFile> {
    match open_provider_source_path(path)? {
        OpenedProviderSourcePath::File(file) => Ok(file),
        OpenedProviderSourcePath::Directory(_) => Err(invalid_path(
            path,
            "provider transcript paths must be regular files",
        )),
    }
}

pub(crate) fn open_provider_source_path(path: &Path) -> Result<OpenedProviderSourcePath> {
    let path = platform::normalize_authority_path(path);
    ensure_absolute_traversal_free(&path)?;
    let opened = platform::open_absolute(&path).map_err(|error| map_open_error(&path, error))?;
    match opened {
        platform::OpenedPath::File {
            file,
            metadata,
            filesystem: _,
        } => {
            let stamp = platform::object_stamp(&file, &metadata)?;
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
            let stamp = platform::object_stamp(&file, &metadata)?;
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

fn ensure_absolute_traversal_free(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(invalid_path(
            path,
            "provider source authority paths must be absolute and traversal-free",
        ));
    }
    Ok(())
}

fn validate_relative_path(path: &Path) -> Result<()> {
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(invalid_path(
            path,
            "provider source descendants must be traversal-free relative paths",
        ));
    }
    Ok(())
}

fn validate_child_name(name: &OsStr, path: &Path) -> Result<()> {
    if name.is_empty()
        || name == OsStr::new(".")
        || name == OsStr::new("..")
        || Path::new(name).components().count() != 1
        || !matches!(
            Path::new(name).components().next(),
            Some(Component::Normal(_))
        )
    {
        return Err(invalid_path(
            path,
            "provider source child names must be single normal components",
        ));
    }
    Ok(())
}

fn map_open_error(path: &Path, error: AuthorityOpenError) -> CaptureError {
    match error {
        AuthorityOpenError::Io(error) => error.into(),
        AuthorityOpenError::Rejected(reason) => invalid_path(path, reason),
    }
}

fn map_changed_open_error(path: &Path, error: AuthorityOpenError) -> CaptureError {
    match error {
        AuthorityOpenError::Io(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound
                    | io::ErrorKind::InvalidData
                    | io::ErrorKind::PermissionDenied
            ) =>
        {
            changed_path(path)
        }
        AuthorityOpenError::Rejected(_) => changed_path(path),
        AuthorityOpenError::Io(error) => error.into(),
    }
}

/// Reason recorded when a provider source path component is neither a regular
/// file nor a directory (for example a Unix-domain socket, FIFO, or device
/// node). Traversal callers can skip such entries safely without treating the
/// enclosing provider source as unreadable.
pub(crate) const NON_REGULAR_PROVIDER_SOURCE_REASON: &str =
    "provider source paths must be regular files or directories";

/// True when `error` is the safe rejection of a non-regular special-file entry
/// (see [`NON_REGULAR_PROVIDER_SOURCE_REASON`]), as opposed to a symlink
/// rejection or a genuine IO failure that must fail the enclosing traversal.
pub(crate) fn is_non_regular_source_rejection(error: &CaptureError) -> bool {
    matches!(
        error,
        CaptureError::InvalidProviderTranscriptPath { reason, .. }
            if *reason == NON_REGULAR_PROVIDER_SOURCE_REASON
    )
}

fn invalid_path(path: &Path, reason: &'static str) -> CaptureError {
    CaptureError::InvalidProviderTranscriptPath {
        path: path.to_path_buf(),
        reason,
    }
}

fn changed_path(path: &Path) -> CaptureError {
    CaptureError::InvalidProviderTranscriptPath {
        path: path.to_path_buf(),
        reason: "provider source changed while its authority handle was retained",
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Read, time::Duration};

    use super::*;

    #[test]
    fn retained_root_reads_the_original_tree_after_named_root_replacement() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("root");
        let moved = temp.path().join("moved-root");
        let replacement = temp.path().join("replacement");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&replacement).unwrap();
        fs::write(root.join("source.jsonl"), b"original\n").unwrap();
        fs::write(replacement.join("source.jsonl"), b"replacement\n").unwrap();
        let authority = ProviderSourceRoot::open(&root).unwrap();

        fs::rename(&root, &moved).unwrap();
        fs::rename(&replacement, &root).unwrap();

        let source = authority.open_file(Path::new("source.jsonl")).unwrap();
        let mut reader = source.bounded_reader(64).unwrap();
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"original\n");
        assert!(authority.revalidate().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn replaced_descendant_symlink_cannot_escape_retained_root() {
        use std::os::unix::fs::symlink;

        let temp = crate::test_support_paths::tempdir().unwrap();
        let outside = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("root");
        let nested = root.join("nested");
        let moved = root.join("moved-nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("source.jsonl"), b"inside\n").unwrap();
        fs::write(outside.path().join("source.jsonl"), b"outside\n").unwrap();
        let authority = ProviderSourceRoot::open(&root).unwrap();

        fs::rename(&nested, &moved).unwrap();
        symlink(outside.path(), &nested).unwrap();

        assert!(authority
            .open_file(Path::new("nested/source.jsonl"))
            .is_err());
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
    #[test]
    fn symlinked_ancestor_is_classified_as_a_rejected_provider_path() {
        use std::os::unix::fs::symlink;

        let temp = crate::test_support_paths::tempdir().unwrap();
        let target = temp.path().join("target");
        let linked = temp.path().join("linked");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("source.jsonl"), b"inside\n").unwrap();
        symlink(&target, &linked).unwrap();

        let error = open_provider_source_file(&linked.join("source.jsonl")).unwrap_err();

        assert!(matches!(
            error,
            CaptureError::InvalidProviderTranscriptPath { reason, .. }
                if reason.contains("symlinked provider source path components")
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
    #[test]
    fn plain_file_ancestor_preserves_the_raw_not_a_directory_io_error() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let plain_file = temp.path().join("plain-file");
        fs::write(&plain_file, b"ordinary").unwrap();

        let error = open_provider_source_file(&plain_file.join("source.jsonl")).unwrap_err();

        assert!(matches!(
            error,
            CaptureError::Io(error) if error.raw_os_error() == Some(libc::ENOTDIR)
        ));
    }

    #[test]
    fn descendants_reject_absolute_and_parent_escape() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let authority = ProviderSourceRoot::open(temp.path()).unwrap();

        assert!(authority.open_path(Path::new("../outside")).is_err());
        assert!(authority.open_path(temp.path()).is_err());
    }

    #[test]
    fn exact_range_reads_from_open_handle_and_detects_named_replacement() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        let moved = temp.path().join("moved.jsonl");
        fs::write(&path, b"0123456789").unwrap();
        let root = ProviderSourceRoot::open(temp.path()).unwrap();
        let source = root.open_file(Path::new("source.jsonl")).unwrap();

        assert_eq!(source.read_exact_range(3, 4, 4).unwrap(), b"3456");
        fs::rename(&path, &moved).unwrap();
        fs::write(&path, b"abcdefghij").unwrap();

        let mut retained = source.bounded_reader(10).unwrap();
        let mut bytes = Vec::new();
        retained.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"0123456789");
        assert!(source.revalidate_leaf().is_err());
        assert!(source.revalidate().is_err());
    }

    #[test]
    fn active_source_family_contract_retained_handle_allows_growth_and_rejects_replacement() {
        use std::io::Write;

        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        let moved = temp.path().join("moved.jsonl");
        fs::write(&path, b"first\n").unwrap();
        let root = ProviderSourceRoot::open(temp.path()).unwrap();
        let source = root.open_file(Path::new("source.jsonl")).unwrap();

        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"second\n")
            .unwrap();
        fs::write(temp.path().join("new-sibling.jsonl"), b"sibling\n").unwrap();
        assert_eq!(
            source.read_exact_range_allow_append(0, 6, 6).unwrap(),
            b"first\n"
        );
        assert!(source.revalidate_same_object().is_ok());
        assert!(root.revalidate_same_object().is_ok());
        assert!(source.revalidate().is_err());

        fs::rename(&path, &moved).unwrap();
        fs::write(&path, b"replacement\n").unwrap();
        assert!(source.revalidate_same_object_leaf().is_err());
        assert!(source.read_exact_range_allow_append(0, 6, 6).is_err());
    }

    #[test]
    fn leaf_revalidation_and_one_terminal_root_fence_have_distinct_purposes() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("root");
        let moved = temp.path().join("moved-root");
        let replacement = temp.path().join("replacement");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&replacement).unwrap();
        fs::write(root.join("source.jsonl"), b"original\n").unwrap();
        fs::write(replacement.join("source.jsonl"), b"replacement\n").unwrap();
        let authority = ProviderSourceRoot::open(&root).unwrap();
        let source = authority.open_file(Path::new("source.jsonl")).unwrap();

        fs::rename(&root, &moved).unwrap();
        fs::rename(&replacement, &root).unwrap();

        source.revalidate_leaf().unwrap();
        assert!(authority.revalidate_same_object().is_err());
        assert!(authority.revalidate().is_err());
        assert!(source.revalidate_same_object().is_err());
        assert!(source.revalidate().is_err());
    }

    #[test]
    fn authority_fingerprints_are_stable_for_the_same_objects_and_change_on_mutation() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("root");
        let path = root.join("source.json");
        fs::create_dir(&root).unwrap();
        fs::write(&path, b"before").unwrap();

        let first_root = ProviderSourceRoot::open(&root).unwrap();
        let first_file = first_root.open_file(Path::new("source.json")).unwrap();
        let reopened_root = ProviderSourceRoot::open(&root).unwrap();
        let reopened_file = reopened_root.open_file(Path::new("source.json")).unwrap();
        assert_eq!(
            first_root.authority_fingerprint(),
            reopened_root.authority_fingerprint()
        );
        assert_eq!(
            first_file.authority_fingerprint(),
            reopened_file.authority_fingerprint()
        );

        let changed_modified = fs::metadata(&path)
            .unwrap()
            .modified()
            .unwrap()
            .checked_add(Duration::from_secs(2))
            .unwrap();
        fs::write(&path, b"after!").unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(changed_modified))
            .unwrap();
        let changed_file = ProviderSourceRoot::open(&root)
            .unwrap()
            .open_file(Path::new("source.json"))
            .unwrap();
        assert_ne!(
            first_file.authority_fingerprint(),
            changed_file.authority_fingerprint()
        );
        assert!(first_file.revalidate().is_err());

        let moved_root = temp.path().join("moved-root");
        fs::rename(&root, &moved_root).unwrap();
        fs::create_dir(&root).unwrap();
        let replacement_root = ProviderSourceRoot::open(&root).unwrap();
        assert_ne!(
            first_root.authority_fingerprint(),
            replacement_root.authority_fingerprint()
        );
        assert!(first_root.revalidate().is_err());
    }
}
