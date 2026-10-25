use std::{
    ffi::OsStr,
    fs::File,
    io,
    mem::MaybeUninit,
    os::windows::{
        ffi::OsStrExt as _,
        fs::FileExt as _,
        io::{AsRawHandle as _, FromRawHandle as _, RawHandle},
    },
    thread,
    time::Duration,
};

use ctx_history_platform::platform_security::{
    restrict_private_file_handle, verify_private_directory_handle, verify_private_file_handle,
};
use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            NtCreateFile, FILE_CREATE, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT,
            FILE_SYNCHRONOUS_IO_NONALERT,
        },
    },
    Win32::{
        Foundation::{
            RtlNtStatusToDosError, ERROR_IO_PENDING, ERROR_LOCK_VIOLATION, HANDLE,
            OBJ_CASE_INSENSITIVE, UNICODE_STRING,
        },
        Storage::FileSystem::{
            GetFileInformationByHandle, LockFileEx, UnlockFileEx, BY_HANDLE_FILE_INFORMATION,
            FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
            FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_READ, FILE_SHARE_WRITE,
            LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, READ_CONTROL, SYNCHRONIZE,
            WRITE_DAC,
        },
        System::IO::{IO_STATUS_BLOCK, OVERLAPPED},
    },
};

use super::LockKind;
use crate::read_root::OpenedDirectory;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObjectIdentity {
    volume: u64,
    file: u64,
}

#[derive(Debug)]
pub(crate) struct OpenedCoordinator {
    root: File,
    file: File,
    root_identity: ObjectIdentity,
    file_identity: ObjectIdentity,
}

impl OpenedCoordinator {
    pub(crate) fn open(
        root_directory: &OpenedDirectory,
        name: &str,
        initialization_name: &str,
        magic: &[u8],
        create: bool,
    ) -> io::Result<Self> {
        let root = root_directory.try_clone_file()?;
        verify_private_directory_handle(&root)?;
        let root_identity = directory_identity(&root)?;
        let initialization = open_initialization_file(&root, initialization_name, create)?;
        initialization.sync_all()?;
        lock_initialization(&initialization)?;
        let (file, created) = match nt_open_at(&root, OsStr::new(name), FILE_OPEN) {
            Ok(file) => (file, false),
            Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                match nt_open_at(&root, OsStr::new(name), FILE_CREATE) {
                    Ok(file) => (file, true),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        (nt_open_at(&root, OsStr::new(name), FILE_OPEN)?, false)
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        };
        // The versioned filename defines the lock ranges. These bytes are only
        // an initialization marker, never generation or owner authority. Repair
        // the same inode/handle so existing process locks remain effective.
        if create {
            prepare_control_file(&file)?;
        }
        let needs_initialization =
            created || (create && verify_coordinator_file(&file, magic).is_err());
        if needs_initialization {
            #[cfg(test)]
            if let Some(prefix_len) =
                super::super::coordinator_creation_crash_prefix_len(magic.len())
            {
                write_exact_at(&file, &magic[..prefix_len])?;
                file.sync_all()?;
                std::process::exit(super::super::COORDINATOR_CREATION_CRASH_EXIT_CODE);
            }
            write_exact_at(&file, magic)?;
            file.set_len(magic.len() as u64)?;
            file.sync_all()?;
        }
        verify_coordinator_file(&file, magic)?;
        let file_identity = file_identity(&file)?;
        let opened = Self {
            root,
            file,
            root_identity,
            file_identity,
        };
        opened.verify_binding(name, magic)?;
        Ok(opened)
    }

    pub(crate) fn verify_binding(&self, name: &str, magic: &[u8]) -> io::Result<()> {
        if directory_identity(&self.root)? != self.root_identity {
            return Err(invalid_state());
        }
        verify_coordinator_file(&self.file, magic)?;
        if file_identity(&self.file)? != self.file_identity {
            return Err(invalid_state());
        }
        let named = nt_open_at(&self.root, OsStr::new(name), FILE_OPEN)?;
        verify_coordinator_file(&named, magic)?;
        if file_identity(&named)? != self.file_identity {
            return Err(invalid_state());
        }
        Ok(())
    }

    pub(crate) fn try_lock(&self, offset: u64, kind: LockKind) -> io::Result<bool> {
        try_lock_file(&self.file, offset, kind)
    }

    pub(crate) fn unlock(&self, offset: u64) -> io::Result<()> {
        unlock_file(&self.file, offset)
    }
}

fn open_initialization_file(root: &File, name: &str, create: bool) -> io::Result<File> {
    let (file, created) = match nt_open_at(root, OsStr::new(name), FILE_OPEN) {
        Ok(file) => (file, false),
        Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
            match nt_open_at(root, OsStr::new(name), FILE_CREATE) {
                Ok(file) => (file, true),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    (nt_open_at(root, OsStr::new(name), FILE_OPEN)?, false)
                }
                Err(error) => return Err(error),
            }
        }
        Err(error) => return Err(error),
    };
    if created || create {
        prepare_control_file(&file)?;
    }
    verify_initialization_file(&file)?;
    Ok(file)
}

