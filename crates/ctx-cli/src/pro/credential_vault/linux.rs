use std::{
    ffi::{CString, OsStr},
    fs::{self, File},
    io::{self, Read as _, Write as _},
    os::{
        fd::{AsRawFd as _, FromRawFd as _},
        unix::{
            ffi::OsStrExt as _,
            fs::{MetadataExt as _, PermissionsExt as _},
        },
    },
    path::{Component, Path, PathBuf},
};

use fs2::FileExt as _;
use zeroize::Zeroize as _;

#[cfg(target_os = "macos")]
use std::{
    ffi::{c_int, c_void},
    ptr::null_mut,
};

#[cfg(target_os = "linux")]
use super::secret_service;
use super::{
    validate_record_id, CredentialVaultBackend, CredentialVaultError, SecretBytes,
    MAX_STORED_SECRET_BYTES,
};

const BACKEND_MARKER: &str = ".ctx-pro.credential-backend-v1";
const BACKEND_MARKER_STAGE: &str = ".ctx-pro.credential-backend-v1.next";
const FILE_VAULT_DIRECTORY: &str = ".ctx-pro.credentials-v1";
const FILE_VAULT_LOCK: &str = ".ctx-pro.credentials-v1.lock";
const FILE_RECORD_STAGE_SUFFIX: &str = ".next";
const FILE_SELECTION: &[u8] = b"ctx-pro-credential-backend-v1:file\n";
#[cfg(target_os = "linux")]
const SECRET_SERVICE_SELECTION: &[u8] = b"ctx-pro-credential-backend-v1:secret-service\n";
const MAX_BACKEND_MARKER_BYTES: usize = 64;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;
const PRIVATE_GRAPH_STORE_DIRECTORY: &str = ".ctx-pro-key-store-v1";
const PRO_GRAPH_FILES: [&str; 3] = ["ctx-pro.db", "ctx-pro.db.next", "ctx-pro.db.previous"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BackendSelection {
    File,
    Native,
}

#[cfg(target_os = "linux")]
trait SecretServiceAdapter: Send + Sync {
    fn probe(&self) -> Result<(), CredentialVaultError>;
    fn load(&self, record_id: &str) -> Result<SecretBytes, CredentialVaultError>;
    fn load_or_store(
        &self,
        record_id: &str,
        candidate: &[u8],
    ) -> Result<SecretBytes, CredentialVaultError>;
    fn store(&self, record_id: &str, value: &[u8]) -> Result<(), CredentialVaultError>;
    fn delete(&self, record_id: &str) -> Result<(), CredentialVaultError>;
}

#[derive(Debug, Clone, Copy)]
#[cfg(target_os = "linux")]
struct ProductionSecretService;

#[cfg(target_os = "linux")]
impl SecretServiceAdapter for ProductionSecretService {
    fn probe(&self) -> Result<(), CredentialVaultError> {
        secret_service::PlatformBackend::production().probe()
    }

    fn load(&self, record_id: &str) -> Result<SecretBytes, CredentialVaultError> {
        secret_service::PlatformBackend::production().load(record_id)
    }

    fn load_or_store(
        &self,
        record_id: &str,
        candidate: &[u8],
    ) -> Result<SecretBytes, CredentialVaultError> {
        secret_service::PlatformBackend::production().load_or_store(record_id, candidate)
    }

    fn store(&self, record_id: &str, value: &[u8]) -> Result<(), CredentialVaultError> {
        secret_service::PlatformBackend::production().store(record_id, value)
    }

    fn delete(&self, record_id: &str) -> Result<(), CredentialVaultError> {
        secret_service::PlatformBackend::production().delete(record_id)
    }
}

#[cfg(target_os = "linux")]
pub(super) struct PlatformBackend(LinuxBackend<ProductionSecretService>);

#[cfg(target_os = "linux")]
impl PlatformBackend {
    pub(super) fn production(data_root: &Path) -> Self {
        Self(LinuxBackend::new(data_root, ProductionSecretService))
    }

    pub(super) fn cleanup_if_empty(&self) -> Result<(), CredentialVaultError> {
        self.0.cleanup_if_empty()
    }
}

#[cfg(target_os = "linux")]
impl CredentialVaultBackend for PlatformBackend {
    fn load(&self, record_id: &str) -> Result<SecretBytes, CredentialVaultError> {
        self.0.load(record_id)
    }

    fn load_or_store(
        &self,
        record_id: &str,
        candidate: &[u8],
    ) -> Result<SecretBytes, CredentialVaultError> {
        self.0.load_or_store(record_id, candidate)
    }

    fn store(&self, record_id: &str, value: &[u8]) -> Result<(), CredentialVaultError> {
        self.0.store(record_id, value)
    }

    fn delete(&self, record_id: &str) -> Result<(), CredentialVaultError> {
        self.0.delete(record_id)
    }
}

#[cfg(target_os = "linux")]
struct LinuxBackend<A> {
    data_root: PathBuf,
    secret_service: A,
}

#[cfg(target_os = "linux")]
impl<A> LinuxBackend<A> {
    fn new(data_root: &Path, secret_service: A) -> Self {
        Self {
            data_root: data_root.to_path_buf(),
            secret_service,
        }
    }
}

#[cfg(target_os = "linux")]
impl<A: SecretServiceAdapter> LinuxBackend<A> {
    fn inspect_unselected_secret_service<T>(
        secret_service: &A,
        operation: impl FnOnce(&A) -> Result<T, CredentialVaultError>,
    ) -> Result<T, CredentialVaultError> {
        match secret_service.probe() {
            Ok(()) => operation(secret_service),
            Err(CredentialVaultError::Unavailable { .. }) => Err(CredentialVaultError::NotFound),
            Err(error) => Err(error),
        }
    }

    fn with_mutating_selected_backend<T>(
        &self,
        operation: impl FnOnce(BackendSelection, &VaultRoot, &A) -> Result<T, CredentialVaultError>,
    ) -> Result<T, CredentialVaultError> {
        let root = VaultRoot::open(&self.data_root, SECRET_SERVICE_SELECTION)?;
        let lock = root.open_lock()?;
        lock.lock_exclusive()
            .map_err(|error| map_io_error(&error))?;
        let result = (|| {
            if let Some(selection) = root.read_selection()? {
                return operation(selection, &root, &self.secret_service);
            }
            root.validate_unselected_file_state()?;
            match self.secret_service.probe() {
                Ok(()) => {
                    root.write_selection(BackendSelection::Native)?;
                    // Any failure after the operation starts is indeterminate:
                    // Secret Service may have committed before a verification
                    // read failed. Keep the durable selection and fail closed
                    // rather than replaying the mutation into the file vault.
                    operation(BackendSelection::Native, &root, &self.secret_service)
                }
                Err(error @ CredentialVaultError::Unavailable { .. })
                    if root.preexisting_sensitive_state()? =>
                {
                    Err(error)
                }
                Err(CredentialVaultError::Unavailable { .. }) => {
                    root.write_selection(BackendSelection::File)?;
                    operation(BackendSelection::File, &root, &self.secret_service)
                }
                Err(error) => Err(error),
            }
        })();
        let unlock = fs2::FileExt::unlock(&lock).map_err(|error| map_io_error(&error));
        match (result, unlock) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }

    fn with_read_backend<T>(
        &self,
        operation: impl FnOnce(
            Option<BackendSelection>,
            &VaultRoot,
            &A,
        ) -> Result<T, CredentialVaultError>,
    ) -> Result<T, CredentialVaultError> {
        let root = VaultRoot::open(&self.data_root, SECRET_SERVICE_SELECTION)?;
        let initial_selection = root.read_selection()?;
        let Some(lock) = root.open_existing_lock()? else {
            return if initial_selection.is_some() {
                Err(CredentialVaultError::Corrupt)
            } else {
                root.validate_unselected_file_state()?;
                operation(None, &root, &self.secret_service)
            };
        };
        lock.lock_exclusive()
            .map_err(|error| map_io_error(&error))?;
        let result = (|| {
            let selection = root.read_selection()?;
            if selection.is_none() {
                root.validate_unselected_file_state()?;
            }
            operation(selection, &root, &self.secret_service)
        })();
        let unlock = fs2::FileExt::unlock(&lock).map_err(|error| map_io_error(&error));
        match (result, unlock) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }

    fn cleanup_if_empty(&self) -> Result<(), CredentialVaultError> {
        let root = match VaultRoot::open(&self.data_root, SECRET_SERVICE_SELECTION) {
            Ok(root) => root,
            Err(CredentialVaultError::NotFound) => return Ok(()),
            Err(error) => return Err(error),
        };
        let initial_selection = root.read_selection()?;
        let Some(lock) = root.open_existing_lock()? else {
            return if initial_selection.is_some() {
                Err(CredentialVaultError::Corrupt)
            } else {
                Ok(())
            };
        };
        lock.lock_exclusive()
            .map_err(|error| map_io_error(&error))?;
        let result = (|| {
            let Some(selection) = root.read_selection()? else {
                root.remove_marker_stage()?;
                return Ok(());
            };
            match selection {
                BackendSelection::File => root.remove_empty_file_vault()?,
                BackendSelection::Native => self.secret_service.probe()?,
            }
            root.remove_selection()
        })();
        let unlock = fs2::FileExt::unlock(&lock).map_err(|error| map_io_error(&error));
        match (result, unlock) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), _) | (Ok(()), Err(error)) => Err(error),
        }
    }
}

