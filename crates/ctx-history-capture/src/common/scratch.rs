use std::{
    ffi::OsStr,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use fs2::FileExt;

use crate::{CaptureError, Result};

const SCRATCH_ROOT_NAME: &str = "ctx-history-capture-scratch-v1";
const MANAGER_LOCK_NAME: &str = ".manager.lock";
const NEXT_RUN_ID_NAME: &str = ".next-run-id";
const SWEEP_STATE_NAME: &str = ".sweep-state";
const LEASE_NAME: &str = "lease";
const OWNER_NAME: &str = "owner";
const MAX_SCAVENGE_RUNS: usize = 32;

pub(crate) struct CaptureScratchSpace {
    root: PathBuf,
    path: PathBuf,
    lease: Option<File>,
}

impl CaptureScratchSpace {
    pub(crate) fn create(kind: &'static str) -> Result<Self> {
        Self::create_at_root(default_scratch_root(), kind)
    }

    #[cfg(test)]
    pub(crate) fn create_in(root: PathBuf, kind: &'static str) -> Result<Self> {
        Self::create_at_root(root, kind)
    }

    fn create_at_root(root: PathBuf, kind: &'static str) -> Result<Self> {
        validate_kind(kind)?;
        ensure_private_directory(&root)?;
        let _manager_lock = acquire_manager_lock(&root)?;
        let run_id = allocate_run_id(&root)?;
        scavenge_abandoned_runs(&root, run_id)?;

        let path = run_path(&root, run_id);
        create_private_directory(&path)?;
        let lease = create_private_file(&path.join(LEASE_NAME))?;
        FileExt::lock_exclusive(&lease)?;
        let mut owner = create_private_file(&path.join(OWNER_NAME))?;
        writeln!(owner, "pid={}", std::process::id())?;
        writeln!(owner, "run_id={run_id}")?;
        writeln!(owner, "kind={kind}")?;
        owner.sync_all()?;

        Ok(Self {
            root,
            path,
            lease: Some(lease),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn create_file(&self, name: &str) -> Result<File> {
        validate_file_name(name)?;
        Ok(create_private_file(&self.path.join(name))?)
    }

    fn cleanup(&mut self) {
        let Ok(_manager_lock) = acquire_manager_lock(&self.root) else {
            return;
        };
        if let Some(lease) = self.lease.take() {
            let _ = FileExt::unlock(&lease);
            drop(lease);
        }
        if validate_scratch_run_directory(&self.path).is_ok() {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

impl Drop for CaptureScratchSpace {
    fn drop(&mut self) {
        self.cleanup();
    }
}

struct ManagerLock {
    file: File,
}

impl Drop for ManagerLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn default_scratch_root() -> PathBuf {
    #[cfg(unix)]
    {
        let uid = unsafe { libc::geteuid() };
        std::env::temp_dir().join(format!("{SCRATCH_ROOT_NAME}-{uid}"))
    }
    #[cfg(not(unix))]
    {
        std::env::temp_dir().join(SCRATCH_ROOT_NAME)
    }
}

fn validate_kind(kind: &str) -> Result<()> {
    if kind.is_empty()
        || !kind
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(CaptureError::SystemInvariant(
            "capture scratch kind must be lowercase ASCII",
        ));
    }
    Ok(())
}

fn validate_file_name(name: &str) -> Result<()> {
    let path = Path::new(name);
    if name.is_empty()
        || path.file_name() != Some(OsStr::new(name))
        || path.components().count() != 1
    {
        return Err(CaptureError::InvalidPayload(
            "capture scratch file name must be one path component".to_owned(),
        ));
    }
    Ok(())
}

fn ensure_private_directory(path: &Path) -> io::Result<()> {
    match create_private_directory(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            validate_private_directory(path)
        }
        Err(error) => Err(error),
    }
}

fn acquire_manager_lock(root: &Path) -> io::Result<ManagerLock> {
    let path = root.join(MANAGER_LOCK_NAME);
    let file = match create_private_file(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            open_private_regular_file(&path)?
        }
        Err(error) => return Err(error),
    };
    FileExt::lock_exclusive(&file)?;
    Ok(ManagerLock { file })
}

fn run_path(root: &Path, run_id: u64) -> PathBuf {
    root.join(format!("run-{run_id:020}"))
}

fn open_or_create_private_control_file(root: &Path, name: &str) -> io::Result<File> {
    let path = root.join(name);
    match create_private_file(&path) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            open_private_regular_file(&path)
        }
        Err(error) => Err(error),
    }
}

fn read_control_file(file: &mut File) -> io::Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut contents = String::new();
    file.take(256).read_to_string(&mut contents)?;
    Ok(contents)
}

fn write_control_file(file: &mut File, contents: &str) -> io::Result<()> {
    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()
}

fn allocate_run_id(root: &Path) -> io::Result<u64> {
    let mut file = open_or_create_private_control_file(root, NEXT_RUN_ID_NAME)?;
    let contents = read_control_file(&mut file)?;
    let next = if contents.is_empty() {
        0
    } else {
        contents.trim().parse::<u64>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "capture scratch next-run ID is corrupt",
            )
        })?
    };
    let following = next
        .checked_add(1)
        .ok_or_else(|| io::Error::other("capture scratch run ID space is exhausted"))?;
    write_control_file(&mut file, &format!("{following}\n"))?;
    Ok(next)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SweepState {
    cursor: u64,
    highwater: u64,
}

