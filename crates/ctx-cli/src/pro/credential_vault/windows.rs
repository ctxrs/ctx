#![allow(unsafe_code)]

use std::{
    fs::File,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
};

use fs2::FileExt as _;
use sha2::{Digest, Sha256};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_NOT_FOUND,
    ERROR_NO_SUCH_LOGON_SESSION, FILETIME, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::Credentials::{
    CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_MAX_CREDENTIAL_BLOB_SIZE,
    CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
};
use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use zeroize::Zeroize;

use super::windows_file::{BackendSelection, VaultRoot};
use super::{validate_record_id, CredentialVaultBackend, CredentialVaultError, SecretBytes};

const TARGET_PREFIX: &str = "ctx-pro/credentials/v1/";
const MUTEX_PREFIX: &str = "Local\\ctx-pro-credentials-v1-";
const USER_NAME: &str = "ctx-pro";
const LOCK_WAIT_MILLIS: u32 = 30_000;
const MAX_SECRET_BYTES: usize = CRED_MAX_CREDENTIAL_BLOB_SIZE as usize;
const PROBE_TARGET: &str = "ctx-pro/credentials/v1/probe";
const CREDENTIAL_MANAGER_SELECTION: &[u8] = b"ctx-pro-credential-backend-v1:credential-manager\n";

pub(super) struct PlatformBackend(WindowsBackend<CredentialManagerBackend>);

impl PlatformBackend {
    pub(super) fn production(data_root: &Path) -> Self {
        Self(WindowsBackend::new(data_root, CredentialManagerBackend))
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

    fn store(&self, record_id: &str, secret: &[u8]) -> Result<(), CredentialVaultError> {
        self.0.store(record_id, secret)
    }

    fn delete(&self, record_id: &str) -> Result<(), CredentialVaultError> {
        self.0.delete(record_id)
    }
}

struct WindowsBackend<B> {
    data_root: PathBuf,
    native: NativeBackend<B>,
}

impl<B> WindowsBackend<B> {
    fn new(data_root: &Path, backend: B) -> Self {
        Self {
            data_root: data_root.to_path_buf(),
            native: NativeBackend::new(backend),
        }
    }
}

impl<B: CredentialBackend> WindowsBackend<B> {
    fn inspect_unselected_native<T>(
        &self,
        operation: impl FnOnce(&NativeBackend<B>) -> Result<T, CredentialVaultError>,
    ) -> Result<T, CredentialVaultError> {
        match self.native.probe() {
            Ok(()) => operation(&self.native),
            Err(CredentialVaultError::Unavailable {
                platform: "windows",
            }) => Err(CredentialVaultError::NotFound),
            Err(error) => Err(error),
        }
    }

    fn with_mutating_backend<T>(
        &self,
        operation: impl FnOnce(
            BackendSelection,
            &VaultRoot,
            &NativeBackend<B>,
        ) -> Result<T, CredentialVaultError>,
    ) -> Result<T, CredentialVaultError> {
        let root = VaultRoot::open(&self.data_root, CREDENTIAL_MANAGER_SELECTION)?;
        let lock = root.open_lock()?;
        lock.lock_exclusive()
            .map_err(|_| CredentialVaultError::Backend)?;
        let result = (|| {
            let selection = match root.read_selection()? {
                Some(selection) => selection,
                None => {
                    root.validate_unselected_file_state()?;
                    match self.native.probe() {
                        Ok(()) => {
                            root.write_selection(BackendSelection::Native)?;
                            BackendSelection::Native
                        }
                        Err(
                            error @ CredentialVaultError::Unavailable {
                                platform: "windows",
                            },
                        ) if root.preexisting_sensitive_state()? => return Err(error),
                        Err(CredentialVaultError::Unavailable {
                            platform: "windows",
                        }) => {
                            root.write_selection(BackendSelection::File)?;
                            BackendSelection::File
                        }
                        Err(error) => return Err(error),
                    }
                }
            };
            operation(selection, &root, &self.native)
        })();
        let unlock = file_unlock(&lock);
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
            &NativeBackend<B>,
        ) -> Result<T, CredentialVaultError>,
    ) -> Result<T, CredentialVaultError> {
        let root = VaultRoot::open(&self.data_root, CREDENTIAL_MANAGER_SELECTION)?;
        let initial_selection = root.read_selection()?;
        let Some(lock) = root.open_existing_lock()? else {
            return if initial_selection.is_some() {
                Err(CredentialVaultError::Corrupt)
            } else {
                root.validate_unselected_file_state()?;
                operation(None, &root, &self.native)
            };
        };
        lock.lock_exclusive()
            .map_err(|_| CredentialVaultError::Backend)?;
        let result = (|| {
            let selection = root.read_selection()?;
            if selection.is_none() {
                root.validate_unselected_file_state()?;
            }
            operation(selection, &root, &self.native)
        })();
        let unlock = file_unlock(&lock);
        match (result, unlock) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }

