//! Current-user-only Windows file fallback for public credential records.

#![allow(unsafe_code)]

use std::{
    ffi::{c_void, OsStr},
    fs::{self, File},
    io::{self, Read as _, Write as _},
    mem::size_of,
    os::windows::{
        ffi::OsStrExt as _,
        fs::{MetadataExt as _, OpenOptionsExt as _},
        io::AsRawHandle as _,
    },
    path::{Component, Path, PathBuf},
    ptr::{addr_of, null_mut},
};

use windows_sys::Win32::{
    Foundation::{
        CloseHandle, GetLastError, LocalFree, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS,
        GENERIC_READ, GENERIC_WRITE, HANDLE,
    },
    Security::{
        AddAccessAllowedAceEx,
        Authorization::{GetSecurityInfo, SetSecurityInfo, SE_FILE_OBJECT},
        EqualSid, GetAce, GetLengthSid, GetSecurityDescriptorControl, GetTokenInformation,
        InitializeAcl, IsValidSid, TokenUser, ACCESS_ALLOWED_ACE, ACL, ACL_REVISION,
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSID, SE_DACL_PROTECTED,
        TOKEN_QUERY, TOKEN_USER,
    },
    Storage::FileSystem::{
        FileDispositionInfo, GetFileInformationByHandle, MoveFileExW, SetFileInformationByHandle,
        BY_HANDLE_FILE_INFORMATION, DELETE, FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, READ_CONTROL, WRITE_DAC,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};
use zeroize::{Zeroize as _, Zeroizing};

use super::{validate_record_id, CredentialVaultError, SecretBytes, MAX_STORED_SECRET_BYTES};

const BACKEND_MARKER: &str = ".ctx-pro.credential-backend-v1";
const BACKEND_MARKER_STAGE: &str = ".ctx-pro.credential-backend-v1.next";
const FILE_VAULT_DIRECTORY: &str = ".ctx-pro.credentials-v1";
const FILE_VAULT_LOCK: &str = ".ctx-pro.credentials-v1.lock";
const FILE_RECORD_STAGE_SUFFIX: &str = ".next";
pub(super) const FILE_SELECTION: &[u8] = b"ctx-pro-credential-backend-v1:file\n";
const MAX_BACKEND_MARKER_BYTES: usize = 64;
const PRIVATE_GRAPH_STORE_DIRECTORY: &str = ".ctx-pro-key-store-v1";
const PRO_GRAPH_FILES: [&str; 3] = ["ctx-pro.db", "ctx-pro.db.next", "ctx-pro.db.previous"];
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const PRIVATE_ACCESS_MASK: u32 = 0x001f_01ff;
const PRIVATE_DIRECTORY_INHERITANCE: u8 = 0x03;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BackendSelection {
    File,
    Native,
}

pub(super) struct VaultRoot {
    data_root_path: PathBuf,
    data_root: File,
    pro_path: PathBuf,
    pro: File,
    native_selection: &'static [u8],
}

impl VaultRoot {
    pub(super) fn open(
        data_root: &Path,
        native_selection: &'static [u8],
    ) -> Result<Self, CredentialVaultError> {
        validate_data_root(data_root)?;
        let data_root_handle =
            open_directory(data_root, false).map_err(|_| CredentialVaultError::InvalidDataRoot)?;
        validate_directory_handle(&data_root_handle, false)
            .map_err(|_| CredentialVaultError::InvalidDataRoot)?;
        validate_named_directory(data_root, &data_root_handle, false)
            .map_err(|_| CredentialVaultError::InvalidDataRoot)?;

        let pro_path = data_root.join("pro");
        ctx_history_core::platform_security::verify_private_directory(&pro_path)
            .map_err(|_| CredentialVaultError::Corrupt)?;
        let pro = match open_directory(&pro_path, false) {
            Ok(pro) => pro,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(CredentialVaultError::NotFound);
            }
            Err(_) => return Err(CredentialVaultError::Corrupt),
        };
        validate_directory_handle(&pro, false)?;
        validate_named_directory(&pro_path, &pro, false)?;
        Ok(Self {
            data_root_path: data_root.to_path_buf(),
            data_root: data_root_handle,
            pro_path,
            pro,
            native_selection,
        })
    }

    pub(super) fn open_lock(&self) -> Result<File, CredentialVaultError> {
        self.validate_layout()?;
        open_or_create_private_file(&self.pro_path.join(FILE_VAULT_LOCK))
    }

    pub(super) fn open_existing_lock(&self) -> Result<Option<File>, CredentialVaultError> {
        self.validate_layout()?;
        open_existing_private_file(&self.pro_path.join(FILE_VAULT_LOCK), None)
    }

    pub(super) fn read_selection(&self) -> Result<Option<BackendSelection>, CredentialVaultError> {
        self.validate_layout()?;
        let Some(mut bytes) = read_bounded_file(
            &self.pro_path.join(BACKEND_MARKER),
            MAX_BACKEND_MARKER_BYTES,
        )?
        else {
            return Ok(None);
        };
        let selection = match bytes.as_slice() {
            FILE_SELECTION => Ok(Some(BackendSelection::File)),
            native if native == self.native_selection => Ok(Some(BackendSelection::Native)),
            _ => Err(CredentialVaultError::Corrupt),
        };
        bytes.zeroize();
        selection
    }

    pub(super) fn write_selection(
        &self,
        selection: BackendSelection,
    ) -> Result<(), CredentialVaultError> {
        if self.read_selection()?.is_some() {
            return Err(CredentialVaultError::Corrupt);
        }
        let bytes = match selection {
            BackendSelection::File => FILE_SELECTION,
            BackendSelection::Native => self.native_selection,
        };
        atomic_write_private_file(&self.pro_path, BACKEND_MARKER, BACKEND_MARKER_STAGE, bytes)?;
        if self.read_selection()? == Some(selection) {
            Ok(())
        } else {
            Err(CredentialVaultError::Backend)
        }
    }

    pub(super) fn preexisting_sensitive_state(&self) -> Result<bool, CredentialVaultError> {
        self.validate_layout()?;
        for directory in [FILE_VAULT_DIRECTORY, PRIVATE_GRAPH_STORE_DIRECTORY] {
            if sensitive_entry_exists(&self.pro_path.join(directory), EntryKind::Directory)? {
                return Ok(true);
            }
        }
        for file in std::iter::once(BACKEND_MARKER_STAGE).chain(PRO_GRAPH_FILES) {
            if sensitive_entry_exists(&self.pro_path.join(file), EntryKind::File)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) fn validate_unselected_file_state(&self) -> Result<(), CredentialVaultError> {
        self.validate_layout()?;
        if sensitive_entry_exists(
            &self.pro_path.join(FILE_VAULT_DIRECTORY),
            EntryKind::Directory,
        )? || sensitive_entry_exists(&self.pro_path.join(BACKEND_MARKER_STAGE), EntryKind::File)?
        {
            Err(CredentialVaultError::Corrupt)
        } else {
            Ok(())
        }
    }

    pub(super) fn remove_marker_stage(&self) -> Result<(), CredentialVaultError> {
        remove_private_file(&self.pro_path.join(BACKEND_MARKER_STAGE)).map(|_| ())
    }

    pub(super) fn remove_selection(&self) -> Result<(), CredentialVaultError> {
        let _ = remove_private_file(&self.pro_path.join(BACKEND_MARKER_STAGE))?;
        let _ = remove_private_file(&self.pro_path.join(BACKEND_MARKER))?;
        if self.read_selection()?.is_none() {
            Ok(())
        } else {
            Err(CredentialVaultError::Backend)
        }
    }

    pub(super) fn load_file_record(
        &self,
        record_id: &str,
    ) -> Result<SecretBytes, CredentialVaultError> {
        validate_record_id(record_id)?;
        let vault = self
            .file_vault(false)?
            .ok_or(CredentialVaultError::NotFound)?;
        vault.load(record_id)
    }

    pub(super) fn load_or_store_file_record(
        &self,
        record_id: &str,
        candidate: &[u8],
    ) -> Result<SecretBytes, CredentialVaultError> {
        validate_record_id(record_id)?;
        drop(SecretBytes::new(candidate.to_vec())?);
        let vault = self
            .file_vault(true)?
            .ok_or(CredentialVaultError::Backend)?;
        match vault.load(record_id) {
            Ok(existing) => Ok(existing),
            Err(CredentialVaultError::NotFound) => {
                vault.store(record_id, candidate)?;
                vault.load(record_id)
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn store_file_record(
        &self,
        record_id: &str,
        value: &[u8],
    ) -> Result<(), CredentialVaultError> {
        validate_record_id(record_id)?;
        drop(SecretBytes::new(value.to_vec())?);
        self.file_vault(true)?
            .ok_or(CredentialVaultError::Backend)?
            .store(record_id, value)
    }

    pub(super) fn delete_file_record(&self, record_id: &str) -> Result<(), CredentialVaultError> {
        validate_record_id(record_id)?;
        self.file_vault(false)?
            .ok_or(CredentialVaultError::NotFound)?
            .delete(record_id)
    }

    pub(super) fn remove_empty_file_vault(&self) -> Result<(), CredentialVaultError> {
        let Some(vault) = self.file_vault(false)? else {
            return Ok(());
        };
        drop(vault);
        let path = self.pro_path.join(FILE_VAULT_DIRECTORY);
        fs::remove_dir(&path).map_err(|_| CredentialVaultError::Corrupt)?;
        if path_exists(&path)? {
            Err(CredentialVaultError::Backend)
        } else {
            Ok(())
        }
    }

    fn file_vault(&self, create: bool) -> Result<Option<FileVault>, CredentialVaultError> {
        self.validate_layout()?;
        let path = self.pro_path.join(FILE_VAULT_DIRECTORY);
        let directory = if create {
            Some(create_or_open_private_directory(&path)?)
        } else {
            open_existing_private_directory(&path)?
        };
        Ok(directory.map(|directory| FileVault { path, directory }))
    }

    fn validate_layout(&self) -> Result<(), CredentialVaultError> {
        ctx_history_core::platform_security::verify_private_directory(&self.data_root_path)
            .map_err(|_| CredentialVaultError::InvalidDataRoot)?;
        validate_directory_handle(&self.data_root, false)
            .map_err(|_| CredentialVaultError::InvalidDataRoot)?;
        validate_named_directory(&self.data_root_path, &self.data_root, false)
            .map_err(|_| CredentialVaultError::InvalidDataRoot)?;
        ctx_history_core::platform_security::verify_private_directory(&self.pro_path)
            .map_err(|_| CredentialVaultError::Corrupt)?;
        validate_directory_handle(&self.pro, false)?;
        validate_named_directory(&self.pro_path, &self.pro, false)
    }
}

struct FileVault {
    path: PathBuf,
    directory: File,
}

impl FileVault {
    fn load(&self, record_id: &str) -> Result<SecretBytes, CredentialVaultError> {
        validate_directory_handle(&self.directory, true)?;
        validate_named_directory(&self.path, &self.directory, true)?;
        let Some(mut bytes) =
            read_bounded_file(&self.path.join(record_id), MAX_STORED_SECRET_BYTES)?
        else {
            return Err(CredentialVaultError::NotFound);
        };
        match SecretBytes::new(std::mem::take(&mut bytes)) {
            Ok(secret) => Ok(secret),
            Err(_) => {
                bytes.zeroize();
                Err(CredentialVaultError::Corrupt)
            }
        }
    }

    fn store(&self, record_id: &str, value: &[u8]) -> Result<(), CredentialVaultError> {
        let stage = format!("{record_id}{FILE_RECORD_STAGE_SUFFIX}");
        atomic_write_private_file(&self.path, record_id, &stage, value)?;
        let persisted = self.load(record_id)?;
        if persisted.as_slice() == value {
            Ok(())
        } else {
            Err(CredentialVaultError::Backend)
        }
    }

    fn delete(&self, record_id: &str) -> Result<(), CredentialVaultError> {
        let stage = format!("{record_id}{FILE_RECORD_STAGE_SUFFIX}");
        let removed_stage = remove_private_file(&self.path.join(stage))?;
        let removed_record = remove_private_file(&self.path.join(record_id))?;
        if path_exists(&self.path.join(record_id))? {
            return Err(CredentialVaultError::Backend);
        }
        if removed_stage || removed_record {
            Ok(())
        } else {
            Err(CredentialVaultError::NotFound)
        }
    }
}

#[derive(Clone, Copy)]
enum EntryKind {
    Directory,
    File,
}

fn sensitive_entry_exists(path: &Path, expected: EntryKind) -> Result<bool, CredentialVaultError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            let expected_type = match expected {
                EntryKind::Directory => metadata.is_dir(),
                EntryKind::File => metadata.is_file(),
            };
            if expected_type && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
                Ok(true)
            } else {
                Err(CredentialVaultError::Corrupt)
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            Err(CredentialVaultError::Locked)
        }
        Err(_) => Err(CredentialVaultError::Backend),
    }
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

fn create_or_open_private_directory(path: &Path) -> Result<File, CredentialVaultError> {
    let created = match fs::create_dir(path) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            return Err(CredentialVaultError::Locked);
        }
        Err(_) => return Err(CredentialVaultError::Backend),
    };
    let directory = open_directory(path, created).map_err(|_| CredentialVaultError::Corrupt)?;
    if created {
        restrict_private_handle(&directory, ObjectKind::Directory)?;
    } else {
        verify_private_handle(&directory, ObjectKind::Directory)?;
    }
    validate_named_directory(path, &directory, true)?;
    Ok(directory)
}