#[cfg(target_os = "linux")]
impl<A: SecretServiceAdapter> CredentialVaultBackend for LinuxBackend<A> {
    fn load(&self, record_id: &str) -> Result<SecretBytes, CredentialVaultError> {
        validate_record_id(record_id)?;
        self.with_read_backend(|selection, root, secret_service| match selection {
            Some(BackendSelection::File) => root.load_file_record(record_id),
            Some(BackendSelection::Native) => secret_service.load(record_id),
            None => Self::inspect_unselected_secret_service(secret_service, |secret_service| {
                secret_service.load(record_id)
            }),
        })
    }

    fn load_or_store(
        &self,
        record_id: &str,
        candidate: &[u8],
    ) -> Result<SecretBytes, CredentialVaultError> {
        validate_record_id(record_id)?;
        drop(SecretBytes::new(candidate.to_vec())?);
        self.with_mutating_selected_backend(|selection, root, secret_service| match selection {
            BackendSelection::File => root.load_or_store_file_record(record_id, candidate),
            BackendSelection::Native => secret_service.load_or_store(record_id, candidate),
        })
    }

    fn store(&self, record_id: &str, value: &[u8]) -> Result<(), CredentialVaultError> {
        validate_record_id(record_id)?;
        drop(SecretBytes::new(value.to_vec())?);
        self.with_mutating_selected_backend(|selection, root, secret_service| match selection {
            BackendSelection::File => root.store_file_record(record_id, value),
            BackendSelection::Native => secret_service.store(record_id, value),
        })
    }

