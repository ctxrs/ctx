use std::{
    collections::BTreeSet,
    ffi::{CStr, CString, OsStr, OsString},
    fs::{File, Metadata},
    io,
    mem::MaybeUninit,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::{FileExt, MetadataExt},
        },
    },
    path::{Component, Path, PathBuf},
};

use sha2::{Digest, Sha256};

use super::AuthorityOpenError;

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
use std::mem::size_of;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ObjectStamp {
    device: u64,
    inode: u64,
    mode: u32,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FilesystemIdentity {
    filesystem_type: i64,
    mount_id: u64,
}

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FilesystemIdentity {
    filesystem_name: String,
    filesystem_id: [u8; size_of::<libc::fsid_t>()],
}

pub(super) enum OpenedPath {
    File {
        file: File,
        metadata: Metadata,
        filesystem: FilesystemIdentity,
    },
    Directory {
        file: File,
        metadata: Metadata,
        filesystem: FilesystemIdentity,
    },
}

pub(super) fn normalize_authority_path(path: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let mut components = path.components();
        if !matches!(components.next(), Some(Component::RootDir)) {
            return path.to_path_buf();
        }
        let Some(Component::Normal(first)) = components.next() else {
            return path.to_path_buf();
        };
        if matches!(first.as_bytes(), b"tmp" | b"var" | b"etc") {
            let mut normalized = PathBuf::from("/private");
            normalized.push(first);
            for component in components {
                normalized.push(component.as_os_str());
            }
            return normalized;
        }
    }
    path.to_path_buf()
}

pub(super) fn open_absolute(path: &Path) -> Result<OpenedPath, AuthorityOpenError> {
    let mut components = path.components().peekable();
    if !matches!(components.next(), Some(Component::RootDir)) {
        return Err(AuthorityOpenError::Rejected(
            "Unix provider source authority paths must be absolute",
        ));
    }
    let mut current = open_component(
        libc::AT_FDCWD,
        OsStr::new("/"),
        Some(ExpectedType::Directory),
    )?;
    let mut saw_component = false;
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            if matches!(component, Component::CurDir) {
                continue;
            }
            return Err(AuthorityOpenError::Rejected(
                "Unix provider source paths contain an unsupported component",
            ));
        };
        saw_component = true;
        let expected = components.peek().map(|_| ExpectedType::Directory);
        current = open_component(current.as_raw_fd(), name, expected)?;
    }
    if !saw_component {
        return classify_opened(current);
    }
    classify_opened(current)
}

pub(super) fn open_child(
    parent: &File,
    name: &OsStr,
    filesystem: &FilesystemIdentity,
) -> Result<OpenedPath, AuthorityOpenError> {
    let file = open_component(parent.as_raw_fd(), name, None)?;
    let opened = classify_opened(file)?;
    let child_filesystem = match &opened {
        OpenedPath::File { filesystem, .. } | OpenedPath::Directory { filesystem, .. } => {
            filesystem
        }
    };
    if child_filesystem != filesystem {
        return Err(AuthorityOpenError::Rejected(
            "provider source descendants may not cross filesystem mounts",
        ));
    }
    Ok(opened)
}

pub(super) fn directory_entries(
    directory: &File,
    maximum_entries: usize,
) -> Result<Vec<OsString>, AuthorityOpenError> {
    let duplicate = unsafe { libc::fcntl(directory.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error().into());
    }
    let stream = unsafe { libc::fdopendir(duplicate) };
    if stream.is_null() {
        let cause = io::Error::last_os_error();
        unsafe {
            libc::close(duplicate);
        }
        return Err(cause.into());
    }
    unsafe {
        libc::rewinddir(stream);
    }

    let mut names = BTreeSet::new();
    let result = loop {
        clear_errno();
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            let errno = current_errno();
            if errno == 0 {
                break Ok(());
            }
            break Err(AuthorityOpenError::Io(io::Error::from_raw_os_error(errno)));
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        let name = OsString::from_vec(name.to_vec());
        if !names.contains(&name) && names.len() >= maximum_entries {
            break Err(AuthorityOpenError::Rejected(
                "provider source directory exceeds its bounded entry budget",
            ));
        }
        names.insert(name);
    };
    let close_result = unsafe { libc::closedir(stream) };
    result?;
    if close_result != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(names.into_iter().collect())
}