fn open_existing_private_directory(path: &Path) -> Result<Option<File>, CredentialVaultError> {
    let directory = match open_directory(path, false) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            return Err(CredentialVaultError::Locked);
        }
        Err(_) => return Err(CredentialVaultError::Corrupt),
    };
    verify_private_handle(&directory, ObjectKind::Directory)?;
    validate_named_directory(path, &directory, true)?;
    Ok(Some(directory))
}

fn open_directory(path: &Path, mutate_acl: bool) -> io::Result<File> {
    let access = READ_CONTROL | if mutate_acl { WRITE_DAC } else { 0 };
    fs::OpenOptions::new()
        .access_mode(access)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

fn open_or_create_private_file(path: &Path) -> Result<File, CredentialVaultError> {
    match open_new_private_file(path) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            open_existing_private_file(path, None)?.ok_or(CredentialVaultError::Corrupt)
        }
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            Err(CredentialVaultError::Locked)
        }
        Err(_) => Err(CredentialVaultError::Backend),
    }
}

fn open_new_private_file(path: &Path) -> io::Result<File> {
    let file = fs::OpenOptions::new()
        .access_mode(GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .create_new(true)
        .open(path)?;
    restrict_private_handle(&file, ObjectKind::File).map_err(credential_io)?;
    validate_named_file(path, &file, Some(0)).map_err(credential_io)?;
    Ok(file)
}

fn open_existing_private_file(
    path: &Path,
    expected_size: Option<usize>,
) -> Result<Option<File>, CredentialVaultError> {
    let file = match fs::OpenOptions::new()
        .access_mode(GENERIC_READ | READ_CONTROL)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            return Err(CredentialVaultError::Locked);
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
    let named = open_directory(path, false).map_err(|_| CredentialVaultError::Corrupt)?;
    validate_directory_handle(&named, require_private)?;
    if file_identity(opened)? == file_identity(&named)? {
        Ok(())
    } else {
        Err(CredentialVaultError::Corrupt)
    }
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
    if file_identity(opened)? == file_identity(&named)? {
        Ok(())
    } else {
        Err(CredentialVaultError::Corrupt)
    }
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

fn path_exists(path: &Path) -> Result<bool, CredentialVaultError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 => Ok(true),
        Ok(_) => Err(CredentialVaultError::Corrupt),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(CredentialVaultError::Backend),
    }
}