    fn cleanup_if_empty(&self) -> Result<(), CredentialVaultError> {
        let root = match VaultRoot::open(&self.data_root, CREDENTIAL_MANAGER_SELECTION) {
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
            .map_err(|_| CredentialVaultError::Backend)?;
        let result = (|| {
            let Some(selection) = root.read_selection()? else {
                root.remove_marker_stage()?;
                return Ok(());
            };
            match selection {
                BackendSelection::File => root.remove_empty_file_vault()?,
                BackendSelection::Native => self.native.probe()?,
            }
            root.remove_selection()
        })();
        let unlock = file_unlock(&lock);
        match (result, unlock) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), _) | (Ok(()), Err(error)) => Err(error),
        }
    }
}

impl<B: CredentialBackend> CredentialVaultBackend for WindowsBackend<B> {
    fn load(&self, record_id: &str) -> Result<SecretBytes, CredentialVaultError> {
        validate_record_id(record_id)?;
        self.with_read_backend(|selection, root, native| match selection {
            Some(BackendSelection::File) => root.load_file_record(record_id),
            Some(BackendSelection::Native) => native.load(record_id),
            None => self.inspect_unselected_native(|native| native.load(record_id)),
        })
    }

    fn load_or_store(
        &self,
        record_id: &str,
        candidate: &[u8],
    ) -> Result<SecretBytes, CredentialVaultError> {
        validate_record_id(record_id)?;
        drop(SecretBytes::new(candidate.to_vec())?);
        self.with_mutating_backend(|selection, root, native| match selection {
            BackendSelection::File => root.load_or_store_file_record(record_id, candidate),
            BackendSelection::Native => native.load_or_store(record_id, candidate),
        })
    }

    fn store(&self, record_id: &str, secret: &[u8]) -> Result<(), CredentialVaultError> {
        validate_record_id(record_id)?;
        drop(SecretBytes::new(secret.to_vec())?);
        self.with_mutating_backend(|selection, root, native| match selection {
            BackendSelection::File => root.store_file_record(record_id, secret),
            BackendSelection::Native => native.store(record_id, secret),
        })
    }

    fn delete(&self, record_id: &str) -> Result<(), CredentialVaultError> {
        validate_record_id(record_id)?;
        self.with_read_backend(|selection, root, native| match selection {
            Some(BackendSelection::File) => root.delete_file_record(record_id),
            Some(BackendSelection::Native) => native.delete(record_id),
            None => self.inspect_unselected_native(|native| native.delete(record_id)),
        })
    }
}

fn file_unlock(file: &File) -> Result<(), CredentialVaultError> {
    file.unlock().map_err(|_| CredentialVaultError::Backend)
}

struct NativeBackend<B = CredentialManagerBackend> {
    backend: B,
}

impl NativeBackend<CredentialManagerBackend> {
    const fn production() -> Self {
        Self {
            backend: CredentialManagerBackend,
        }
    }
}

impl<B> NativeBackend<B> {
    const fn new(backend: B) -> Self {
        Self { backend }
    }
}

