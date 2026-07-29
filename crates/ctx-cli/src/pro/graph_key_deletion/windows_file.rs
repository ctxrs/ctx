//! Delete-only routing for the private helper's current-user Windows key store.

#![allow(unsafe_code)]

use std::{
    ffi::c_void,
    fs::{self, File},
    io::{self, Read as _},
    mem::size_of,
    os::windows::{
        fs::{MetadataExt as _, OpenOptionsExt as _},
        io::AsRawHandle as _,
    },
    path::{Component, Path, PathBuf},
    ptr::{addr_of, null_mut},
};

use fs2::FileExt as _;
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, GetLastError, LocalFree, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS,
        GENERIC_READ, HANDLE,
    },
    Security::{
        Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
        EqualSid, GetAce, GetLengthSid, GetSecurityDescriptorControl, GetTokenInformation,
        IsValidSid, TokenUser, ACCESS_ALLOWED_ACE, ACL, DACL_SECURITY_INFORMATION, PSID,
        SE_DACL_PROTECTED, TOKEN_QUERY, TOKEN_USER,
    },
    Storage::FileSystem::{
        FileDispositionInfo, GetFileInformationByHandle, SetFileInformationByHandle,
        BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_INFO,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

use crate::pro::credential_vault::CredentialVaultError;

const STORE_DIRECTORY: &str = ".ctx-pro-key-store-v1";
const RECORDS_DIRECTORY: &str = "records";
const BACKEND_MARKER: &str = "backend";
const LOCK_FILE: &str = "lock";
const FILE_SELECTION: &[u8; 12] = b"CTXKSB01FILE";
const RECORD_BYTES: usize = 8 + 32 + 32;
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const PRIVATE_ACCESS_MASK: u32 = 0x001f_01ff;
const PRIVATE_DIRECTORY_INHERITANCE: u8 = 0x03;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BackendSelection {
    File,
    Native,
}

pub(super) fn delete(
    pro_root: &Path,
    account: &str,
    native_selection: &'static [u8; 12],
    native_delete: impl FnOnce() -> Result<(), CredentialVaultError>,
) -> Result<(), CredentialVaultError> {
    let Some(store) = Store::open_existing(pro_root, native_selection)? else {
        return native_delete();
    };
    let initial_selection = store.read_selection()?;
    if initial_selection.is_none() {
        store.validate_unselected_file_state()?;
    }
    let Some(lock) = store.open_existing_lock()? else {
        return if initial_selection.is_some() {
            Err(CredentialVaultError::Corrupt)
        } else {
            native_delete()
        };
    };
    lock.lock_exclusive()
        .map_err(|_| CredentialVaultError::Backend)?;
    let result = (|| {
        store.validate_layout()?;
        validate_named_file(&store.lock_path(), &lock, None)?;
        match store.read_selection()? {
            Some(BackendSelection::File) => store.delete_record(account),
            Some(BackendSelection::Native) => native_delete(),
            None => {
                store.validate_unselected_file_state()?;
                native_delete()
            }
        }
    })();
    let final_validation = store
        .validate_layout()
        .and_then(|()| validate_named_file(&store.lock_path(), &lock, None));
    let unlock = fs2::FileExt::unlock(&lock).map_err(|_| CredentialVaultError::Backend);
    match (result, final_validation, unlock) {
        (Ok(()), Ok(()), Ok(())) => Ok(()),
        (Err(error), _, _) | (Ok(()), Err(error), _) | (Ok(()), Ok(()), Err(error)) => Err(error),
    }
}

struct Store {
    pro_root_path: PathBuf,
    pro_root: File,
    root_path: PathBuf,
    root: File,
    records_path: PathBuf,
    records: File,
    native_selection: &'static [u8; 12],
}

impl Store {
    fn open_existing(
        pro_root: &Path,
        native_selection: &'static [u8; 12],
    ) -> Result<Option<Self>, CredentialVaultError> {
        if !path_exists(pro_root)? {
            return Ok(None);
        }
        validate_data_root(pro_root)?;
        let pro_root_handle =
            open_directory(pro_root).map_err(|_| CredentialVaultError::InvalidDataRoot)?;
        validate_directory_handle(&pro_root_handle, false)?;
        validate_named_directory(pro_root, &pro_root_handle, false)?;

        let root_path = pro_root.join(STORE_DIRECTORY);
        let Some(root) = open_existing_private_directory(&root_path)? else {
            return Ok(None);
        };
        let records_path = root_path.join(RECORDS_DIRECTORY);
        let Some(records) = open_existing_private_directory(&records_path)? else {
            return if directory_has_unexpected_entries(&root_path, &[])? {
                Err(CredentialVaultError::Corrupt)
            } else {
                Ok(None)
            };
        };
        let store = Self {
            pro_root_path: pro_root.to_path_buf(),
            pro_root: pro_root_handle,
            root_path,
            root,
            records_path,
            records,
            native_selection,
        };
        store.validate_layout()?;
        Ok(Some(store))
    }

    fn validate_layout(&self) -> Result<(), CredentialVaultError> {
        validate_directory_handle(&self.pro_root, false)?;
        validate_named_directory(&self.pro_root_path, &self.pro_root, false)?;
        validate_named_directory(&self.root_path, &self.root, true)?;
        validate_named_directory(&self.records_path, &self.records, true)
    }

    fn lock_path(&self) -> PathBuf {
        self.root_path.join(LOCK_FILE)
    }

    fn open_existing_lock(&self) -> Result<Option<File>, CredentialVaultError> {
        open_existing_private_file(&self.lock_path(), None, false)
    }

    fn validate_unselected_file_state(&self) -> Result<(), CredentialVaultError> {
        self.validate_layout()?;
        if directory_has_unexpected_entries(&self.records_path, &[])?
            || directory_has_unexpected_entries(&self.root_path, &[RECORDS_DIRECTORY, LOCK_FILE])?
        {
            Err(CredentialVaultError::Corrupt)
        } else {
            Ok(())
        }
    }

    fn read_selection(&self) -> Result<Option<BackendSelection>, CredentialVaultError> {
        let path = self.root_path.join(BACKEND_MARKER);
        let Some(mut marker) = open_existing_private_file(&path, Some(12), false)? else {
            return Ok(None);
        };
        let mut bytes = [0_u8; 12];
        marker
            .read_exact(&mut bytes)
            .map_err(|_| CredentialVaultError::Corrupt)?;
        let mut extra = [0_u8; 1];
        if marker
            .read(&mut extra)
            .map_err(|_| CredentialVaultError::Corrupt)?
            != 0
        {
            return Err(CredentialVaultError::Corrupt);
        }
        validate_named_file(&path, &marker, Some(12))?;
        if bytes == *FILE_SELECTION {
            Ok(Some(BackendSelection::File))
        } else if bytes == *self.native_selection {
            Ok(Some(BackendSelection::Native))
        } else {
            Err(CredentialVaultError::Corrupt)
        }
    }

    fn delete_record(&self, account: &str) -> Result<(), CredentialVaultError> {
        let path = self.records_path.join(record_name(account)?);
        let Some(record) = open_existing_private_file(&path, Some(RECORD_BYTES), true)? else {
            return Err(CredentialVaultError::NotFound);
        };
        validate_named_file(&path, &record, Some(RECORD_BYTES))?;
        delete_open_file(&record)?;
        drop(record);
        if path_exists(&path)? {
            Err(CredentialVaultError::Backend)
        } else {
            Ok(())
        }
    }
}

fn directory_has_unexpected_entries(
    directory: &Path,
    allowed: &[&str],
) -> Result<bool, CredentialVaultError> {
    for entry in fs::read_dir(directory).map_err(|_| CredentialVaultError::Corrupt)? {
        let entry = entry.map_err(|_| CredentialVaultError::Corrupt)?;
        if allowed
            .iter()
            .any(|allowed_name| entry.file_name() == std::ffi::OsStr::new(allowed_name))
        {
            continue;
        }
        return Ok(true);
    }
    Ok(false)
}

fn record_name(account: &str) -> Result<String, CredentialVaultError> {
    let suffix = account
        .strip_prefix("nvr1-g-")
        .ok_or(CredentialVaultError::Corrupt)?;
    if suffix.len() != 64
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(CredentialVaultError::Corrupt);
    }
    Ok(format!("{account}.record"))
}