    fn delete(&self, record_id: &str) -> Result<(), CredentialVaultError> {
        validate_record_id(record_id)?;
        self.with_read_backend(|selection, root, secret_service| match selection {
            Some(BackendSelection::File) => root.delete_file_record(record_id),
            Some(BackendSelection::Native) => secret_service.delete(record_id),
            None => Self::inspect_unselected_secret_service(secret_service, |secret_service| {
                secret_service.delete(record_id)
            }),
        })
    }
}

pub(super) struct VaultRoot {
    pro: File,
    native_selection: &'static [u8],
}

impl VaultRoot {
    pub(super) fn open(
        data_root: &Path,
        native_selection: &'static [u8],
    ) -> Result<Self, CredentialVaultError> {
        let data = open_absolute_directory(data_root)?;
        verify_private_directory(&data).map_err(|_| CredentialVaultError::InvalidDataRoot)?;
        let pro = match open_directory_at(&data, OsStr::new("pro")) {
            Ok(pro) => pro,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(CredentialVaultError::NotFound)
            }
            Err(error) => return Err(map_path_error(&error)),
        };
        verify_private_directory(&pro).map_err(|_| CredentialVaultError::Corrupt)?;
        Ok(Self {
            pro,
            native_selection,
        })
    }

    pub(super) fn open_lock(&self) -> Result<File, CredentialVaultError> {
        open_or_create_private_file(&self.pro, OsStr::new(FILE_VAULT_LOCK), true)
    }

    pub(super) fn open_existing_lock(&self) -> Result<Option<File>, CredentialVaultError> {
        open_existing_private_file(&self.pro, OsStr::new(FILE_VAULT_LOCK), libc::O_RDWR)
    }

    pub(super) fn read_selection(&self) -> Result<Option<BackendSelection>, CredentialVaultError> {
        let Some(file) =
            open_existing_private_file(&self.pro, OsStr::new(BACKEND_MARKER), libc::O_RDONLY)?
        else {
            return Ok(None);
        };
        let bytes = read_bounded(file, MAX_BACKEND_MARKER_BYTES)?;
        match bytes.as_slice() {
            FILE_SELECTION => Ok(Some(BackendSelection::File)),
            native if native == self.native_selection => Ok(Some(BackendSelection::Native)),
            _ => Err(CredentialVaultError::Corrupt),
        }
    }

    pub(super) fn write_selection(
        &self,
        selection: BackendSelection,
    ) -> Result<(), CredentialVaultError> {
        let bytes = match selection {
            BackendSelection::File => FILE_SELECTION,
            BackendSelection::Native => self.native_selection,
        };
        atomic_write_private_file(
            &self.pro,
            OsStr::new(BACKEND_MARKER),
            OsStr::new(BACKEND_MARKER_STAGE),
            bytes,
        )?;
        if self.read_selection()? == Some(selection) {
            Ok(())
        } else {
            Err(CredentialVaultError::Backend)
        }
    }

    pub(super) fn preexisting_sensitive_state(&self) -> Result<bool, CredentialVaultError> {
        for directory in [FILE_VAULT_DIRECTORY, PRIVATE_GRAPH_STORE_DIRECTORY] {
            if sensitive_entry_exists(&self.pro, OsStr::new(directory), EntryKind::Directory)? {
                return Ok(true);
            }
        }
        for file in std::iter::once(BACKEND_MARKER_STAGE).chain(PRO_GRAPH_FILES) {
            if sensitive_entry_exists(&self.pro, OsStr::new(file), EntryKind::File)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) fn validate_unselected_file_state(&self) -> Result<(), CredentialVaultError> {
        if sensitive_entry_exists(
            &self.pro,
            OsStr::new(FILE_VAULT_DIRECTORY),
            EntryKind::Directory,
        )? || sensitive_entry_exists(
            &self.pro,
            OsStr::new(BACKEND_MARKER_STAGE),
            EntryKind::File,
        )? {
            Err(CredentialVaultError::Corrupt)
        } else {
            Ok(())
        }
    }

    pub(super) fn remove_marker_stage(&self) -> Result<(), CredentialVaultError> {
        if remove_private_file(&self.pro, OsStr::new(BACKEND_MARKER_STAGE))? {
            self.pro.sync_all().map_err(|error| map_io_error(&error))?;
        }
        Ok(())
    }

    pub(super) fn remove_selection(&self) -> Result<(), CredentialVaultError> {
        let removed_stage = remove_private_file(&self.pro, OsStr::new(BACKEND_MARKER_STAGE))?;
        let removed_marker = remove_private_file(&self.pro, OsStr::new(BACKEND_MARKER))?;
        if removed_stage || removed_marker {
            self.pro.sync_all().map_err(|error| map_io_error(&error))?;
        }
        if self.read_selection()?.is_none() {
            Ok(())
        } else {
            Err(CredentialVaultError::Backend)
        }
    }

    fn file_vault(&self, create: bool) -> Result<Option<FileVault>, CredentialVaultError> {
        match open_directory_at(&self.pro, OsStr::new(FILE_VAULT_DIRECTORY)) {
            Ok(directory) => {
                verify_private_directory(&directory).map_err(|_| CredentialVaultError::Corrupt)?;
                Ok(Some(FileVault { directory }))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound && !create => Ok(None),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                create_private_directory(&self.pro, OsStr::new(FILE_VAULT_DIRECTORY))?;
                self.pro.sync_all().map_err(|error| map_io_error(&error))?;
                let directory = open_directory_at(&self.pro, OsStr::new(FILE_VAULT_DIRECTORY))
                    .map_err(|error| map_path_error(&error))?;
                verify_private_directory(&directory).map_err(|_| CredentialVaultError::Corrupt)?;
                Ok(Some(FileVault { directory }))
            }
            Err(error) => Err(map_path_error(&error)),
        }
    }

    pub(super) fn load_file_record(
        &self,
        record_id: &str,
    ) -> Result<SecretBytes, CredentialVaultError> {
        self.file_vault(false)?
            .ok_or(CredentialVaultError::NotFound)?
            .load(record_id)
    }

    pub(super) fn load_or_store_file_record(
        &self,
        record_id: &str,
        candidate: &[u8],
    ) -> Result<SecretBytes, CredentialVaultError> {
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
        self.file_vault(true)?
            .ok_or(CredentialVaultError::Backend)?
            .store(record_id, value)
    }

    pub(super) fn delete_file_record(&self, record_id: &str) -> Result<(), CredentialVaultError> {
        self.file_vault(false)?
            .ok_or(CredentialVaultError::NotFound)?
            .delete(record_id)
    }

    pub(super) fn remove_empty_file_vault(&self) -> Result<(), CredentialVaultError> {
        let Some(vault) = self.file_vault(false)? else {
            return Ok(());
        };
        let name = path_component(OsStr::new(FILE_VAULT_DIRECTORY))?;
        // SAFETY: both the parent descriptor and NUL-terminated entry name are valid.
        let result =
            unsafe { libc::unlinkat(self.pro.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) };
        if result != 0 {
            return Err(map_path_error(&io::Error::last_os_error()));
        }
        drop(vault);
        self.pro.sync_all().map_err(|error| map_io_error(&error))?;
        match open_directory_at(&self.pro, OsStr::new(FILE_VAULT_DIRECTORY)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Ok(_) => Err(CredentialVaultError::Backend),
            Err(error) => Err(map_path_error(&error)),
        }
    }
}