impl<B: CredentialBackend> NativeBackend<B> {
    fn probe(&self) -> Result<(), CredentialVaultError> {
        match self.backend.load_secret(PROBE_TARGET) {
            Ok(mut secret) => {
                secret.zeroize();
                Ok(())
            }
            Err(CredentialVaultError::NotFound) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

impl<B: CredentialBackend> CredentialVaultBackend for NativeBackend<B> {
    fn load(&self, record_id: &str) -> Result<SecretBytes, CredentialVaultError> {
        validate_record_id(record_id)?;
        let target = credential_target(record_id);
        bounded_secret(self.backend.load_secret(&target)?)
    }

    fn load_or_store(
        &self,
        record_id: &str,
        candidate: &[u8],
    ) -> Result<SecretBytes, CredentialVaultError> {
        validate_record_id(record_id)?;
        check_secret_size(candidate.len())?;
        let target = credential_target(record_id);
        self.backend
            .with_write_lock(&target, || match self.backend.load_secret(&target) {
                Ok(secret) => bounded_secret(secret),
                Err(CredentialVaultError::NotFound) => {
                    self.backend.store_secret(&target, candidate)?;
                    bounded_secret(self.backend.load_secret(&target)?)
                }
                Err(error) => Err(error),
            })
    }

    fn store(&self, record_id: &str, secret: &[u8]) -> Result<(), CredentialVaultError> {
        validate_record_id(record_id)?;
        check_secret_size(secret.len())?;
        let target = credential_target(record_id);
        self.backend
            .with_write_lock(&target, || self.backend.store_secret(&target, secret))
    }

    fn delete(&self, record_id: &str) -> Result<(), CredentialVaultError> {
        validate_record_id(record_id)?;
        let target = credential_target(record_id);
        self.backend
            .with_write_lock(&target, || self.backend.delete_secret(&target))
    }
}

trait CredentialBackend: Send + Sync {
    fn load_secret(&self, target: &str) -> Result<Vec<u8>, CredentialVaultError>;
    fn store_secret(&self, target: &str, secret: &[u8]) -> Result<(), CredentialVaultError>;
    fn delete_secret(&self, target: &str) -> Result<(), CredentialVaultError>;
    fn with_write_lock<T>(
        &self,
        target: &str,
        operation: impl FnOnce() -> Result<T, CredentialVaultError>,
    ) -> Result<T, CredentialVaultError>;
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CredentialManagerBackend;

impl CredentialBackend for CredentialManagerBackend {
    fn load_secret(&self, target: &str) -> Result<Vec<u8>, CredentialVaultError> {
        let target = wide_string(target);
        let mut raw = null_mut();
        // SAFETY: `target` is NUL-terminated for the call and `raw` is a valid
        // out pointer. The returned allocation is immediately guarded.
        if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &raw mut raw) } == 0 {
            return Err(map_read_error(last_error()));
        }
        CredentialBuffer::new(raw)?.copy_blob()
    }

    fn store_secret(&self, target: &str, secret: &[u8]) -> Result<(), CredentialVaultError> {
        check_secret_size(secret.len())?;
        let mut target = wide_string(target);
        let mut user_name = wide_string(USER_NAME);
        let blob_size = u32::try_from(secret.len()).map_err(|_| size_error(secret.len()))?;
        let credential = CREDENTIALW {
            Flags: 0,
            Type: CRED_TYPE_GENERIC,
            TargetName: target.as_mut_ptr(),
            Comment: null_mut(),
            LastWritten: FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            },
            CredentialBlobSize: blob_size,
            CredentialBlob: secret.as_ptr().cast_mut(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            AttributeCount: 0,
            Attributes: null_mut(),
            TargetAlias: null_mut(),
            UserName: user_name.as_mut_ptr(),
        };
        // SAFETY: all pointers remain live for this synchronous call and the
        // byte count matches the caller-owned secret.
        if unsafe { CredWriteW(&raw const credential, 0) } == 0 {
            return Err(map_operation_error(last_error()));
        }
        Ok(())
    }

    fn delete_secret(&self, target: &str) -> Result<(), CredentialVaultError> {
        let target = wide_string(target);
        // SAFETY: `target` is valid NUL-terminated UTF-16 for the call.
        if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
            return Err(map_delete_error(last_error()));
        }
        Ok(())
    }

    fn with_write_lock<T>(
        &self,
        target: &str,
        operation: impl FnOnce() -> Result<T, CredentialVaultError>,
    ) -> Result<T, CredentialVaultError> {
        let name = wide_string(&mutex_name(target));
        // SAFETY: default security attributes are requested and `name` is
        // NUL-terminated for the duration of the call.
        let handle = unsafe { CreateMutexW(null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(map_operation_error(last_error()));
        }
        let mut mutex = MutexHandle::new(handle);
        // SAFETY: `handle` is a live mutex handle owned by `mutex`.
        let wait = unsafe { WaitForSingleObject(handle, LOCK_WAIT_MILLIS) };
        if !matches!(wait, WAIT_OBJECT_0 | WAIT_ABANDONED) {
            return Err(CredentialVaultError::Backend);
        }
        mutex.owned = true;
        let result = operation();
        let release = mutex.release();
        match (result, release) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }
}

struct CredentialBuffer {
    raw: *mut CREDENTIALW,
}

impl CredentialBuffer {
    fn new(raw: *mut CREDENTIALW) -> Result<Self, CredentialVaultError> {
        if raw.is_null() {
            Err(CredentialVaultError::Backend)
        } else {
            Ok(Self { raw })
        }
    }

    fn copy_blob(&self) -> Result<Vec<u8>, CredentialVaultError> {
        // SAFETY: this is a non-null CredReadW allocation held by this guard.
        let credential = unsafe { &*self.raw };
        let size = usize::try_from(credential.CredentialBlobSize)
            .map_err(|_| CredentialVaultError::Backend)?;
        check_secret_size(size)?;
        if size == 0 {
            return Ok(Vec::new());
        }
        if credential.CredentialBlob.is_null() {
            return Err(CredentialVaultError::Backend);
        }
        // SAFETY: Credential Manager returned `size` bytes in the still-live
        // allocation and the documented native maximum bounds the slice.
        Ok(unsafe { std::slice::from_raw_parts(credential.CredentialBlob, size) }.to_vec())
    }
}

impl Drop for CredentialBuffer {
    fn drop(&mut self) {
        // SAFETY: this guard owns the CredReadW allocation. Clear a valid,
        // bounded native blob before releasing the allocation exactly once.
        unsafe {
            let credential = &mut *self.raw;
            if !credential.CredentialBlob.is_null()
                && credential.CredentialBlobSize <= CRED_MAX_CREDENTIAL_BLOB_SIZE
            {
                std::slice::from_raw_parts_mut(
                    credential.CredentialBlob,
                    credential.CredentialBlobSize as usize,
                )
                .zeroize();
            }
            CredFree(self.raw.cast());
        }
    }
}

struct MutexHandle {
    handle: HANDLE,
    owned: bool,
}

impl MutexHandle {
    const fn new(handle: HANDLE) -> Self {
        Self {
            handle,
            owned: false,
        }
    }

    fn release(&mut self) -> Result<(), CredentialVaultError> {
        if self.owned {
            // SAFETY: this guard owns the mutex after a successful wait.
            if unsafe { ReleaseMutex(self.handle) } == 0 {
                return Err(CredentialVaultError::Backend);
            }
            self.owned = false;
        }
        Ok(())
    }
}

impl Drop for MutexHandle {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: best-effort release of the mutex owned by this guard.
            unsafe { ReleaseMutex(self.handle) };
        }
        // SAFETY: this guard owns one live handle and closes it exactly once.
        unsafe { CloseHandle(self.handle) };
    }
}