pub(super) fn object_stamp(_file: &File, metadata: &Metadata) -> io::Result<ObjectStamp> {
    Ok(ObjectStamp {
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
        length: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    })
}

pub(super) fn object_fingerprint(stamp: &ObjectStamp) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx.retained-authority.unix-object-v1\0");
    digest.update(stamp.device.to_be_bytes());
    digest.update(stamp.inode.to_be_bytes());
    digest.update(stamp.mode.to_be_bytes());
    digest.update(stamp.length.to_be_bytes());
    digest.update(stamp.modified_seconds.to_be_bytes());
    digest.update(stamp.modified_nanoseconds.to_be_bytes());
    digest.update(stamp.changed_seconds.to_be_bytes());
    digest.update(stamp.changed_nanoseconds.to_be_bytes());
    digest.finalize().into()
}

pub(super) fn same_object(left: &ObjectStamp, right: &ObjectStamp) -> bool {
    left.device == right.device && left.inode == right.inode && left.mode == right.mode
}

pub(super) fn object_change_token(stamp: &ObjectStamp) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(super::ORDINARY_FILE_TOKEN_DOMAIN);
    digest.update(b"unix\0");
    digest.update(stamp.device.to_le_bytes());
    digest.update(stamp.inode.to_le_bytes());
    digest.update(stamp.changed_seconds.to_le_bytes());
    digest.update(stamp.changed_nanoseconds.to_le_bytes());
    digest.finalize().into()
}

#[allow(
    dead_code,
    reason = "exact range hydration migrates to this positioned read API in follow-up slices"
)]
pub(super) fn read_exact_at(file: &File, mut bytes: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !bytes.is_empty() {
        let read = file.read_at(bytes, offset)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "provider source changed during exact range read",
            ));
        }
        offset = offset
            .checked_add(read as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "range offset overflow"))?;
        bytes = &mut bytes[read..];
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExpectedType {
    Directory,
}

fn open_component(
    parent: libc::c_int,
    name: &OsStr,
    expected: Option<ExpectedType>,
) -> Result<File, AuthorityOpenError> {
    let name = CString::new(name.as_bytes()).map_err(|_| {
        AuthorityOpenError::Rejected("provider source path components may not contain NUL bytes")
    })?;
    let mut flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
    if expected == Some(ExpectedType::Directory) {
        flags |= libc::O_DIRECTORY;
    }
    let descriptor = unsafe { libc::openat(parent, name.as_ptr(), flags) };
    if descriptor < 0 {
        let cause = io::Error::last_os_error();
        return match cause.raw_os_error() {
            Some(libc::ELOOP) => Err(AuthorityOpenError::Rejected(
                "symlinked provider source path components are rejected",
            )),
            Some(libc::ENXIO | libc::ENODEV) => Err(AuthorityOpenError::Rejected(
                "provider source paths must be regular files or directories",
            )),
            _ => Err(cause.into()),
        };
    }
    let file = unsafe { File::from_raw_fd(descriptor) };
    let metadata = file.metadata()?;
    if expected == Some(ExpectedType::Directory) && !metadata.file_type().is_dir() {
        return Err(AuthorityOpenError::Rejected(
            "provider source ancestor components must be directories",
        ));
    }
    Ok(file)
}

fn classify_opened(file: File) -> Result<OpenedPath, AuthorityOpenError> {
    let metadata = file.metadata()?;
    let filesystem = filesystem_identity(&file)?;
    if metadata.file_type().is_file() {
        Ok(OpenedPath::File {
            file,
            metadata,
            filesystem,
        })
    } else if metadata.file_type().is_dir() {
        Ok(OpenedPath::Directory {
            file,
            metadata,
            filesystem,
        })
    } else {
        Err(AuthorityOpenError::Rejected(
            "provider source paths must be regular files or directories",
        ))
    }
}