#[derive(Clone, Copy)]
enum EntryKind {
    Directory,
    File,
}

fn sensitive_entry_exists(
    parent: &File,
    name: &OsStr,
    expected: EntryKind,
) -> Result<bool, CredentialVaultError> {
    let name = path_component(name)?;
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: the parent descriptor and NUL-terminated component are valid;
    // successful fstatat initializes the complete stat structure.
    let result = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result != 0 {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::NotFound {
            Ok(false)
        } else {
            Err(map_path_error(&error))
        };
    }
    // SAFETY: the successful fstatat above initialized every field.
    let metadata = unsafe { metadata.assume_init() };
    let file_type = metadata.st_mode & libc::S_IFMT;
    let expected_type = match expected {
        EntryKind::Directory => libc::S_IFDIR,
        EntryKind::File => libc::S_IFREG,
    };
    if file_type == expected_type {
        Ok(true)
    } else {
        Err(CredentialVaultError::Corrupt)
    }
}

struct FileVault {
    directory: File,
}

impl FileVault {
    fn load(&self, record_id: &str) -> Result<SecretBytes, CredentialVaultError> {
        let Some(file) =
            open_existing_private_file(&self.directory, OsStr::new(record_id), libc::O_RDONLY)?
        else {
            return Err(CredentialVaultError::NotFound);
        };
        let mut bytes = read_bounded(file, MAX_STORED_SECRET_BYTES)?;
        if bytes.len() > MAX_STORED_SECRET_BYTES {
            bytes.zeroize();
            return Err(CredentialVaultError::Corrupt);
        }
        SecretBytes::new(bytes).map_err(|_| CredentialVaultError::Corrupt)
    }