fn credential_target(record_id: &str) -> String {
    format!("{TARGET_PREFIX}{:x}", Sha256::digest(record_id.as_bytes()))
}

fn mutex_name(target: &str) -> String {
    format!("{MUTEX_PREFIX}{:x}", Sha256::digest(target.as_bytes()))
}

fn wide_string(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn bounded_secret(mut secret: Vec<u8>) -> Result<SecretBytes, CredentialVaultError> {
    if let Err(error) = check_secret_size(secret.len()) {
        secret.zeroize();
        return Err(error);
    }
    SecretBytes::new(secret)
}

fn last_error() -> u32 {
    // SAFETY: GetLastError has no preconditions and is called immediately after
    // a failed Win32 API operation on the same thread.
    unsafe { GetLastError() }
}

const fn size_error(actual: usize) -> CredentialVaultError {
    CredentialVaultError::SecretTooLarge {
        max: MAX_SECRET_BYTES,
        actual,
    }
}

const fn check_secret_size(actual: usize) -> Result<(), CredentialVaultError> {
    if actual > MAX_SECRET_BYTES {
        Err(size_error(actual))
    } else {
        Ok(())
    }
}

const fn map_read_error(code: u32) -> CredentialVaultError {
    if code == ERROR_NOT_FOUND {
        CredentialVaultError::NotFound
    } else {
        map_operation_error(code)
    }
}

const fn map_delete_error(code: u32) -> CredentialVaultError {
    if code == ERROR_NOT_FOUND {
        CredentialVaultError::NotFound
    } else {
        map_operation_error(code)
    }
}

const fn map_operation_error(code: u32) -> CredentialVaultError {
    match code {
        ERROR_ACCESS_DENIED => CredentialVaultError::Locked,
        ERROR_NO_SUCH_LOGON_SESSION => CredentialVaultError::Unavailable {
            platform: "windows",
        },
        ERROR_ALREADY_EXISTS => CredentialVaultError::Ambiguous,
        _ => CredentialVaultError::Backend,
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::sync::{Arc, Barrier, Mutex};
    use std::thread;

    use super::*;

    const TEST_RECORD_ID: &str =
        "cv2-0000000000000000000000000000000000000000000000000000000000000000";

    #[derive(Debug, Clone, Copy)]
    enum ForcedError {
        Locked,
        Unavailable,
    }

    #[derive(Debug, Default)]
    struct MockState {
        secret: Option<Vec<u8>>,
        error: Option<ForcedError>,
        writes: usize,
        locks: usize,
    }

    #[derive(Debug, Default)]
    struct MockBackend {
        state: Mutex<MockState>,
        write_lock: Mutex<()>,
    }

    impl CredentialBackend for MockBackend {
        fn load_secret(&self, _target: &str) -> Result<Vec<u8>, CredentialVaultError> {
            let state = self
                .state
                .lock()
                .map_err(|_| CredentialVaultError::Backend)?;
            match state.error {
                Some(ForcedError::Locked) => return Err(CredentialVaultError::Locked),
                Some(ForcedError::Unavailable) => {
                    return Err(CredentialVaultError::Unavailable {
                        platform: "windows",
                    });
                }
                None => {}
            }
            state.secret.clone().ok_or(CredentialVaultError::NotFound)
        }

        fn store_secret(&self, _target: &str, secret: &[u8]) -> Result<(), CredentialVaultError> {
            let mut state = self
                .state
                .lock()
                .map_err(|_| CredentialVaultError::Backend)?;
            match state.error {
                Some(ForcedError::Locked) => return Err(CredentialVaultError::Locked),
                Some(ForcedError::Unavailable) => {
                    return Err(CredentialVaultError::Unavailable {
                        platform: "windows",
                    });
                }
                None => {}
            }
            state.secret = Some(secret.to_vec());
            state.writes += 1;
            Ok(())
        }

        fn delete_secret(&self, _target: &str) -> Result<(), CredentialVaultError> {
            let mut state = self
                .state
                .lock()
                .map_err(|_| CredentialVaultError::Backend)?;
            match state.error {
                Some(ForcedError::Locked) => return Err(CredentialVaultError::Locked),
                Some(ForcedError::Unavailable) => {
                    return Err(CredentialVaultError::Unavailable {
                        platform: "windows",
                    });
                }
                None => {}
            }
            let mut removed = state.secret.take().ok_or(CredentialVaultError::NotFound)?;
            removed.zeroize();
            state.writes += 1;
            Ok(())
        }

        fn with_write_lock<T>(
            &self,
            _target: &str,
            operation: impl FnOnce() -> Result<T, CredentialVaultError>,
        ) -> Result<T, CredentialVaultError> {
            let _guard = self
                .write_lock
                .lock()
                .map_err(|_| CredentialVaultError::Backend)?;
            self.state
                .lock()
                .map_err(|_| CredentialVaultError::Backend)?
                .locks += 1;
            operation()
        }
    }

    fn private_root() -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        ctx_history_core::platform_security::restrict_private_directory(root.path())?;
        let pro = root.path().join("pro");
        std::fs::create_dir(&pro)?;
        ctx_history_core::platform_security::restrict_private_directory(&pro)?;
        Ok(root)
    }

    fn force_error(
        backend: &MockBackend,
        error: Option<ForcedError>,
    ) -> Result<(), CredentialVaultError> {
        backend
            .state
            .lock()
            .map_err(|_| CredentialVaultError::Backend)?
            .error = error;
        Ok(())
    }

    fn selector_marker(root: &Path) -> PathBuf {
        root.join("pro").join(".ctx-pro.credential-backend-v1")
    }

    fn fallback_record(root: &Path) -> PathBuf {
        root.join("pro")
            .join(".ctx-pro.credentials-v1")
            .join(TEST_RECORD_ID)
    }

    #[test]
    fn target_and_mutex_are_stable_bounded_and_opaque() {
        let id = "workos-session:alice@example.test";
        let target = credential_target(id);
        assert_eq!(target, credential_target(id));
        assert_eq!(target.len(), TARGET_PREFIX.len() + 64);
        assert!(!target.contains(id));
        assert!(!target.contains("alice"));
        let mutex = mutex_name(&target);
        assert_eq!(mutex.len(), MUTEX_PREFIX.len() + 64);
        assert!(!mutex.contains(&target));
    }

    #[test]
    fn mock_round_trip_delete_and_write_lock() -> Result<(), CredentialVaultError> {
        let vault = NativeBackend::new(MockBackend::default());
        vault.store(TEST_RECORD_ID, b"private")?;
        assert_eq!(vault.load(TEST_RECORD_ID)?.as_slice(), b"private");
        vault.delete(TEST_RECORD_ID)?;
        assert!(matches!(
            vault.load(TEST_RECORD_ID),
            Err(CredentialVaultError::NotFound)
        ));
        let state = vault
            .backend
            .state
            .lock()
            .map_err(|_| CredentialVaultError::Backend)?;
        assert_eq!((state.writes, state.locks), (2, 2));
        Ok(())
    }

    #[test]
    fn oversize_secret_is_rejected_before_native_write() {
        let vault = NativeBackend::new(MockBackend::default());
        assert!(matches!(
            vault.store(TEST_RECORD_ID, &vec![7; MAX_SECRET_BYTES + 1]),
            Err(CredentialVaultError::SecretTooLarge { .. })
        ));
        assert!(vault
            .backend
            .state
            .lock()
            .is_ok_and(|state| state.writes == 0));
    }

    #[test]
    fn concurrent_first_use_returns_one_winner() -> Result<(), CredentialVaultError> {
        let vault = Arc::new(NativeBackend::new(MockBackend::default()));
        let barrier = Arc::new(Barrier::new(2));
        let mut workers = Vec::new();
        for candidate in [b"first".to_vec(), b"second".to_vec()] {
            let vault = Arc::clone(&vault);
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || {
                barrier.wait();
                vault.load_or_store(TEST_RECORD_ID, &candidate)
            }));
        }
        let first = workers
            .remove(0)
            .join()
            .map_err(|_| CredentialVaultError::Backend)??;
        let second = workers
            .remove(0)
            .join()
            .map_err(|_| CredentialVaultError::Backend)??;
        assert_eq!(first.as_slice(), second.as_slice());
        let state = vault
            .backend
            .state
            .lock()
            .map_err(|_| CredentialVaultError::Backend)?;
        assert_eq!((state.writes, state.locks), (1, 2));
        Ok(())
    }

    #[test]
    fn pristine_read_only_unavailable_inspection_creates_nothing(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = private_root()?;
        let backend = MockBackend::default();
        force_error(&backend, Some(ForcedError::Unavailable))?;
        let vault = WindowsBackend::new(root.path(), backend);

        assert!(matches!(
            vault.load(TEST_RECORD_ID),
            Err(CredentialVaultError::NotFound)
        ));
        assert!(matches!(
            vault.delete(TEST_RECORD_ID),
            Err(CredentialVaultError::NotFound)
        ));
        assert!(!selector_marker(root.path()).exists());
        assert!(!root
            .path()
            .join("pro/.ctx-pro.credentials-v1.lock")
            .exists());
        assert!(!root.path().join("pro/.ctx-pro.credentials-v1").exists());
        Ok(())
    }

    #[test]
    fn unavailable_fresh_root_selects_sticky_file_backend_and_cleans_up(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = private_root()?;
        let backend = MockBackend::default();
        force_error(&backend, Some(ForcedError::Unavailable))?;
        let first = WindowsBackend::new(root.path(), backend);
        first.store(TEST_RECORD_ID, b"fallback-secret")?;
        assert_eq!(
            std::fs::read(selector_marker(root.path()))?,
            super::super::windows_file::FILE_SELECTION
        );
        assert_eq!(first.load(TEST_RECORD_ID)?.as_slice(), b"fallback-secret");

        let restarted = WindowsBackend::new(root.path(), MockBackend::default());
        assert_eq!(
            restarted.load(TEST_RECORD_ID)?.as_slice(),
            b"fallback-secret"
        );
        restarted.delete(TEST_RECORD_ID)?;
        assert!(matches!(
            restarted.load(TEST_RECORD_ID),
            Err(CredentialVaultError::NotFound)
        ));
        restarted.cleanup_if_empty()?;
        assert!(!selector_marker(root.path()).exists());
        assert!(!root.path().join("pro/.ctx-pro.credentials-v1").exists());
        Ok(())
    }

    #[test]
    fn native_selection_is_sticky_and_never_downgrades() -> Result<(), Box<dyn std::error::Error>> {
        let root = private_root()?;
        let vault = WindowsBackend::new(root.path(), MockBackend::default());
        vault.store(TEST_RECORD_ID, b"native")?;
        assert_eq!(
            std::fs::read(selector_marker(root.path()))?,
            CREDENTIAL_MANAGER_SELECTION
        );

        force_error(&vault.native.backend, Some(ForcedError::Unavailable))?;
        assert!(matches!(
            vault.store(TEST_RECORD_ID, b"must-not-fallback"),
            Err(CredentialVaultError::Unavailable {
                platform: "windows"
            })
        ));
        assert_eq!(
            std::fs::read(selector_marker(root.path()))?,
            CREDENTIAL_MANAGER_SELECTION
        );
        assert!(!root.path().join("pro/.ctx-pro.credentials-v1").exists());
        Ok(())
    }

    #[test]
    fn locked_or_preexisting_sensitive_state_never_selects_fallback(
    ) -> Result<(), Box<dyn std::error::Error>> {
        for preexisting in [false, true] {
            let root = private_root()?;
            if preexisting {
                let graph_store = root.path().join("pro/.ctx-pro-key-store-v1");
                std::fs::create_dir(&graph_store)?;
                ctx_history_core::platform_security::restrict_private_directory(&graph_store)?;
            }
            let backend = MockBackend::default();
            force_error(
                &backend,
                Some(if preexisting {
                    ForcedError::Unavailable
                } else {
                    ForcedError::Locked
                }),
            )?;
            let vault = WindowsBackend::new(root.path(), backend);
            let result = vault.store(TEST_RECORD_ID, b"secret");
            if preexisting {
                assert!(matches!(
                    result,
                    Err(CredentialVaultError::Unavailable {
                        platform: "windows"
                    })
                ));
            } else {
                assert!(matches!(result, Err(CredentialVaultError::Locked)));
            }
            assert!(!selector_marker(root.path()).exists());
            assert!(!root.path().join("pro/.ctx-pro.credentials-v1").exists());
        }
        Ok(())
    }

    #[test]
    fn markerless_file_vault_and_selector_temp_fail_closed_before_native_probe(
    ) -> Result<(), Box<dyn std::error::Error>> {
        for orphan_vault in [true, false] {
            let root = private_root()?;
            let path = if orphan_vault {
                root.path().join("pro/.ctx-pro.credentials-v1")
            } else {
                root.path().join("pro/.ctx-pro.credential-backend-v1.next")
            };
            if orphan_vault {
                std::fs::create_dir(&path)?;
                ctx_history_core::platform_security::restrict_private_directory(&path)?;
            } else {
                std::fs::write(&path, b"interrupted")?;
                ctx_history_core::platform_security::restrict_private_file(&path)?;
            }
            let vault = WindowsBackend::new(root.path(), MockBackend::default());
            assert!(matches!(
                vault.store(TEST_RECORD_ID, b"secret"),
                Err(CredentialVaultError::Corrupt)
            ));
            assert!(!selector_marker(root.path()).exists());
        }
        Ok(())
    }

    #[test]
    fn fallback_rejects_acl_hardlink_and_junction_tampering(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = private_root()?;
        let backend = MockBackend::default();
        force_error(&backend, Some(ForcedError::Unavailable))?;
        let vault = WindowsBackend::new(root.path(), backend);
        vault.store(TEST_RECORD_ID, b"secret")?;

        let record = fallback_record(root.path());
        let hardlink = root.path().join("unexpected-record-link");
        std::fs::hard_link(&record, &hardlink)?;
        assert!(matches!(
            vault.load(TEST_RECORD_ID),
            Err(CredentialVaultError::Corrupt)
        ));
        std::fs::remove_file(hardlink)?;

        let file_vault = root.path().join("pro/.ctx-pro.credentials-v1");
        ctx_history_core::platform_security::restrict_private_directory(&file_vault)?;
        assert!(matches!(
            vault.load(TEST_RECORD_ID),
            Err(CredentialVaultError::Corrupt)
        ));

        let clean_root = private_root()?;
        let clean_backend = MockBackend::default();
        force_error(&clean_backend, Some(ForcedError::Unavailable))?;
        let clean_vault = WindowsBackend::new(clean_root.path(), clean_backend);
        clean_vault.store(TEST_RECORD_ID, b"secret")?;
        let records = clean_root.path().join("pro/.ctx-pro.credentials-v1");
        let displaced = clean_root.path().join("displaced-records");
        std::fs::rename(&records, &displaced)?;
        let status = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&records)
            .arg(&displaced)
            .status()?;
        assert!(status.success(), "failed to create test junction");
        assert!(matches!(
            clean_vault.load(TEST_RECORD_ID),
            Err(CredentialVaultError::Corrupt)
        ));
        std::fs::remove_dir(&records)?;
        std::fs::rename(displaced, records)?;
        Ok(())
    }

    #[test]
    fn fallback_concurrent_first_use_returns_one_persisted_candidate(
    ) -> Result<(), Box<dyn std::error::Error>> {
        const WORKERS: usize = 8;

        let root = private_root()?;
        let backend = MockBackend::default();
        force_error(&backend, Some(ForcedError::Unavailable))?;
        let vault = Arc::new(WindowsBackend::new(root.path(), backend));
        let barrier = Arc::new(Barrier::new(WORKERS));
        let mut workers = Vec::with_capacity(WORKERS);
        for candidate in 0..WORKERS as u8 {
            let vault = Arc::clone(&vault);
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || {
                barrier.wait();
                vault
                    .load_or_store(TEST_RECORD_ID, &[candidate; 32])
                    .map(|secret| secret.as_slice().to_vec())
            }));
        }
        let mut persisted = None;
        for worker in workers {
            let candidate = worker.join().map_err(|_| CredentialVaultError::Backend)??;
            if let Some(expected) = &persisted {
                assert_eq!(&candidate, expected);
            } else {
                persisted = Some(candidate);
            }
        }
        assert!(persisted.is_some());
        Ok(())
    }

    #[test]
    fn win32_errors_map_to_sanitized_types() {
        assert!(matches!(
            map_read_error(ERROR_NOT_FOUND),
            CredentialVaultError::NotFound
        ));
        assert!(matches!(
            map_operation_error(ERROR_ACCESS_DENIED),
            CredentialVaultError::Locked
        ));
        assert!(matches!(
            map_operation_error(ERROR_NO_SUCH_LOGON_SESSION),
            CredentialVaultError::Unavailable {
                platform: "windows"
            }
        ));
        assert!(matches!(
            map_operation_error(ERROR_ALREADY_EXISTS),
            CredentialVaultError::Ambiguous
        ));
    }
}