#[cfg(target_os = "linux")]
fn filesystem_identity(file: &File) -> Result<FilesystemIdentity, AuthorityOpenError> {
    let mut filesystem = MaybeUninit::<libc::statfs>::zeroed();
    if unsafe { libc::fstatfs(file.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    let filesystem = unsafe { filesystem.assume_init() };
    let filesystem_type = filesystem.f_type;
    if !linux_filesystem_is_qualified(filesystem_type) {
        return Err(AuthorityOpenError::Rejected(
            "provider source roots require a qualified local Linux filesystem",
        ));
    }

    let empty = c"";
    let mut statx = MaybeUninit::<libc::statx>::zeroed();
    let result = unsafe {
        libc::statx(
            file.as_raw_fd(),
            empty.as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_STATX_DONT_SYNC,
            libc::STATX_MNT_ID,
            statx.as_mut_ptr(),
        )
    };
    if result != 0 {
        return Err(AuthorityOpenError::Rejected(
            "Linux mount identity is unavailable for provider source authority",
        ));
    }
    let statx = unsafe { statx.assume_init() };
    if statx.stx_mask & libc::STATX_MNT_ID == 0 {
        return Err(AuthorityOpenError::Rejected(
            "Linux mount identity is unavailable for provider source authority",
        ));
    }
    Ok(FilesystemIdentity {
        filesystem_type,
        mount_id: statx.stx_mnt_id,
    })
}

#[cfg(target_os = "linux")]
fn linux_filesystem_is_qualified(filesystem_type: i64) -> bool {
    const EXT_SUPER_MAGIC: i64 = 0xEF53;
    const XFS_SUPER_MAGIC: i64 = 0x5846_5342;
    const BTRFS_SUPER_MAGIC: i64 = 0x9123_683E;
    const F2FS_SUPER_MAGIC: i64 = 0xF2F5_2010;

    matches!(
        filesystem_type,
        EXT_SUPER_MAGIC | XFS_SUPER_MAGIC | BTRFS_SUPER_MAGIC | F2FS_SUPER_MAGIC
    )
}

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
fn filesystem_identity(file: &File) -> Result<FilesystemIdentity, AuthorityOpenError> {
    let mut filesystem = MaybeUninit::<libc::statfs>::zeroed();
    if unsafe { libc::fstatfs(file.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    let filesystem = unsafe { filesystem.assume_init() };
    #[cfg(target_os = "macos")]
    let is_local = filesystem.f_flags & (libc::MNT_LOCAL as u32) != 0;
    #[cfg(target_os = "freebsd")]
    let is_local = filesystem.f_flags & (libc::MNT_LOCAL as u64) != 0;
    if !is_local {
        return Err(AuthorityOpenError::Rejected(
            "network provider source roots are rejected",
        ));
    }
    let name = unsafe { CStr::from_ptr(filesystem.f_fstypename.as_ptr()) }
        .to_str()
        .map_err(|_| {
            AuthorityOpenError::Rejected("provider source filesystem names must be valid UTF-8")
        })?
        .to_ascii_lowercase();
    #[cfg(target_os = "macos")]
    let qualified = matches!(name.as_str(), "apfs" | "hfs");
    #[cfg(target_os = "freebsd")]
    let qualified = name == "ufs";
    if !qualified {
        return Err(AuthorityOpenError::Rejected(
            "provider source roots require a qualified local filesystem",
        ));
    }
    let mut filesystem_id = [0_u8; size_of::<libc::fsid_t>()];
    unsafe {
        std::ptr::copy_nonoverlapping(
            (&filesystem.f_fsid as *const libc::fsid_t).cast::<u8>(),
            filesystem_id.as_mut_ptr(),
            filesystem_id.len(),
        );
    }
    Ok(FilesystemIdentity {
        filesystem_name: name,
        filesystem_id,
    })
}

#[cfg(target_os = "linux")]
fn errno_location() -> *mut libc::c_int {
    unsafe { libc::__errno_location() }
}

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
fn errno_location() -> *mut libc::c_int {
    unsafe { libc::__error() }
}

fn clear_errno() {
    unsafe {
        *errno_location() = 0;
    }
}

fn current_errno() -> libc::c_int {
    unsafe { *errno_location() }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[test]
    fn filesystem_policy_rejects_network_fuse_and_virtual_roots() {
        const NFS_SUPER_MAGIC: i64 = 0x6969;
        const CIFS_SUPER_MAGIC: i64 = 0xFF53_4D42;
        const FUSE_SUPER_MAGIC: i64 = 0x6573_5546;
        const PROC_SUPER_MAGIC: i64 = 0x9FA0;

        for filesystem in [
            NFS_SUPER_MAGIC,
            CIFS_SUPER_MAGIC,
            FUSE_SUPER_MAGIC,
            PROC_SUPER_MAGIC,
        ] {
            assert!(!super::linux_filesystem_is_qualified(filesystem));
        }
        assert!(super::linux_filesystem_is_qualified(0xEF53));
    }
}