    fn store(&self, record_id: &str, value: &[u8]) -> Result<(), CredentialVaultError> {
        let stage = record_stage_name(record_id);
        atomic_write_private_file(
            &self.directory,
            OsStr::new(record_id),
            stage.as_os_str(),
            value,
        )?;
        let persisted = self.load(record_id)?;
        if persisted.as_slice() == value {
            Ok(())
        } else {
            Err(CredentialVaultError::Backend)
        }
    }

    fn delete(&self, record_id: &str) -> Result<(), CredentialVaultError> {
        let stage = record_stage_name(record_id);
        let removed_stage = remove_private_file(&self.directory, stage.as_os_str())?;
        let removed_record = remove_private_file(&self.directory, OsStr::new(record_id))?;
        if removed_stage || removed_record {
            self.directory
                .sync_all()
                .map_err(|error| map_io_error(&error))?;
        }
        if open_existing_private_file(&self.directory, OsStr::new(record_id), libc::O_RDONLY)?
            .is_some()
            || open_existing_private_file(&self.directory, stage.as_os_str(), libc::O_RDONLY)?
                .is_some()
        {
            return Err(CredentialVaultError::Backend);
        }
        if removed_stage || removed_record {
            Ok(())
        } else {
            Err(CredentialVaultError::NotFound)
        }
    }
}