fn read_bounded_file(path: &Path, maximum: usize) -> Result<Option<Vec<u8>>, CredentialVaultError> {
    let Some(mut file) = open_existing_private_file(path, None)? else {
        return Ok(None);
    };
    let metadata = file.metadata().map_err(|_| CredentialVaultError::Corrupt)?;
    if metadata.file_size() > maximum as u64 {
        return Err(CredentialVaultError::Corrupt);
    }
    let mut bytes = Zeroizing::new(Vec::with_capacity(metadata.file_size() as usize));
    if std::io::Read::by_ref(&mut file)
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > maximum
    {
        bytes.zeroize();
        return Err(CredentialVaultError::Corrupt);
    }
    validate_named_file(path, &file, Some(bytes.len()))?;
    Ok(Some(std::mem::take(&mut *bytes)))
}

fn atomic_write_private_file(
    parent: &Path,
    target_name: &str,
    stage_name: &str,
    bytes: &[u8],
) -> Result<(), CredentialVaultError> {
    if bytes.len() > MAX_STORED_SECRET_BYTES.max(MAX_BACKEND_MARKER_BYTES) {
        return Err(CredentialVaultError::Corrupt);
    }
    let target = parent.join(target_name);
    let stage = parent.join(stage_name);
    let _ = remove_private_file(&stage)?;
    let _ = open_existing_private_file(&target, None)?;
    let mut staged = open_new_private_file(&stage).map_err(|error| {
        if error.kind() == io::ErrorKind::PermissionDenied {
            CredentialVaultError::Locked
        } else {
            CredentialVaultError::Backend
        }
    })?;
    let result = (|| {
        staged
            .write_all(bytes)
            .map_err(|_| CredentialVaultError::Backend)?;
        staged
            .sync_all()
            .map_err(|_| CredentialVaultError::Backend)?;
        validate_named_file(&stage, &staged, Some(bytes.len()))?;
        drop(staged);
        move_file(&stage, &target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = remove_private_file(&stage);
    }
    result
}

fn move_file(source: &Path, target: &Path) -> Result<(), CredentialVaultError> {
    let source = wide_string(source.as_os_str());
    let target = wide_string(target.as_os_str());
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(CredentialVaultError::Backend)
    } else {
        Ok(())
    }
}

fn remove_private_file(path: &Path) -> Result<bool, CredentialVaultError> {
    let file = match fs::OpenOptions::new()
        .access_mode(GENERIC_READ | READ_CONTROL | DELETE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            return Err(CredentialVaultError::Locked);
        }
        Err(_) => return Err(CredentialVaultError::Corrupt),
    };
    validate_named_file(path, &file, None)?;
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
        return Err(CredentialVaultError::Backend);
    }
    drop(file);
    if path_exists(path)? {
        Err(CredentialVaultError::Backend)
    } else {
        Ok(true)
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

fn restrict_private_handle(handle: &File, kind: ObjectKind) -> Result<(), CredentialVaultError> {
    validate_object_type(handle, kind)?;
    let identity = CurrentUserIdentity::load()?;
    let mut acl = private_acl(identity.sid(), kind)?;
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
    verify_private_handle_with_identity(handle, kind, identity.sid())
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

fn private_acl(
    current_user: PSID,
    kind: ObjectKind,
) -> Result<AlignedBuffer, CredentialVaultError> {
    let sid_bytes = sid_size(current_user)?;
    let ace_header = size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>();
    let bytes = size_of::<ACL>()
        .checked_add(ace_header)
        .and_then(|value| value.checked_add(sid_bytes))
        .ok_or(CredentialVaultError::Backend)?;
    let mut acl = AlignedBuffer::new(bytes)?;
    let acl_bytes = u32::try_from(acl.byte_len()).map_err(|_| CredentialVaultError::Backend)?;
    if unsafe { InitializeAcl(acl.as_mut_ptr().cast(), acl_bytes, ACL_REVISION) } == 0
        || unsafe {
            AddAccessAllowedAceEx(
                acl.as_mut_ptr().cast(),
                ACL_REVISION,
                kind.inheritance_flags().into(),
                PRIVATE_ACCESS_MASK,
                current_user,
            )
        } == 0
    {
        Err(CredentialVaultError::Backend)
    } else {
        Ok(acl)
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
    byte_len: usize,
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
            byte_len,
        })
    }

    const fn byte_len(&self) -> usize {
        self.byte_len
    }

    fn as_ptr(&self) -> *const usize {
        self.words.as_ptr()
    }

    fn as_mut_ptr(&mut self) -> *mut usize {
        self.words.as_mut_ptr()
    }
}

fn wide_string(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn credential_io(_: CredentialVaultError) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, "unsafe credential vault")
}