fn read_sweep_state(file: &mut File, next_run_id: u64) -> io::Result<SweepState> {
    let contents = read_control_file(file)?;
    if contents.is_empty() {
        return Ok(SweepState {
            cursor: 0,
            highwater: next_run_id,
        });
    }
    let mut lines = contents.lines();
    let cursor = lines
        .next()
        .and_then(|line| line.strip_prefix("cursor="))
        .and_then(|value| value.parse::<u64>().ok());
    let highwater = lines
        .next()
        .and_then(|line| line.strip_prefix("highwater="))
        .and_then(|value| value.parse::<u64>().ok());
    if lines.next().is_some() || cursor.is_none() || highwater.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "capture scratch sweep state is corrupt",
        ));
    }
    let state = SweepState {
        cursor: cursor.unwrap(),
        highwater: highwater.unwrap(),
    };
    if state.cursor > state.highwater || state.highwater > next_run_id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "capture scratch sweep state is outside the allocated run range",
        ));
    }
    Ok(state)
}

fn write_sweep_state(file: &mut File, state: SweepState) -> io::Result<()> {
    write_control_file(
        file,
        &format!("cursor={}\nhighwater={}\n", state.cursor, state.highwater),
    )
}

fn scavenge_abandoned_runs(root: &Path, current_run_id: u64) -> io::Result<()> {
    let next_run_id = current_run_id
        .checked_add(1)
        .ok_or_else(|| io::Error::other("capture scratch run ID space is exhausted"))?;
    let mut state_file = open_or_create_private_control_file(root, SWEEP_STATE_NAME)?;
    let mut state = read_sweep_state(&mut state_file, next_run_id)?;
    if state.cursor == state.highwater {
        state = SweepState {
            cursor: 0,
            highwater: next_run_id,
        };
    }

    let mut inspected = 0usize;
    while inspected < MAX_SCAVENGE_RUNS && state.cursor < state.highwater {
        let run_id = state.cursor;
        state.cursor += 1;
        inspected += 1;
        if run_id == current_run_id {
            continue;
        }
        let path = run_path(root, run_id);
        match validate_scratch_run_directory(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        }
        let lease_path = path.join(LEASE_NAME);
        let lease = match open_private_regular_file(&lease_path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                validate_scratch_run_contents(&path)?;
                fs::remove_dir_all(&path)?;
                continue;
            }
            Err(error) => return Err(error),
        };
        match FileExt::try_lock_exclusive(&lease) {
            Ok(()) => {
                FileExt::unlock(&lease)?;
                drop(lease);
                validate_scratch_run_contents(&path)?;
                fs::remove_dir_all(&path)?;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
    }
    write_sweep_state(&mut state_file, state)
}

fn validate_scratch_run_contents(path: &Path) -> io::Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "capture scratch run contains a link or non-file entry",
            ));
        }
        validate_private_owner(&metadata)?;
    }
    Ok(())
}

fn validate_scratch_run_directory(path: &Path) -> io::Result<()> {
    validate_private_directory(path)
}