fn open_absolute_directory(path: &Path) -> Result<File, CredentialVaultError> {
    if !path.is_absolute()
        || path == Path::new("/")
        || path.components().any(|component| {
            matches!(
                component,
                Component::CurDir | Component::ParentDir | Component::Prefix(_)
            )
        })
    {
        return Err(CredentialVaultError::InvalidDataRoot);
    }
    let root = CString::new("/").map_err(|_| CredentialVaultError::InvalidDataRoot)?;
    // SAFETY: AT_FDCWD and the static NUL-terminated root path are valid.
    let descriptor = unsafe {
        libc::openat(
            libc::AT_FDCWD,
            root.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY,
        )
    };
    let mut current =
        file_from_descriptor(descriptor).map_err(|_| CredentialVaultError::InvalidDataRoot)?;
    for component in path.components() {
        if let Component::Normal(name) = component {
            current = open_directory_at(&current, name)
                .map_err(|_| CredentialVaultError::InvalidDataRoot)?;
        }
    }
    Ok(current)
}

fn open_directory_at(parent: &File, name: &OsStr) -> io::Result<File> {
    open_directory_raw(parent.as_raw_fd(), name)
}

fn open_directory_raw(parent: libc::c_int, name: &OsStr) -> io::Result<File> {
    let name = io_path_component(name)?;
    // SAFETY: parent is a live descriptor and name is a NUL-terminated path component.
    let descriptor = unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY,
        )
    };
    file_from_descriptor(descriptor)
}

fn create_private_directory(parent: &File, name: &OsStr) -> Result<(), CredentialVaultError> {
    let name = path_component(name)?;
    // SAFETY: parent is a live descriptor and name is a NUL-terminated path component.
    let result =
        unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), PRIVATE_DIRECTORY_MODE) };
    let created = result == 0;
    if !created {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::AlreadyExists {
            return Err(map_path_error(&error));
        }
    }
    let directory = open_directory_raw(parent.as_raw_fd(), OsStr::from_bytes(name.as_bytes()))
        .map_err(|error| map_path_error(&error))?;
    if created {
        directory
            .set_permissions(fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE))
            .map_err(|error| map_io_error(&error))?;
    }
    verify_private_directory(&directory).map_err(|_| CredentialVaultError::Corrupt)
}

fn open_or_create_private_file(
    parent: &File,
    name: &OsStr,
    read_write: bool,
) -> Result<File, CredentialVaultError> {
    let access = if read_write {
        libc::O_RDWR
    } else {
        libc::O_WRONLY
    };
    match open_file_at(
        parent,
        name,
        access | libc::O_CREAT | libc::O_EXCL,
        PRIVATE_FILE_MODE,
    ) {
        Ok(file) => {
            file.set_permissions(fs::Permissions::from_mode(PRIVATE_FILE_MODE))
                .map_err(|error| map_io_error(&error))?;
            verify_private_file(&file)?;
            Ok(file)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            open_existing_private_file(parent, name, access)?.ok_or(CredentialVaultError::Backend)
        }
        Err(error) => Err(map_path_error(&error)),
    }
}