fn try_lock_file(file: &File, offset: u64, kind: LockKind) -> io::Result<bool> {
    let mut overlapped = overlapped_at(offset);
    let flags = LOCKFILE_FAIL_IMMEDIATELY
        | match kind {
            LockKind::Shared => 0,
            LockKind::Exclusive => LOCKFILE_EXCLUSIVE_LOCK,
        };
    if unsafe { LockFileEx(file.as_raw_handle(), flags, 0, 1, 0, &mut overlapped) } != 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if matches!(
        error.raw_os_error(),
        Some(code) if code == ERROR_IO_PENDING as i32 || code == ERROR_LOCK_VIOLATION as i32
    ) {
        Ok(false)
    } else {
        Err(error)
    }
}

fn unlock_file(file: &File, offset: u64) -> io::Result<()> {
    let mut overlapped = overlapped_at(offset);
    if unsafe { UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &mut overlapped) } != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn lock_initialization(file: &File) -> io::Result<()> {
    loop {
        if try_lock_file(file, 0, LockKind::Exclusive)? {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn nt_open_at(parent: &File, name: &OsStr, disposition: u32) -> io::Result<File> {
    let mut wide = name.encode_wide().collect::<Vec<_>>();
    if wide.is_empty()
        || wide
            .iter()
            .any(|unit| *unit == 0 || *unit == b'/' as u16 || *unit == b'\\' as u16)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "lease coordinator name is not one path component",
        ));
    }
    let byte_len = wide
        .len()
        .checked_mul(2)
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "lease name is too long"))?;
    let mut unicode = UNICODE_STRING {
        Length: byte_len,
        MaximumLength: byte_len,
        Buffer: wide.as_mut_ptr(),
    };
    let object_attributes = OBJECT_ATTRIBUTES {
        Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle() as HANDLE,
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
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | READ_CONTROL | WRITE_DAC | SYNCHRONIZE,
            &object_attributes,
            &mut status_block,
            std::ptr::null(),
            FILE_ATTRIBUTE_NORMAL,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            disposition,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null(),
            0,
        )
    };
    if status < 0 {
        return Err(io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(status) } as i32,
        ));
    }
    Ok(unsafe { File::from_raw_handle(handle as RawHandle) })
}

fn verify_coordinator_file(file: &File, magic: &[u8]) -> io::Result<()> {
    verify_private_file_handle(file)?;
    let information = information(file)?;
    if information.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0
        || information.nNumberOfLinks != 1
        || file.metadata()?.len() != magic.len() as u64
    {
        return Err(invalid_state());
    }
    let mut bytes = vec![0_u8; magic.len()];
    read_exact_at(file, &mut bytes)?;
    if bytes != magic {
        return Err(invalid_state());
    }
    Ok(())
}

fn prepare_control_file(file: &File) -> io::Result<()> {
    let information = information(file)?;
    if information.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0
        || information.nNumberOfLinks != 1
    {
        return Err(invalid_state());
    }
    if verify_private_file_handle(file).is_err() {
        restrict_private_file_handle(file)?;
    }
    Ok(())
}

fn verify_initialization_file(file: &File) -> io::Result<()> {
    verify_private_file_handle(file)?;
    let information = information(file)?;
    if information.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) == 0
        && information.nNumberOfLinks == 1
    {
        Ok(())
    } else {
        Err(invalid_state())
    }
}

fn read_exact_at(file: &File, bytes: &mut [u8]) -> io::Result<()> {
    let mut read = 0_usize;
    while read < bytes.len() {
        let count = file.seek_read(&mut bytes[read..], read as u64)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "lease coordinator read",
            ));
        }
        read += count;
    }
    Ok(())
}

fn write_exact_at(file: &File, bytes: &[u8]) -> io::Result<()> {
    let mut written = 0_usize;
    while written < bytes.len() {
        let count = file.seek_write(&bytes[written..], written as u64)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "lease coordinator write",
            ));
        }
        written += count;
    }
    Ok(())
}

fn directory_identity(file: &File) -> io::Result<ObjectIdentity> {
    let information = information(file)?;
    if information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
        || information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(invalid_state());
    }
    Ok(identity_from_information(&information))
}

fn file_identity(file: &File) -> io::Result<ObjectIdentity> {
    let information = information(file)?;
    if information.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0
    {
        return Err(invalid_state());
    }
    Ok(identity_from_information(&information))
}

fn information(file: &File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    if unsafe {
        GetFileInformationByHandle(file.as_raw_handle() as HANDLE, information.as_mut_ptr())
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { information.assume_init() })
}

fn identity_from_information(information: &BY_HANDLE_FILE_INFORMATION) -> ObjectIdentity {
    ObjectIdentity {
        volume: u64::from(information.dwVolumeSerialNumber),
        file: (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
    }
}

fn overlapped_at(offset: u64) -> OVERLAPPED {
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    overlapped.Anonymous.Anonymous.Offset = offset as u32;
    overlapped.Anonymous.Anonymous.OffsetHigh = (offset >> 32) as u32;
    overlapped
}

fn invalid_state() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid generation lease coordinator",
    )
}
