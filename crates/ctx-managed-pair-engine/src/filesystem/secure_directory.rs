use std::{
    ffi::OsStr,
    fs::File,
    path::{Component, Path},
};

use anyhow::{anyhow, bail, Context as _, Result};

#[cfg(windows)]
use super::windows_file_information;
use super::{file_information, validate_absolute_root};
#[cfg(windows)]
use std::fs::OpenOptions;

#[derive(Debug)]
pub(super) struct SecureDirectory {
    pub(super) file: File,
    #[cfg(windows)]
    _ancestors: Vec<File>,
}

pub(super) struct EntryMetadata {
    pub(super) is_file: bool,
    pub(super) is_symlink: bool,
    pub(super) device: u64,
    pub(super) file: u64,
    #[cfg(windows)]
    pub(super) attributes: u32,
}

impl SecureDirectory {
    pub(super) fn require_path_identity(&self, path: &Path) -> Result<()> {
        let rebound = Self::open(path)?;
        let expected = file_information(&self.file, "managed-pair directory")?;
        let actual = file_information(&rebound.file, "managed-pair directory")?;
        if expected.0 != actual.0 || expected.1 != actual.1 {
            bail!(
                "managed-pair directory identity changed at {}",
                path.display()
            );
        }
        Ok(())
    }