fn open_existing_private_file(
    parent: &File,
    name: &OsStr,
    access: libc::c_int,
) -> Result<Option<File>, CredentialVaultError> {
    match open_file_at(parent, name, access, 0) {
        Ok(file) => {
            verify_private_file(&file)?;
            Ok(Some(file))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(map_path_error(&error)),
    }
}

fn open_file_at(parent: &File, name: &OsStr, flags: libc::c_int, mode: u32) -> io::Result<File> {
    let name = io_path_component(name)?;
    // SAFETY: parent is a live descriptor, name is NUL-terminated, and mode is valid.
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            mode,
        )
    };
    file_from_descriptor(descriptor)
}

fn file_from_descriptor(descriptor: libc::c_int) -> io::Result<File> {
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: descriptor was just returned by openat and ownership transfers to File.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

fn verify_private_directory(directory: &File) -> io::Result<()> {
    let metadata = directory.metadata()?;
    if metadata.file_type().is_dir()
        && metadata.uid() == effective_uid()
        && metadata.mode() & 0o7777 == PRIVATE_DIRECTORY_MODE
    {
        verify_no_macos_extended_acl(directory)
    } else {
        Err(private_path_error())
    }
}

fn verify_private_file(file: &File) -> Result<(), CredentialVaultError> {
    let metadata = file.metadata().map_err(|error| map_io_error(&error))?;
    if metadata.file_type().is_file()
        && metadata.uid() == effective_uid()
        && metadata.mode() & 0o7777 == PRIVATE_FILE_MODE
        && metadata.nlink() == 1
    {
        verify_no_macos_extended_acl(file).map_err(|_| CredentialVaultError::Corrupt)
    } else {
        Err(CredentialVaultError::Corrupt)
    }
}

#[cfg(not(target_os = "macos"))]
fn verify_no_macos_extended_acl(_file: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn verify_no_macos_extended_acl(file: &File) -> io::Result<()> {
    const ACL_TYPE_EXTENDED: c_int = 0x0000_0100;
    const ACL_FIRST_ENTRY: c_int = 0;
    const DARWIN_ENOENT: c_int = 2;
    const DARWIN_EINVAL: c_int = 22;

    unsafe extern "C" {
        fn __error() -> *mut c_int;
        fn acl_get_fd_np(fd: c_int, acl_type: c_int) -> *mut c_void;
        fn acl_get_entry(acl: *mut c_void, entry_id: c_int, entry: *mut *mut c_void) -> c_int;
        fn acl_free(object: *mut c_void) -> c_int;
    }

    struct ExtendedAcl(*mut c_void);

    impl Drop for ExtendedAcl {
        fn drop(&mut self) {
            // SAFETY: this guard owns the allocation returned by
            // `acl_get_fd_np` and releases it exactly once.
            unsafe {
                acl_free(self.0);
            }
        }
    }

    // SAFETY: the descriptor is live and `ACL_TYPE_EXTENDED` is the stable
    // Darwin ACL type for the non-POSIX access-control list.
    unsafe {
        *__error() = 0;
    }
    let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(DARWIN_ENOENT) {
            Ok(())
        } else {
            Err(error)
        };
    }
    let acl = ExtendedAcl(acl);
    let mut entry = null_mut();
    // Darwin reports EINVAL when ACL_FIRST_ENTRY is requested from a valid
    // empty ACL. Any actual entry could grant access beyond mode 0700/0600.
    // SAFETY: `acl` is live and `entry` is a valid out pointer.
    unsafe {
        *__error() = 0;
    }
    let result = unsafe { acl_get_entry(acl.0, ACL_FIRST_ENTRY, &raw mut entry) };
    if result == 0 {
        Err(private_path_error())
    } else {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(DARWIN_EINVAL) {
            Ok(())
        } else {
            Err(error)
        }
    }
}

fn atomic_write_private_file(
    directory: &File,
    target: &OsStr,
    stage: &OsStr,
    bytes: &[u8],
) -> Result<(), CredentialVaultError> {
    if remove_private_file(directory, stage)? {
        directory.sync_all().map_err(|error| map_io_error(&error))?;
    }
    let _ = open_existing_private_file(directory, target, libc::O_RDONLY)?;
    let mut staged = open_or_create_private_file(directory, stage, false)?;
    let write_result = (|| {
        staged
            .write_all(bytes)
            .map_err(|error| map_io_error(&error))?;
        staged.sync_all().map_err(|error| map_io_error(&error))?;
        rename_entry(directory, stage, target)?;
        directory.sync_all().map_err(|error| map_io_error(&error))
    })();
    if write_result.is_err() {
        let _ = remove_private_file(directory, stage);
    }
    write_result
}

fn rename_entry(
    directory: &File,
    source: &OsStr,
    target: &OsStr,
) -> Result<(), CredentialVaultError> {
    let source = path_component(source)?;
    let target = path_component(target)?;
    // SAFETY: the descriptor is live and both names are NUL-terminated components.
    let result = unsafe {
        libc::renameat(
            directory.as_raw_fd(),
            source.as_ptr(),
            directory.as_raw_fd(),
            target.as_ptr(),
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(map_path_error(&io::Error::last_os_error()))
    }
}

fn remove_private_file(directory: &File, name: &OsStr) -> Result<bool, CredentialVaultError> {
    if open_existing_private_file(directory, name, libc::O_RDONLY)?.is_none() {
        return Ok(false);
    }
    let name = path_component(name)?;
    // SAFETY: the descriptor is live and name is a NUL-terminated component.
    let result = unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) };
    if result == 0 {
        Ok(true)
    } else {
        Err(map_path_error(&io::Error::last_os_error()))
    }
}