fn validate_data_root(path: &Path) -> Result<(), CredentialVaultError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(CredentialVaultError::InvalidDataRoot);
    }
    ctx_history_core::platform_security::verify_private_directory(path)
        .map_err(|_| CredentialVaultError::InvalidDataRoot)
}

fn open_existing_private_directory(path: &Path) -> Result<Option<File>, CredentialVaultError> {
    let directory = match open_directory(path) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            return Err(CredentialVaultError::Locked)
        }
        Err(_) => return Err(CredentialVaultError::Corrupt),
    };
    verify_private_handle(&directory, ObjectKind::Directory)?;
    validate_named_directory(path, &directory, true)?;
    Ok(Some(directory))
}

fn open_directory(path: &Path) -> io::Result<File> {
    fs::OpenOptions::new()
        .access_mode(READ_CONTROL)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

fn open_existing_private_file(
    path: &Path,
    expected_size: Option<usize>,
    delete_access: bool,
) -> Result<Option<File>, CredentialVaultError> {
    let access = GENERIC_READ | READ_CONTROL | if delete_access { DELETE } else { 0 };
    let file = match fs::OpenOptions::new()
        .access_mode(access)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            return Err(CredentialVaultError::Locked)
        }
        Err(_) => return Err(CredentialVaultError::Corrupt),
    };
    validate_named_file(path, &file, expected_size)?;
    Ok(Some(file))
}