fn validate_private_directory(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "capture scratch path is not a private directory",
        ));
    }
    validate_private_owner(&metadata)?;
    validate_private_directory_permissions(&metadata)?;
    #[cfg(windows)]
    {
        let directory = open_existing_directory_no_follow(path)?;
        validate_private_windows_handle(&directory, true)?;
    }
    Ok(())
}

fn open_private_regular_file(path: &Path) -> io::Result<File> {
    let file = open_existing_file_no_follow(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "capture scratch lease is not a regular file",
        ));
    }
    validate_private_owner(&metadata)?;
    validate_private_file_permissions(&metadata)?;
    #[cfg(windows)]
    validate_private_windows_handle(&file, false)?;
    Ok(file)
}

#[cfg(windows)]
fn open_existing_directory_no_follow(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL,
    };

    let mut options = fs::OpenOptions::new();
    options
        .access_mode(FILE_READ_ATTRIBUTES | READ_CONTROL)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    options.open(path)
}

#[cfg(unix)]
fn create_private_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(windows)]
fn create_private_directory(path: &Path) -> io::Result<()> {
    use std::{mem, os::windows::ffi::OsStrExt};
    use windows_sys::Win32::{
        Foundation::LocalFree, Security::SECURITY_ATTRIBUTES, Storage::FileSystem::CreateDirectoryW,
    };

    let descriptor = private_windows_security_descriptor(true)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let path = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let created = unsafe { CreateDirectoryW(path.as_ptr(), &attributes) };
    let error = (created == 0).then(io::Error::last_os_error);
    unsafe {
        LocalFree(descriptor);
    }
    error.map_or(Ok(()), Err)
}

#[cfg(not(any(unix, windows)))]
fn create_private_directory(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private capture scratch is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(windows)]
fn create_private_file(path: &Path) -> io::Result<File> {
    use std::{mem, os::windows::ffi::OsStrExt, os::windows::io::FromRawHandle, ptr};
    use windows_sys::Win32::{
        Foundation::{LocalFree, INVALID_HANDLE_VALUE},
        Security::SECURITY_ATTRIBUTES,
        Storage::FileSystem::{
            CreateFileW, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE,
        },
    };

    let descriptor = private_windows_security_descriptor(false)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let path = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            FILE_GENERIC_READ | FILE_GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            &attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    let error = (handle == INVALID_HANDLE_VALUE).then(io::Error::last_os_error);
    unsafe {
        LocalFree(descriptor);
    }
    if let Some(error) = error {
        return Err(error);
    }
    Ok(unsafe { File::from_raw_handle(handle) })
}