fn read_bounded(mut file: File, maximum: usize) -> Result<Vec<u8>, CredentialVaultError> {
    let metadata = file.metadata().map_err(|error| map_io_error(&error))?;
    if metadata.len() > maximum as u64 {
        return Err(CredentialVaultError::Corrupt);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    let read_result = std::io::Read::by_ref(&mut file)
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| map_io_error(&error));
    if let Err(error) = read_result {
        bytes.zeroize();
        return Err(error);
    }
    if bytes.len() > maximum {
        bytes.zeroize();
        return Err(CredentialVaultError::Corrupt);
    }
    Ok(bytes)
}

fn record_stage_name(record_id: &str) -> std::ffi::OsString {
    format!("{record_id}{FILE_RECORD_STAGE_SUFFIX}").into()
}

fn path_component(name: &OsStr) -> Result<CString, CredentialVaultError> {
    io_path_component(name).map_err(|_| CredentialVaultError::Corrupt)
}

fn io_path_component(name: &OsStr) -> io::Result<CString> {
    if name.is_empty() || name.as_bytes().contains(&b'/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "vault path must be one component",
        ));
    }
    CString::new(name.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "vault path contains a NUL byte",
        )
    })
}

fn map_path_error(error: &io::Error) -> CredentialVaultError {
    if error.kind() == io::ErrorKind::PermissionDenied {
        CredentialVaultError::Locked
    } else if matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) {
        CredentialVaultError::Corrupt
    } else {
        CredentialVaultError::Backend
    }
}

fn map_io_error(error: &io::Error) -> CredentialVaultError {
    if error.kind() == io::ErrorKind::PermissionDenied {
        CredentialVaultError::Locked
    } else {
        CredentialVaultError::Backend
    }
}

fn private_path_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "credential vault path is not owner-private",
    )
}

fn effective_uid() -> u32 {
    // SAFETY: geteuid has no arguments and no failure mode on Linux.
    unsafe { libc::geteuid() }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "linux_tests.rs"]
mod tests;