    #[cfg(unix)]
    pub(super) fn open(path: &Path) -> Result<Self> {
        use std::{
            ffi::CString,
            os::unix::{
                ffi::OsStrExt as _,
                io::{AsRawFd as _, FromRawFd as _},
            },
        };

        validate_absolute_root(path, "managed-pair directory")?;
        let root = CString::new("/").map_err(|_| anyhow!("invalid filesystem root"))?;
        let root_fd = unsafe {
            libc::open(
                root.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY,
            )
        };
        if root_fd < 0 {
            return Err(std::io::Error::last_os_error()).context("open filesystem root");
        }
        let mut current = unsafe { File::from_raw_fd(root_fd) };
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => {
                    let name = CString::new(name.as_bytes())
                        .map_err(|_| anyhow!("managed-pair path contains a NUL"))?;
                    let fd = unsafe {
                        libc::openat(
                            current.as_raw_fd(),
                            name.as_ptr(),
                            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY,
                        )
                    };
                    if fd < 0 {
                        return Err(std::io::Error::last_os_error()).with_context(|| {
                            format!("open no-follow managed-pair directory {}", path.display())
                        });
                    }
                    current = unsafe { File::from_raw_fd(fd) };
                }
                _ => bail!("managed-pair directory is not a safe absolute path"),
            }
        }
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let metadata = current.metadata()?;
        if !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o022 != 0
        {
            bail!(
                "managed-pair directory is not owner-safe: {}",
                path.display()
            );
        }
        Ok(Self { file: current })
    }

    #[cfg(windows)]
    pub(super) fn open(path: &Path) -> Result<Self> {
        use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, READ_CONTROL,
        };

        validate_absolute_root(path, "managed-pair directory")?;
        let mut paths: Vec<_> = path
            .ancestors()
            .filter(|ancestor| !ancestor.as_os_str().is_empty())
            .collect();
        paths.reverse();
        let mut handles = Vec::with_capacity(paths.len());
        for ancestor in paths {
            let access = FILE_GENERIC_READ
                | READ_CONTROL
                | if ancestor == path {
                    FILE_GENERIC_WRITE
                } else {
                    0
                };
            let mut options = OpenOptions::new();
            options
                .access_mode(access)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
            let handle = options.open(ancestor).with_context(|| {
                format!(
                    "open protected managed-pair ancestor {}",
                    ancestor.display()
                )
            })?;
            let metadata = handle.metadata()?;
            if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            {
                bail!(
                    "managed-pair directory traverses a reparse point: {}",
                    ancestor.display()
                );
            }
            handles.push(handle);
        }
        let file = handles
            .pop()
            .ok_or_else(|| anyhow!("managed-pair directory has no Windows handle"))?;
        ctx_history_platform::platform_security::verify_private_directory_handle(&file)?;
        Ok(Self {
            file,
            _ancestors: handles,
        })
    }

    #[cfg(not(any(unix, windows)))]
    pub(super) fn open(_path: &Path) -> Result<Self> {
        bail!("managed-pair directories are unsupported on this platform")
    }

    #[cfg(unix)]
    pub(super) fn open_child_directory(&self, name: &OsStr) -> Result<Self> {
        use std::{
            ffi::CString,
            os::unix::{
                ffi::OsStrExt as _,
                fs::{MetadataExt as _, PermissionsExt as _},
                io::{AsRawFd as _, FromRawFd as _},
            },
        };
        let name = CString::new(name.as_bytes())
            .map_err(|_| anyhow!("managed-pair directory name contains a NUL"))?;
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .context("open managed-pair child directory by retained parent");
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata()?;
        if !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o022 != 0
        {
            bail!("managed-pair child directory is not owner-safe");
        }
        Ok(Self { file })
    }

    #[cfg(windows)]
    pub(super) fn open_child_directory(&self, name: &OsStr) -> Result<Self> {
        use std::os::windows::fs::MetadataExt as _;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL,
        };
        let file = self.open_relative_kind(
            name,
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            windows_sys::Wdk::Storage::FileSystem::FILE_OPEN,
            true,
        )?;
        let metadata = file.metadata()?;
        if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            bail!("managed-pair child directory is a reparse point");
        }
        ctx_history_platform::platform_security::verify_private_directory_handle(&file)?;
        Ok(Self {
            file,
            _ancestors: Vec::new(),
        })
    }

    #[cfg(not(any(unix, windows)))]
    pub(super) fn open_child_directory(&self, _name: &OsStr) -> Result<Self> {
        bail!("managed-pair directories are unsupported on this platform")
    }

    #[cfg(unix)]
    pub(super) fn open_file(&self, name: &OsStr, _path: &Path) -> Result<File> {
        use std::{
            ffi::CString,
            os::unix::{
                ffi::OsStrExt as _,
                io::{AsRawFd as _, FromRawFd as _},
            },
        };
        let name = CString::new(name.as_bytes())
            .map_err(|_| anyhow!("managed-pair file name contains a NUL"))?;
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .context("open no-follow managed-pair file");
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    #[cfg(windows)]
    pub(super) fn open_file(&self, name: &OsStr, _path: &Path) -> Result<File> {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_GENERIC_READ, FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL, SYNCHRONIZE,
        };
        self.open_relative(
            name,
            FILE_GENERIC_READ | READ_CONTROL | SYNCHRONIZE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            windows_sys::Wdk::Storage::FileSystem::FILE_OPEN,
        )
    }

    #[cfg(unix)]
    pub(super) fn create_new(&self, name: &OsStr, _path: &Path, executable: bool) -> Result<File> {
        use std::{
            ffi::CString,
            os::unix::{
                ffi::OsStrExt as _,
                io::{AsRawFd as _, FromRawFd as _},
            },
        };
        let name = CString::new(name.as_bytes())
            .map_err(|_| anyhow!("managed-pair file name contains a NUL"))?;
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                if executable { 0o700 } else { 0o600 },
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).context("create managed-pair file");
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    #[cfg(windows)]
    pub(super) fn create_new(&self, name: &OsStr, _path: &Path, _executable: bool) -> Result<File> {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_READ, READ_CONTROL, SYNCHRONIZE,
            WRITE_DAC,
        };
        self.open_relative(
            name,
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | READ_CONTROL | WRITE_DAC | SYNCHRONIZE,
            FILE_SHARE_READ,
            windows_sys::Wdk::Storage::FileSystem::FILE_CREATE,
        )
    }

    #[cfg(unix)]
    #[allow(clippy::unnecessary_cast)]
    pub(super) fn entry_metadata(
        &self,
        name: &OsStr,
        _path: &Path,
    ) -> Result<Option<EntryMetadata>> {
        use std::{
            ffi::CString,
            mem::MaybeUninit,
            os::unix::{ffi::OsStrExt as _, io::AsRawFd as _},
        };
        let name = CString::new(name.as_bytes())
            .map_err(|_| anyhow!("managed-pair file name contains a NUL"))?;
        let mut stat = MaybeUninit::<libc::stat>::zeroed();
        if unsafe {
            libc::fstatat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(error).context("inspect managed-pair directory entry");
        }
        let stat = unsafe { stat.assume_init() };
        let kind = stat.st_mode & libc::S_IFMT;
        Ok(Some(EntryMetadata {
            is_file: kind == libc::S_IFREG,
            is_symlink: kind == libc::S_IFLNK,
            device: stat.st_dev as u64,
            file: stat.st_ino as u64,
        }))
    }

    #[cfg(windows)]
    pub(super) fn entry_metadata(
        &self,
        name: &OsStr,
        path: &Path,
    ) -> Result<Option<EntryMetadata>> {
        use std::os::windows::fs::MetadataExt as _;
        let file = match self.open_file(name, path) {
            Ok(file) => file,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(None)
            }
            Err(error) => return Err(error).context("inspect managed-pair directory entry"),
        };
        let metadata = file.metadata()?;
        let (device, identity, _) = windows_file_information(&file, "managed-pair entry")?;
        Ok(Some(EntryMetadata {
            is_file: metadata.is_file(),
            is_symlink: metadata.file_attributes() & 0x400 != 0,
            device,
            file: identity,
            attributes: metadata.file_attributes(),
        }))
    }

    #[cfg(unix)]
    pub(super) fn remove_file(&self, name: &OsStr, _path: &Path) -> Result<()> {
        use std::{
            ffi::CString,
            os::unix::{ffi::OsStrExt as _, io::AsRawFd as _},
        };
        let name = CString::new(name.as_bytes())
            .map_err(|_| anyhow!("managed-pair file name contains a NUL"))?;
        if unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(std::io::Error::last_os_error()).context("unlink managed-pair file");
        }
        Ok(())
    }

    #[cfg(windows)]
    pub(super) fn remove_file(&self, name: &OsStr, _path: &Path) -> Result<()> {
        use std::{mem::size_of, os::windows::io::AsRawHandle as _};
        use windows_sys::Win32::Storage::FileSystem::{
            FileDispositionInfo, SetFileInformationByHandle, DELETE, FILE_DISPOSITION_INFO,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL, SYNCHRONIZE,
        };

        let file = self.open_relative(
            name,
            DELETE | READ_CONTROL | SYNCHRONIZE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            windows_sys::Wdk::Storage::FileSystem::FILE_OPEN,
        )?;
        let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
        if unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle().cast(),
                FileDispositionInfo,
                (&raw const disposition).cast(),
                u32::try_from(size_of::<FILE_DISPOSITION_INFO>())?,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error())
                .context("unlink untrusted managed-pair file by handle");
        }
        Ok(())
    }

    #[cfg(unix)]
    pub(super) fn remove_directory(&self, name: &OsStr) -> Result<()> {
        use std::{
            ffi::CString,
            os::unix::{ffi::OsStrExt as _, io::AsRawFd as _},
        };
        let name = CString::new(name.as_bytes())
            .map_err(|_| anyhow!("managed-pair directory name contains a NUL"))?;
        if unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0
        {
            return Err(std::io::Error::last_os_error())
                .context("remove managed-pair directory relative to retained parent");
        }
        Ok(())
    }

    #[cfg(windows)]
    pub(super) fn remove_directory(&self, name: &OsStr) -> Result<()> {
        use std::{mem::size_of, os::windows::io::AsRawHandle as _};
        use windows_sys::Win32::Storage::FileSystem::{
            FileDispositionInfo, SetFileInformationByHandle, DELETE, FILE_DISPOSITION_INFO,
            FILE_SHARE_READ, READ_CONTROL, SYNCHRONIZE,
        };

        let directory = self.open_relative_kind(
            name,
            DELETE | READ_CONTROL | SYNCHRONIZE,
            FILE_SHARE_READ,
            windows_sys::Wdk::Storage::FileSystem::FILE_OPEN,
            true,
        )?;
        let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
        if unsafe {
            SetFileInformationByHandle(
                directory.as_raw_handle().cast(),
                FileDispositionInfo,
                (&raw const disposition).cast(),
                u32::try_from(size_of::<FILE_DISPOSITION_INFO>())?,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error())
                .context("remove managed-pair directory by retained parent handle");
        }
        Ok(())
    }

    #[cfg(windows)]
    pub(super) fn open_relative(
        &self,
        name: &OsStr,
        access: u32,
        share_mode: u32,
        disposition: u32,
    ) -> Result<File> {
        self.open_relative_kind(name, access, share_mode, disposition, false)
    }

    #[cfg(windows)]
    pub(super) fn open_relative_kind(
        &self,
        name: &OsStr,
        access: u32,
        share_mode: u32,
        disposition: u32,
        directory: bool,
    ) -> Result<File> {
        use std::os::windows::io::{FromRawHandle as _, RawHandle};
        use std::os::windows::{ffi::OsStrExt as _, io::AsRawHandle as _};
        use windows_sys::{
            Wdk::{
                Foundation::OBJECT_ATTRIBUTES,
                Storage::FileSystem::{
                    NtCreateFile, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE,
                    FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
                },
            },
            Win32::{
                Foundation::{RtlNtStatusToDosError, HANDLE, OBJ_CASE_INSENSITIVE, UNICODE_STRING},
                Storage::FileSystem::FILE_ATTRIBUTE_NORMAL,
                System::IO::IO_STATUS_BLOCK,
            },
        };

        let path = Path::new(name);
        let mut components = path.components();
        if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
            bail!("managed-pair child name is not one fixed path component");
        }
        let mut wide: Vec<u16> = name.encode_wide().collect();
        if wide.is_empty() || wide.contains(&0) {
            bail!("managed-pair child name is invalid");
        }
        let byte_len = wide
            .len()
            .checked_mul(std::mem::size_of::<u16>())
            .and_then(|length| u16::try_from(length).ok())
            .ok_or_else(|| anyhow!("managed-pair child name is too long"))?;
        let mut unicode = UNICODE_STRING {
            Length: byte_len,
            MaximumLength: byte_len,
            Buffer: wide.as_mut_ptr(),
        };
        let object_attributes = OBJECT_ATTRIBUTES {
            Length: u32::try_from(std::mem::size_of::<OBJECT_ATTRIBUTES>())?,
            RootDirectory: self.file.as_raw_handle().cast(),
            ObjectName: &mut unicode,
            Attributes: OBJ_CASE_INSENSITIVE,
            SecurityDescriptor: std::ptr::null(),
            SecurityQualityOfService: std::ptr::null(),
        };
        let mut status_block = IO_STATUS_BLOCK::default();
        let mut handle: HANDLE = std::ptr::null_mut();
        let status = unsafe {
            NtCreateFile(
                &mut handle,
                access,
                &object_attributes,
                &mut status_block,
                std::ptr::null(),
                FILE_ATTRIBUTE_NORMAL,
                share_mode,
                disposition,
                (if directory {
                    FILE_DIRECTORY_FILE
                } else {
                    FILE_NON_DIRECTORY_FILE
                }) | FILE_OPEN_REPARSE_POINT
                    | FILE_SYNCHRONOUS_IO_NONALERT,
                std::ptr::null(),
                0,
            )
        };
        if status < 0 {
            return Err(std::io::Error::from_raw_os_error(
                unsafe { RtlNtStatusToDosError(status) } as i32,
            )
            .into());
        }
        Ok(unsafe { File::from_raw_handle(handle as RawHandle) })
    }

    pub(super) fn sync(&self) -> Result<()> {
        #[cfg(unix)]
        self.file.sync_all()?;
        Ok(())
    }
}