#[cfg(not(any(unix, windows)))]
fn create_private_file(_path: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private capture scratch is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn open_existing_file_no_follow(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(windows)]
fn open_existing_file_no_follow(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_existing_file_no_follow(_path: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private capture scratch is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn validate_private_owner(metadata: &fs::Metadata) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "capture scratch is not owned by the effective user",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_owner(_metadata: &fs::Metadata) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn validate_private_directory_permissions(metadata: &fs::Metadata) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    if metadata.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "capture scratch directory permissions are not private",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn validate_private_directory_permissions(metadata: &fs::Metadata) -> io::Result<()> {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "capture scratch directory is a reparse point",
        ));
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn validate_private_directory_permissions(_metadata: &fs::Metadata) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private capture scratch is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn validate_private_file_permissions(metadata: &fs::Metadata) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    if metadata.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "capture scratch file permissions are not private",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn validate_private_file_permissions(metadata: &fs::Metadata) -> io::Result<()> {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "capture scratch file is a reparse point",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn validate_private_windows_handle(file: &File, directory: bool) -> io::Result<()> {
    use std::{
        mem::MaybeUninit,
        os::windows::{fs::MetadataExt, io::AsRawHandle},
    };
    use windows_sys::Win32::{
        Foundation::{CloseHandle, LocalFree, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, HANDLE},
        Security::{
            AclSizeInformation,
            Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
            EqualSid, GetAclInformation, GetSecurityDescriptorControl, GetSecurityDescriptorDacl,
            GetTokenInformation, TokenUser, ACL, ACL_SIZE_INFORMATION, DACL_SECURITY_INFORMATION,
            OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED, TOKEN_QUERY,
            TOKEN_USER,
        },
        Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT,
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    let metadata = file.metadata()?;
    let expected_type = if directory {
        metadata.is_dir()
    } else {
        metadata.is_file()
    };
    if !expected_type || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "capture scratch handle has an unsafe type",
        ));
    }

    let mut owner: PSID = std::ptr::null_mut();
    let mut actual_dacl: *mut ACL = std::ptr::null_mut();
    let mut actual_descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle() as HANDLE,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            &mut actual_dacl,
            std::ptr::null_mut(),
            &mut actual_descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }

    let expected_descriptor = match private_windows_security_descriptor(directory) {
        Ok(descriptor) => descriptor,
        Err(error) => {
            unsafe {
                LocalFree(actual_descriptor);
            }
            return Err(error);
        }
    };
    let comparison = (|| {
        if owner.is_null() || actual_dacl.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "capture scratch has no owner or DACL",
            ));
        }

        let mut token: HANDLE = std::ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let owner_matches = (|| {
            let mut required = 0_u32;
            let first = unsafe {
                GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut required)
            };
            if first != 0
                || io::Error::last_os_error().raw_os_error()
                    != Some(ERROR_INSUFFICIENT_BUFFER as i32)
                || required == 0
            {
                return Err(io::Error::last_os_error());
            }
            let word_size = std::mem::size_of::<usize>();
            let word_count = (required as usize).div_ceil(word_size);
            let mut buffer = vec![0_usize; word_count];
            if unsafe {
                GetTokenInformation(
                    token,
                    TokenUser,
                    buffer.as_mut_ptr().cast(),
                    required,
                    &mut required,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            let token_user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
            Ok(unsafe { EqualSid(owner, token_user.User.Sid) } != 0)
        })();
        unsafe {
            CloseHandle(token);
        }
        if !owner_matches? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "capture scratch is not owned by the current user",
            ));
        }

        let mut control = 0_u16;
        let mut revision = 0_u32;
        if unsafe { GetSecurityDescriptorControl(actual_descriptor, &mut control, &mut revision) }
            == 0
            || control & SE_DACL_PROTECTED == 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "capture scratch DACL is not protected",
            ));
        }

        let mut expected_present = 0;
        let mut expected_defaulted = 0;
        let mut expected_dacl: *mut ACL = std::ptr::null_mut();
        if unsafe {
            GetSecurityDescriptorDacl(
                expected_descriptor,
                &mut expected_present,
                &mut expected_dacl,
                &mut expected_defaulted,
            )
        } == 0
            || expected_present == 0
            || expected_dacl.is_null()
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "capture scratch expected DACL is invalid",
            ));
        }

        fn acl_bytes(acl: *const ACL) -> io::Result<Vec<u8>> {
            let mut info = MaybeUninit::<ACL_SIZE_INFORMATION>::zeroed();
            if unsafe {
                GetAclInformation(
                    acl,
                    info.as_mut_ptr().cast(),
                    std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                    AclSizeInformation,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            let info = unsafe { info.assume_init() };
            Ok(unsafe {
                std::slice::from_raw_parts(acl.cast::<u8>(), info.AclBytesInUse as usize).to_vec()
            })
        }

        if acl_bytes(actual_dacl)? != acl_bytes(expected_dacl)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "capture scratch DACL is not owner/System-only",
            ));
        }
        Ok(())
    })();
    unsafe {
        LocalFree(expected_descriptor);
        LocalFree(actual_descriptor);
    }
    comparison
}

#[cfg(not(any(unix, windows)))]
fn validate_private_file_permissions(_metadata: &fs::Metadata) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private capture scratch is unsupported on this platform",
    ))
}

#[cfg(windows)]
fn private_windows_security_descriptor(
    directory: bool,
) -> io::Result<windows_sys::Win32::Security::PSECURITY_DESCRIPTOR> {
    use std::ptr;
    use windows_sys::Win32::Security::{
        Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW, PSECURITY_DESCRIPTOR,
    };

    let sddl = if directory {
        "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)"
    } else {
        "D:P(A;;FA;;;OW)(A;;FA;;;SY)"
    };
    let sddl = sddl.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            ptr::null_mut(),
        )
    };
    if converted == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(descriptor)
}

#[cfg(test)]
include!("scratch/tests.rs");