fn validate_named_directory(
    path: &Path,
    opened: &File,
    require_private: bool,
) -> Result<(), CredentialVaultError> {
    validate_directory_handle(opened, require_private)?;
    let named = open_directory(path).map_err(|_| CredentialVaultError::Corrupt)?;
    validate_directory_handle(&named, require_private)?;
    same_file(opened, &named)
}

fn validate_named_file(
    path: &Path,
    opened: &File,
    expected_size: Option<usize>,
) -> Result<(), CredentialVaultError> {
    validate_file_handle(opened, expected_size)?;
    let named = fs::OpenOptions::new()
        .access_mode(GENERIC_READ | READ_CONTROL)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|_| CredentialVaultError::Corrupt)?;
    validate_file_handle(&named, expected_size)?;
    same_file(opened, &named)
}

fn validate_directory_handle(
    directory: &File,
    require_private: bool,
) -> Result<(), CredentialVaultError> {
    let metadata = directory
        .metadata()
        .map_err(|_| CredentialVaultError::Corrupt)?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(CredentialVaultError::Corrupt);
    }
    if require_private {
        verify_private_handle(directory, ObjectKind::Directory)?;
    }
    Ok(())
}

fn validate_file_handle(
    file: &File,
    expected_size: Option<usize>,
) -> Result<(), CredentialVaultError> {
    let metadata = file.metadata().map_err(|_| CredentialVaultError::Corrupt)?;
    let info = file_information(file)?;
    if !metadata.is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || info.nNumberOfLinks != 1
        || expected_size.is_some_and(|expected| metadata.file_size() != expected as u64)
    {
        return Err(CredentialVaultError::Corrupt);
    }
    verify_private_handle(file, ObjectKind::File)
}

fn same_file(left: &File, right: &File) -> Result<(), CredentialVaultError> {
    if file_identity(left)? == file_identity(right)? {
        Ok(())
    } else {
        Err(CredentialVaultError::Corrupt)
    }
}

