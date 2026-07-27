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

use super::{
    secret_service, validate_record_id, CredentialVaultBackend, CredentialVaultError, SecretBytes,
    MAX_STORED_SECRET_BYTES,
};

const BACKEND_MARKER: &str = ".ctx-pro.credential-backend-v1";
const BACKEND_MARKER_STAGE: &str = ".ctx-pro.credential-backend-v1.next";
const FILE_VAULT_DIRECTORY: &str = ".ctx-pro.credentials-v1";
const FILE_VAULT_LOCK: &str = ".ctx-pro.credentials-v1.lock";
const FILE_RECORD_STAGE_SUFFIX: &str = ".next";
const FILE_SELECTION: &[u8] = b"ctx-pro-credential-backend-v1:file\n";
const SECRET_SERVICE_SELECTION: &[u8] = b"ctx-pro-credential-backend-v1:secret-service\n";
const MAX_BACKEND_MARKER_BYTES: usize = 64;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendSelection {
    File,
    SecretService,
}

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
struct ProductionSecretService;

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

pub(super) struct PlatformBackend(LinuxBackend<ProductionSecretService>);

impl PlatformBackend {
    pub(super) fn production(data_root: &Path) -> Self {
        Self(LinuxBackend::new(data_root, ProductionSecretService))
    }

    pub(super) fn cleanup_if_empty(&self) -> Result<(), CredentialVaultError> {
        self.0.cleanup_if_empty()
    }
}

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

struct LinuxBackend<A> {
    data_root: PathBuf,
    secret_service: A,
}

impl<A> LinuxBackend<A> {
    fn new(data_root: &Path, secret_service: A) -> Self {
        Self {
            data_root: data_root.to_path_buf(),
            secret_service,
        }
    }
}

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
        let root = VaultRoot::open(&self.data_root)?;
        let lock = root.open_lock()?;
        lock.lock_exclusive()
            .map_err(|error| map_io_error(&error))?;
        let result = (|| {
            if let Some(selection) = root.read_selection()? {
                return operation(selection, &root, &self.secret_service);
            }
            match self.secret_service.probe() {
                Ok(()) => {
                    root.write_selection(BackendSelection::SecretService)?;
                    // Any failure after the operation starts is indeterminate:
                    // Secret Service may have committed before a verification
                    // read failed. Keep the durable selection and fail closed
                    // rather than replaying the mutation into the file vault.
                    operation(BackendSelection::SecretService, &root, &self.secret_service)
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
        let root = VaultRoot::open(&self.data_root)?;
        let initial_selection = root.read_selection()?;
        let Some(lock) = root.open_existing_lock()? else {
            return if initial_selection.is_some() {
                Err(CredentialVaultError::Corrupt)
            } else {
                operation(None, &root, &self.secret_service)
            };
        };
        lock.lock_exclusive()
            .map_err(|error| map_io_error(&error))?;
        let result = operation(root.read_selection()?, &root, &self.secret_service);
        let unlock = fs2::FileExt::unlock(&lock).map_err(|error| map_io_error(&error));
        match (result, unlock) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }

    fn cleanup_if_empty(&self) -> Result<(), CredentialVaultError> {
        let root = match VaultRoot::open(&self.data_root) {
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
                BackendSelection::SecretService => self.secret_service.probe()?,
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

impl<A: SecretServiceAdapter> CredentialVaultBackend for LinuxBackend<A> {
    fn load(&self, record_id: &str) -> Result<SecretBytes, CredentialVaultError> {
        validate_record_id(record_id)?;
        self.with_read_backend(|selection, root, secret_service| match selection {
            Some(BackendSelection::File) => root.load_file_record(record_id),
            Some(BackendSelection::SecretService) => secret_service.load(record_id),
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
            BackendSelection::SecretService => secret_service.load_or_store(record_id, candidate),
        })
    }

    fn store(&self, record_id: &str, value: &[u8]) -> Result<(), CredentialVaultError> {
        validate_record_id(record_id)?;
        drop(SecretBytes::new(value.to_vec())?);
        self.with_mutating_selected_backend(|selection, root, secret_service| match selection {
            BackendSelection::File => root.store_file_record(record_id, value),
            BackendSelection::SecretService => secret_service.store(record_id, value),
        })
    }

    fn delete(&self, record_id: &str) -> Result<(), CredentialVaultError> {
        validate_record_id(record_id)?;
        self.with_read_backend(|selection, root, secret_service| match selection {
            Some(BackendSelection::File) => root.delete_file_record(record_id),
            Some(BackendSelection::SecretService) => secret_service.delete(record_id),
            None => Self::inspect_unselected_secret_service(secret_service, |secret_service| {
                secret_service.delete(record_id)
            }),
        })
    }
}

struct VaultRoot {
    pro: File,
}

impl VaultRoot {
    fn open(data_root: &Path) -> Result<Self, CredentialVaultError> {
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
        Ok(Self { pro })
    }

    fn open_lock(&self) -> Result<File, CredentialVaultError> {
        open_or_create_private_file(&self.pro, OsStr::new(FILE_VAULT_LOCK), true)
    }

    fn open_existing_lock(&self) -> Result<Option<File>, CredentialVaultError> {
        open_existing_private_file(&self.pro, OsStr::new(FILE_VAULT_LOCK), libc::O_RDWR)
    }

    fn read_selection(&self) -> Result<Option<BackendSelection>, CredentialVaultError> {
        let Some(file) =
            open_existing_private_file(&self.pro, OsStr::new(BACKEND_MARKER), libc::O_RDONLY)?
        else {
            return Ok(None);
        };
        let bytes = read_bounded(file, MAX_BACKEND_MARKER_BYTES)?;
        match bytes.as_slice() {
            FILE_SELECTION => Ok(Some(BackendSelection::File)),
            SECRET_SERVICE_SELECTION => Ok(Some(BackendSelection::SecretService)),
            _ => Err(CredentialVaultError::Corrupt),
        }
    }

    fn write_selection(&self, selection: BackendSelection) -> Result<(), CredentialVaultError> {
        let bytes = match selection {
            BackendSelection::File => FILE_SELECTION,
            BackendSelection::SecretService => SECRET_SERVICE_SELECTION,
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

    fn remove_marker_stage(&self) -> Result<(), CredentialVaultError> {
        if remove_private_file(&self.pro, OsStr::new(BACKEND_MARKER_STAGE))? {
            self.pro.sync_all().map_err(|error| map_io_error(&error))?;
        }
        Ok(())
    }

    fn remove_selection(&self) -> Result<(), CredentialVaultError> {
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

    fn load_file_record(&self, record_id: &str) -> Result<SecretBytes, CredentialVaultError> {
        self.file_vault(false)?
            .ok_or(CredentialVaultError::NotFound)?
            .load(record_id)
    }

    fn load_or_store_file_record(
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

    fn store_file_record(&self, record_id: &str, value: &[u8]) -> Result<(), CredentialVaultError> {
        self.file_vault(true)?
            .ok_or(CredentialVaultError::Backend)?
            .store(record_id, value)
    }

    fn delete_file_record(&self, record_id: &str) -> Result<(), CredentialVaultError> {
        self.file_vault(false)?
            .ok_or(CredentialVaultError::NotFound)?
            .delete(record_id)
    }

    fn remove_empty_file_vault(&self) -> Result<(), CredentialVaultError> {
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
        Ok(())
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
        Ok(())
    } else {
        Err(CredentialVaultError::Corrupt)
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

#[cfg(test)]
#[path = "linux_tests.rs"]
mod tests;