fn path_exists(path: &Path) -> Result<bool, CredentialVaultError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 => Ok(true),
        Ok(_) => Err(CredentialVaultError::Corrupt),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            Err(CredentialVaultError::Locked)
        }
        Err(_) => Err(CredentialVaultError::Backend),
    }
}

fn delete_open_file(file: &File) -> Result<(), CredentialVaultError> {
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle().cast(),
            FileDispositionInfo,
            (&raw const disposition).cast(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    } == 0
    {
        Err(CredentialVaultError::Backend)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum ObjectKind {
    Directory,
    File,
}

impl ObjectKind {
    const fn inheritance_flags(self) -> u8 {
        match self {
            Self::Directory => PRIVATE_DIRECTORY_INHERITANCE,
            Self::File => 0,
        }
    }
}

fn verify_private_handle(handle: &File, kind: ObjectKind) -> Result<(), CredentialVaultError> {
    validate_object_type(handle, kind)?;
    let identity = CurrentUserIdentity::load()?;
    verify_private_handle_with_identity(handle, kind, identity.sid())
}

fn validate_object_type(handle: &File, kind: ObjectKind) -> Result<(), CredentialVaultError> {
    let metadata = handle
        .metadata()
        .map_err(|_| CredentialVaultError::Corrupt)?;
    let expected = match kind {
        ObjectKind::Directory => metadata.is_dir(),
        ObjectKind::File => metadata.is_file(),
    };
    if expected && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
        Ok(())
    } else {
        Err(CredentialVaultError::Corrupt)
    }
}

fn verify_private_handle_with_identity(
    handle: &File,
    kind: ObjectKind,
    current_user: PSID,
) -> Result<(), CredentialVaultError> {
    let mut dacl: *mut ACL = null_mut();
    let mut descriptor = null_mut();
    let result = unsafe {
        GetSecurityInfo(
            handle.as_raw_handle().cast(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &raw mut dacl,
            null_mut(),
            &raw mut descriptor,
        )
    };
    if result != ERROR_SUCCESS {
        return Err(CredentialVaultError::Locked);
    }
    let _descriptor = LocalAllocation(descriptor);
    if dacl.is_null() {
        return Err(CredentialVaultError::Corrupt);
    }
    let mut control = 0;
    let mut revision = 0;
    if unsafe { GetSecurityDescriptorControl(descriptor, &raw mut control, &raw mut revision) } == 0
        || control & SE_DACL_PROTECTED == 0
        || unsafe { (*dacl).AceCount } != 1
    {
        return Err(CredentialVaultError::Corrupt);
    }
    let mut raw_ace: *mut c_void = null_mut();
    if unsafe { GetAce(dacl, 0, &raw mut raw_ace) } == 0 || raw_ace.is_null() {
        return Err(CredentialVaultError::Corrupt);
    }
    let ace = unsafe { &*raw_ace.cast::<ACCESS_ALLOWED_ACE>() };
    let sid = addr_of!(ace.SidStart).cast_mut().cast::<c_void>();
    if ace.Header.AceType == ACCESS_ALLOWED_ACE_TYPE
        && ace.Header.AceFlags == kind.inheritance_flags()
        && ace.Mask == PRIVATE_ACCESS_MASK
        && unsafe { EqualSid(sid, current_user) } != 0
    {
        Ok(())
    } else {
        Err(CredentialVaultError::Corrupt)
    }
}

fn sid_size(sid: PSID) -> Result<usize, CredentialVaultError> {
    if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
        return Err(CredentialVaultError::Backend);
    }
    usize::try_from(unsafe { GetLengthSid(sid) }).map_err(|_| CredentialVaultError::Backend)
}

struct CurrentUserIdentity {
    _token: Handle,
    token_user: AlignedBuffer,
}

impl CurrentUserIdentity {
    fn load() -> Result<Self, CredentialVaultError> {
        let mut token = null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
            return Err(CredentialVaultError::Locked);
        }
        let token = Handle(token);
        let mut required = 0;
        let first =
            unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &raw mut required) };
        if first != 0 || unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER || required == 0 {
            return Err(CredentialVaultError::Backend);
        }
        let mut token_user = AlignedBuffer::new(
            usize::try_from(required).map_err(|_| CredentialVaultError::Backend)?,
        )?;
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                token_user.as_mut_ptr().cast(),
                required,
                &raw mut required,
            )
        } == 0
        {
            return Err(CredentialVaultError::Backend);
        }
        let identity = Self {
            _token: token,
            token_user,
        };
        let _ = sid_size(identity.sid())?;
        Ok(identity)
    }

    fn sid(&self) -> PSID {
        unsafe { (*self.token_user.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct FileIdentity {
    volume: u32,
    index_high: u32,
    index_low: u32,
}

fn file_identity(file: &File) -> Result<FileIdentity, CredentialVaultError> {
    let info = file_information(file)?;
    Ok(FileIdentity {
        volume: info.dwVolumeSerialNumber,
        index_high: info.nFileIndexHigh,
        index_low: info.nFileIndexLow,
    })
}

fn file_information(file: &File) -> Result<BY_HANDLE_FILE_INFORMATION, CredentialVaultError> {
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &raw mut info) } == 0 {
        Err(CredentialVaultError::Corrupt)
    } else {
        Ok(info)
    }
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
}

struct AlignedBuffer {
    words: Vec<usize>,
}

impl AlignedBuffer {
    fn new(byte_len: usize) -> Result<Self, CredentialVaultError> {
        let word = size_of::<usize>();
        let words = byte_len
            .checked_add(word - 1)
            .ok_or(CredentialVaultError::Backend)?
            / word;
        Ok(Self {
            words: vec![0; words],
        })
    }

    fn as_ptr(&self) -> *const usize {
        self.words.as_ptr()
    }

    fn as_mut_ptr(&mut self) -> *mut usize {
        self.words.as_mut_ptr()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        os::windows::fs::OpenOptionsExt as _,
        process::Command,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Barrier,
        },
        thread,
    };

    use windows_sys::Win32::{
        Foundation::GENERIC_WRITE,
        Security::{
            AddAccessAllowedAceEx, Authorization::SetSecurityInfo, InitializeAcl, ACL_REVISION,
            PROTECTED_DACL_SECURITY_INFORMATION,
        },
        Storage::FileSystem::WRITE_DAC,
    };

    use super::*;

    const NATIVE: &[u8; 12] = b"CTXKSB01WCRM";

    fn restrict_private_handle(
        handle: &File,
        kind: ObjectKind,
    ) -> Result<(), CredentialVaultError> {
        let identity = CurrentUserIdentity::load()?;
        let sid_bytes = sid_size(identity.sid())?;
        let ace_header = size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>();
        let bytes = size_of::<ACL>()
            .checked_add(ace_header)
            .and_then(|value| value.checked_add(sid_bytes))
            .ok_or(CredentialVaultError::Backend)?;
        let mut acl = AlignedBuffer::new(bytes)?;
        if unsafe { InitializeAcl(acl.as_mut_ptr().cast(), bytes as u32, ACL_REVISION) } == 0
            || unsafe {
                AddAccessAllowedAceEx(
                    acl.as_mut_ptr().cast(),
                    ACL_REVISION,
                    kind.inheritance_flags().into(),
                    PRIVATE_ACCESS_MASK,
                    identity.sid(),
                )
            } == 0
        {
            return Err(CredentialVaultError::Backend);
        }
        let result = unsafe {
            SetSecurityInfo(
                handle.as_raw_handle().cast(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                acl.as_mut_ptr().cast(),
                null_mut(),
            )
        };
        if result != ERROR_SUCCESS {
            return Err(CredentialVaultError::Locked);
        }
        verify_private_handle(handle, kind)
    }

    fn private_directory(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        fs::create_dir(path)?;
        let handle = fs::OpenOptions::new()
            .access_mode(READ_CONTROL | WRITE_DAC)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        restrict_private_handle(&handle, ObjectKind::Directory)?;
        Ok(())
    }

    fn private_file(path: &Path, bytes: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .access_mode(GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .create_new(true)
            .open(path)?;
        std::io::Write::write_all(&mut file, bytes)?;
        restrict_private_handle(&file, ObjectKind::File)?;
        Ok(())
    }

    fn file_layout() -> Result<(tempfile::TempDir, PathBuf, String), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        ctx_history_core::platform_security::restrict_private_directory(root.path())?;
        let pro = root.path().join("pro");
        fs::create_dir(&pro)?;
        ctx_history_core::platform_security::restrict_private_directory(&pro)?;
        let store = pro.join(STORE_DIRECTORY);
        private_directory(&store)?;
        let records = store.join(RECORDS_DIRECTORY);
        private_directory(&records)?;
        private_file(&store.join(BACKEND_MARKER), FILE_SELECTION)?;
        private_file(&store.join(LOCK_FILE), b"")?;
        let account =
            "nvr1-g-12c2fbc8efe95366e7da4511ebe8b5c7e17a38321f4d92831d3a520ee5c7dc07".to_owned();
        private_file(
            &records.join(format!("{account}.record")),
            &[7_u8; RECORD_BYTES],
        )?;
        Ok((root, pro, account))
    }

    #[test]
    fn file_selection_deletes_by_handle_and_verifies_absence(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_root, pro, account) = file_layout()?;
        let native_calls = AtomicUsize::new(0);
        delete(&pro, &account, NATIVE, || {
            native_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })?;
        assert_eq!(native_calls.load(Ordering::SeqCst), 0);
        assert!(!pro
            .join(STORE_DIRECTORY)
            .join(RECORDS_DIRECTORY)
            .join(format!("{account}.record"))
            .exists());
        Ok(())
    }

    #[test]
    fn pristine_inspection_and_native_selection_never_create_or_downgrade(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let missing = root.path().join("missing");
        assert!(matches!(
            delete(
                &missing,
                "nvr1-g-0000000000000000000000000000000000000000000000000000000000000000",
                NATIVE,
                || Err(CredentialVaultError::NotFound)
            ),
            Err(CredentialVaultError::NotFound)
        ));
        assert!(!missing.exists());

        let (_root, pro, account) = file_layout()?;
        delete_open_file(
            &open_existing_private_file(
                &pro.join(STORE_DIRECTORY).join(BACKEND_MARKER),
                Some(12),
                true,
            )?
            .unwrap(),
        )?;
        private_file(&pro.join(STORE_DIRECTORY).join(BACKEND_MARKER), NATIVE)?;
        assert!(matches!(
            delete(&pro, &account, NATIVE, || {
                Err(CredentialVaultError::Unavailable {
                    platform: "windows",
                })
            }),
            Err(CredentialVaultError::Unavailable {
                platform: "windows"
            })
        ));
        assert!(pro
            .join(STORE_DIRECTORY)
            .join(RECORDS_DIRECTORY)
            .join(format!("{account}.record"))
            .exists());
        Ok(())
    }

    #[test]
    fn markerless_file_records_and_selector_temps_fail_closed(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_root, pro, account) = file_layout()?;
        let store = pro.join(STORE_DIRECTORY);
        delete_open_file(
            &open_existing_private_file(&store.join(BACKEND_MARKER), Some(12), true)?.unwrap(),
        )?;
        let native_calls = AtomicUsize::new(0);
        assert!(matches!(
            delete(&pro, &account, NATIVE, || {
                native_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
            Err(CredentialVaultError::Corrupt)
        ));
        assert_eq!(native_calls.load(Ordering::SeqCst), 0);

        let (_root, pro, account) = file_layout()?;
        let store = pro.join(STORE_DIRECTORY);
        delete_open_file(
            &open_existing_private_file(&store.join(BACKEND_MARKER), Some(12), true)?.unwrap(),
        )?;
        let record = store
            .join(RECORDS_DIRECTORY)
            .join(format!("{account}.record"));
        delete_open_file(&open_existing_private_file(&record, Some(RECORD_BYTES), true)?.unwrap())?;
        private_file(&store.join(".tmp-interrupted-selection"), b"orphan")?;
        assert!(matches!(
            delete(&pro, &account, NATIVE, || Ok(())),
            Err(CredentialVaultError::Corrupt)
        ));
        Ok(())
    }

    #[test]
    fn partial_store_root_with_unexpected_state_fails_closed(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        ctx_history_core::platform_security::restrict_private_directory(root.path())?;
        let pro = root.path().join("pro");
        fs::create_dir(&pro)?;
        ctx_history_core::platform_security::restrict_private_directory(&pro)?;
        let store = pro.join(STORE_DIRECTORY);
        private_directory(&store)?;
        private_file(&store.join(".tmp-interrupted"), b"orphan")?;
        let native_calls = AtomicUsize::new(0);

        assert!(matches!(
            delete(
                &pro,
                "nvr1-g-0000000000000000000000000000000000000000000000000000000000000000",
                NATIVE,
                || {
                    native_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            ),
            Err(CredentialVaultError::Corrupt)
        ));
        assert_eq!(native_calls.load(Ordering::SeqCst), 0);
        Ok(())
    }

    #[test]
    fn unsafe_acl_hardlink_reparse_and_size_fail_closed() -> Result<(), Box<dyn std::error::Error>>
    {
        let (_root, pro, account) = file_layout()?;
        let record = pro
            .join(STORE_DIRECTORY)
            .join(RECORDS_DIRECTORY)
            .join(format!("{account}.record"));
        let hardlink = pro.join("record-hardlink");
        fs::hard_link(&record, &hardlink)?;
        assert!(matches!(
            delete(&pro, &account, NATIVE, || Ok(())),
            Err(CredentialVaultError::Corrupt)
        ));
        fs::remove_file(&hardlink)?;
        ctx_history_core::platform_security::restrict_private_file(&record)?;
        assert!(matches!(
            delete(&pro, &account, NATIVE, || Ok(())),
            Err(CredentialVaultError::Corrupt)
        ));

        let (_root, pro, account) = file_layout()?;
        let record = pro
            .join(STORE_DIRECTORY)
            .join(RECORDS_DIRECTORY)
            .join(format!("{account}.record"));
        delete_open_file(&open_existing_private_file(&record, Some(RECORD_BYTES), true)?.unwrap())?;
        private_file(&record, &[0_u8; RECORD_BYTES + 1])?;
        assert!(matches!(
            delete(&pro, &account, NATIVE, || Ok(())),
            Err(CredentialVaultError::Corrupt)
        ));

        let (_root, pro, account) = file_layout()?;
        let records = pro.join(STORE_DIRECTORY).join(RECORDS_DIRECTORY);
        let displaced = pro.join("displaced-records");
        fs::rename(&records, &displaced)?;
        let status = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&records)
            .arg(&displaced)
            .status()?;
        assert!(status.success(), "failed to create test junction");
        assert!(matches!(
            delete(&pro, &account, NATIVE, || Ok(())),
            Err(CredentialVaultError::Corrupt)
        ));
        fs::remove_dir(&records)?;
        fs::rename(displaced, records)?;
        Ok(())
    }

    #[test]
    fn concurrent_deletions_are_serialized() -> Result<(), Box<dyn std::error::Error>> {
        let (_root, pro, account) = file_layout()?;
        let pro = Arc::new(pro);
        let account = Arc::new(account);
        let barrier = Arc::new(Barrier::new(3));
        let workers: Vec<_> = (0..2)
            .map(|_| {
                let pro = Arc::clone(&pro);
                let account = Arc::clone(&account);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    delete(&pro, &account, NATIVE, || Ok(()))
                })
            })
            .collect();
        barrier.wait();
        let results: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(CredentialVaultError::NotFound)))
                .count(),
            1
        );
        Ok(())
    }
}
