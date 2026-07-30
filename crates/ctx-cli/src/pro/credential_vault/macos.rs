//! macOS Keychain Services credential-vault backend.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use security_framework::base::Error as SecurityFrameworkError;
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};
use security_framework_sys::base::{
    errSecAuthFailed as ERR_SEC_AUTH_FAILED, errSecItemNotFound as ERR_SEC_ITEM_NOT_FOUND,
};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use super::unix_file::{BackendSelection, VaultRoot, FILE_SELECTION};
use super::{validate_record_id, CredentialVaultBackend, CredentialVaultError, SecretBytes};

const SERVICE: &str = "com.ctx.pro.credentials.v1";
const ACCOUNT_PREFIX: &str = "record-v1-";
const LOCK_FILE: &str = ".ctx-commercial-vault-v1.lock";
const PROBE_ACCOUNT: &str = "ctx-pro-credential-vault-probe-v1";
const KEYCHAIN_SELECTION: &[u8] = b"ctx-pro-credential-backend-v1:keychain\n";
// Keep the public cross-platform contract inside the smallest shipped native
// limit rather than accepting a secret here that cannot be restored elsewhere.
const MAX_SECRET_BYTES: usize = 5 * 512;

// Security.framework's public OSStatus values are ABI-stable. The sys crate
// exposes only a subset of them as Rust constants.
const ERR_SEC_DUPLICATE_ITEM: i32 = -25_299;
const ERR_SEC_NOT_AVAILABLE: i32 = -25_291;
const ERR_SEC_NO_ACCESS_FOR_ITEM: i32 = -25_243;
const ERR_SEC_NO_SUCH_KEYCHAIN: i32 = -25_294;
const ERR_SEC_NO_DEFAULT_KEYCHAIN: i32 = -25_307;
const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25_308;
const ERR_SEC_INTERACTION_REQUIRED: i32 = -25_315;
const ERR_SEC_MISSING_ENTITLEMENT: i32 = -34_018;
const ERR_SEC_USER_CANCELED: i32 = -128;

pub(super) struct PlatformBackend(MacBackend<SecurityFrameworkBackend>);

impl PlatformBackend {
    pub(super) fn production(data_root: &Path) -> Self {
        Self(MacBackend::new(
            data_root,
            SecurityFrameworkBackend { service: SERVICE },
        ))
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

struct MacBackend<B> {
    data_root: PathBuf,
    native: NativeBackend<B>,
}

impl<B> MacBackend<B> {
    fn new(data_root: &Path, backend: B) -> Self {
        Self {
            data_root: data_root.to_path_buf(),
            native: NativeBackend::new(backend),
        }
    }
}

impl<B: KeychainBackend> MacBackend<B> {
    fn inspect_unselected_native<T>(
        &self,
        operation: impl FnOnce(&NativeBackend<B>) -> Result<T, CredentialVaultError>,
    ) -> Result<T, CredentialVaultError> {
        match self.native.probe() {
            Ok(()) => operation(&self.native),
            Err(CredentialVaultError::Unavailable { platform: "macos" }) => {
                Err(CredentialVaultError::NotFound)
            }
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
        let root = VaultRoot::open(&self.data_root, KEYCHAIN_SELECTION)?;
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
                        Err(error @ CredentialVaultError::Unavailable { platform: "macos" })
                            if root.preexisting_sensitive_state()? =>
                        {
                            return Err(error);
                        }
                        Err(CredentialVaultError::Unavailable { platform: "macos" }) => {
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
        let root = VaultRoot::open(&self.data_root, KEYCHAIN_SELECTION)?;
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
        let root = match VaultRoot::open(&self.data_root, KEYCHAIN_SELECTION) {
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

impl<B: KeychainBackend> CredentialVaultBackend for MacBackend<B> {
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
    FileExt::unlock(file).map_err(|_| CredentialVaultError::Backend)
}

struct NativeBackend<B = SecurityFrameworkBackend> {
    backend: B,
}

impl<B> NativeBackend<B> {
    const fn new(backend: B) -> Self {
        Self { backend }
    }
}

impl<B: KeychainBackend> NativeBackend<B> {
    fn probe(&self) -> Result<(), CredentialVaultError> {
        match self.backend.load_secret(PROBE_ACCOUNT) {
            Ok(mut secret) => {
                secret.zeroize();
                Ok(())
            }
            Err(CredentialVaultError::NotFound) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

impl<B: KeychainBackend> CredentialVaultBackend for NativeBackend<B> {
    fn load(&self, record_id: &str) -> Result<SecretBytes, CredentialVaultError> {
        validate_record_id(record_id)?;
        let account = account_name(record_id);
        bounded_secret(self.backend.load_secret(&account)?)
    }

    fn load_or_store(
        &self,
        record_id: &str,
        candidate: &[u8],
    ) -> Result<SecretBytes, CredentialVaultError> {
        validate_record_id(record_id)?;
        check_secret_size(candidate.len())?;
        let account = account_name(record_id);
        self.backend
            .with_write_lock(|| match self.backend.load_secret(&account) {
                Ok(secret) => bounded_secret(secret),
                Err(CredentialVaultError::NotFound) => {
                    self.backend.store_secret(&account, candidate)?;
                    bounded_secret(self.backend.load_secret(&account)?)
                }
                Err(error) => Err(error),
            })
    }

    fn store(&self, record_id: &str, secret: &[u8]) -> Result<(), CredentialVaultError> {
        validate_record_id(record_id)?;
        check_secret_size(secret.len())?;
        let account = account_name(record_id);
        self.backend
            .with_write_lock(|| self.backend.store_secret(&account, secret))
    }

    fn delete(&self, record_id: &str) -> Result<(), CredentialVaultError> {
        validate_record_id(record_id)?;
        let account = account_name(record_id);
        self.backend
            .with_write_lock(|| self.backend.delete_secret(&account))
    }
}

trait KeychainBackend: Send + Sync {
    fn load_secret(&self, account: &str) -> Result<Vec<u8>, CredentialVaultError>;
    fn store_secret(&self, account: &str, secret: &[u8]) -> Result<(), CredentialVaultError>;
    fn delete_secret(&self, account: &str) -> Result<(), CredentialVaultError>;
    fn with_write_lock<T>(
        &self,
        operation: impl FnOnce() -> Result<T, CredentialVaultError>,
    ) -> Result<T, CredentialVaultError>;
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SecurityFrameworkBackend {
    service: &'static str,
}

impl KeychainBackend for SecurityFrameworkBackend {
    fn load_secret(&self, account: &str) -> Result<Vec<u8>, CredentialVaultError> {
        get_generic_password(self.service, account).map_err(map_keychain_error)
    }

    fn store_secret(&self, account: &str, secret: &[u8]) -> Result<(), CredentialVaultError> {
        set_generic_password(self.service, account, secret).map_err(map_keychain_error)
    }

    fn delete_secret(&self, account: &str) -> Result<(), CredentialVaultError> {
        delete_generic_password(self.service, account).map_err(map_keychain_error)
    }

    fn with_write_lock<T>(
        &self,
        operation: impl FnOnce() -> Result<T, CredentialVaultError>,
    ) -> Result<T, CredentialVaultError> {
        let file = open_lock_file()?;
        FileExt::lock_exclusive(&file).map_err(|_| CredentialVaultError::Backend)?;
        let result = operation();
        let unlock = FileExt::unlock(&file).map_err(|_| CredentialVaultError::Backend);
        match (result, unlock) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }
}

fn account_name(record_id: &str) -> String {
    format!("{ACCOUNT_PREFIX}{:x}", Sha256::digest(record_id.as_bytes()))
}

fn bounded_secret(mut secret: Vec<u8>) -> Result<SecretBytes, CredentialVaultError> {
    if let Err(error) = check_secret_size(secret.len()) {
        secret.zeroize();
        return Err(error);
    }
    SecretBytes::new(secret)
}

const fn check_secret_size(actual: usize) -> Result<(), CredentialVaultError> {
    if actual > MAX_SECRET_BYTES {
        Err(CredentialVaultError::SecretTooLarge {
            max: MAX_SECRET_BYTES,
            actual,
        })
    } else {
        Ok(())
    }
}

fn map_keychain_error(error: SecurityFrameworkError) -> CredentialVaultError {
    match error.code() {
        ERR_SEC_ITEM_NOT_FOUND => CredentialVaultError::NotFound,
        ERR_SEC_DUPLICATE_ITEM => CredentialVaultError::Ambiguous,
        ERR_SEC_INTERACTION_NOT_ALLOWED
        | ERR_SEC_INTERACTION_REQUIRED
        | ERR_SEC_AUTH_FAILED
        | ERR_SEC_NO_ACCESS_FOR_ITEM
        | ERR_SEC_MISSING_ENTITLEMENT
        | ERR_SEC_USER_CANCELED => CredentialVaultError::Locked,
        ERR_SEC_NOT_AVAILABLE | ERR_SEC_NO_DEFAULT_KEYCHAIN => {
            CredentialVaultError::Unavailable { platform: "macos" }
        }
        _ => CredentialVaultError::Backend,
    }
}

fn lock_path() -> Result<PathBuf, CredentialVaultError> {
    let directory =
        fs::canonicalize(std::env::temp_dir()).map_err(|_| CredentialVaultError::Backend)?;
    let metadata = fs::symlink_metadata(&directory).map_err(|_| CredentialVaultError::Backend)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() || metadata.mode() & 0o077 != 0 {
        return Err(CredentialVaultError::Backend);
    }
    Ok(directory.join(LOCK_FILE))
}

fn open_lock_file() -> Result<File, CredentialVaultError> {
    open_lock_file_at(&lock_path()?)
}

fn open_lock_file_at(path: &Path) -> Result<File, CredentialVaultError> {
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            validate_lock_path(path)?;
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .map_err(|_| CredentialVaultError::Backend)?
        }
        Err(_) => return Err(CredentialVaultError::Backend),
    };
    validate_lock_path(path)?;
    let opened = file.metadata().map_err(|_| CredentialVaultError::Backend)?;
    let named = fs::symlink_metadata(path).map_err(|_| CredentialVaultError::Backend)?;
    if opened.dev() != named.dev() || opened.ino() != named.ino() {
        return Err(CredentialVaultError::Backend);
    }
    Ok(file)
}

fn validate_lock_path(path: &Path) -> Result<(), CredentialVaultError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| CredentialVaultError::Backend)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.nlink() != 1
        || metadata.mode() & 0o177 != 0
    {
        return Err(CredentialVaultError::Backend);
    }
    let parent = path.parent().ok_or(CredentialVaultError::Backend)?;
    let parent_metadata =
        fs::symlink_metadata(parent).map_err(|_| CredentialVaultError::Backend)?;
    if metadata.uid() != parent_metadata.uid() {
        return Err(CredentialVaultError::Backend);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{symlink, PermissionsExt};
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

    impl KeychainBackend for MockBackend {
        fn load_secret(&self, _account: &str) -> Result<Vec<u8>, CredentialVaultError> {
            let state = self
                .state
                .lock()
                .map_err(|_| CredentialVaultError::Backend)?;
            match state.error {
                Some(ForcedError::Locked) => Err(CredentialVaultError::Locked),
                Some(ForcedError::Unavailable) => {
                    Err(CredentialVaultError::Unavailable { platform: "macos" })
                }
                None => state.secret.clone().ok_or(CredentialVaultError::NotFound),
            }
        }

        fn store_secret(&self, _account: &str, secret: &[u8]) -> Result<(), CredentialVaultError> {
            let mut state = self
                .state
                .lock()
                .map_err(|_| CredentialVaultError::Backend)?;
            match state.error {
                Some(ForcedError::Locked) => return Err(CredentialVaultError::Locked),
                Some(ForcedError::Unavailable) => {
                    return Err(CredentialVaultError::Unavailable { platform: "macos" });
                }
                None => {}
            }
            state.secret = Some(secret.to_vec());
            state.writes += 1;
            Ok(())
        }

        fn delete_secret(&self, _account: &str) -> Result<(), CredentialVaultError> {
            let mut state = self
                .state
                .lock()
                .map_err(|_| CredentialVaultError::Backend)?;
            match state.error {
                Some(ForcedError::Locked) => return Err(CredentialVaultError::Locked),
                Some(ForcedError::Unavailable) => {
                    return Err(CredentialVaultError::Unavailable { platform: "macos" });
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
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700))?;
        let pro = root.path().join("pro");
        fs::create_dir(&pro)?;
        fs::set_permissions(pro, fs::Permissions::from_mode(0o700))?;
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
    fn account_is_stable_bounded_and_opaque() {
        let id = "workos-session:alice@example.test";
        let account = account_name(id);
        assert_eq!(account, account_name(id));
        assert_eq!(account.len(), ACCOUNT_PREFIX.len() + 64);
        assert!(!account.contains(id));
        assert!(!account.contains("alice"));
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
    fn mock_preserves_locked_and_unavailable_states() {
        for (forced, locked) in [
            (ForcedError::Locked, true),
            (ForcedError::Unavailable, false),
        ] {
            let backend = MockBackend::default();
            if let Ok(mut state) = backend.state.lock() {
                state.error = Some(forced);
            }
            let result = NativeBackend::new(backend).load(TEST_RECORD_ID);
            assert_eq!(matches!(result, Err(CredentialVaultError::Locked)), locked);
            if !locked {
                assert!(matches!(
                    result,
                    Err(CredentialVaultError::Unavailable { platform: "macos" })
                ));
            }
        }
    }

    #[test]
    fn pristine_read_only_unavailable_inspection_creates_nothing(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = private_root()?;
        let backend = MockBackend::default();
        force_error(&backend, Some(ForcedError::Unavailable))?;
        let vault = MacBackend::new(root.path(), backend);

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
        let first = MacBackend::new(root.path(), backend);
        first.store(TEST_RECORD_ID, b"fallback-secret")?;
        assert_eq!(fs::read(selector_marker(root.path()))?, FILE_SELECTION);
        assert_eq!(first.load(TEST_RECORD_ID)?.as_slice(), b"fallback-secret");

        let restarted = MacBackend::new(root.path(), MockBackend::default());
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
        let vault = MacBackend::new(root.path(), MockBackend::default());
        vault.store(TEST_RECORD_ID, b"native")?;
        assert_eq!(fs::read(selector_marker(root.path()))?, KEYCHAIN_SELECTION);

        force_error(&vault.native.backend, Some(ForcedError::Unavailable))?;
        assert!(matches!(
            vault.store(TEST_RECORD_ID, b"must-not-fallback"),
            Err(CredentialVaultError::Unavailable { platform: "macos" })
        ));
        assert_eq!(fs::read(selector_marker(root.path()))?, KEYCHAIN_SELECTION);
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
                fs::create_dir(&graph_store)?;
                fs::set_permissions(graph_store, fs::Permissions::from_mode(0o700))?;
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
            let vault = MacBackend::new(root.path(), backend);
            let result = vault.store(TEST_RECORD_ID, b"secret");
            if preexisting {
                assert!(matches!(
                    result,
                    Err(CredentialVaultError::Unavailable { platform: "macos" })
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
                fs::create_dir(&path)?;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
            } else {
                fs::write(&path, b"interrupted")?;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
            }
            let vault = MacBackend::new(root.path(), MockBackend::default());
            assert!(matches!(
                vault.store(TEST_RECORD_ID, b"secret"),
                Err(CredentialVaultError::Corrupt)
            ));
            assert!(!selector_marker(root.path()).exists());
        }
        Ok(())
    }

    #[test]
    fn fallback_layout_and_records_fail_closed_on_path_tampering(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = private_root()?;
        let backend = MockBackend::default();
        force_error(&backend, Some(ForcedError::Unavailable))?;
        let vault = MacBackend::new(root.path(), backend);
        vault.store(TEST_RECORD_ID, b"secret")?;

        for path in [
            root.path().join("pro/.ctx-pro.credentials-v1"),
            root.path().join("pro/.ctx-pro.credentials-v1.lock"),
            selector_marker(root.path()),
            fallback_record(root.path()),
        ] {
            let metadata = fs::symlink_metadata(path)?;
            if metadata.is_dir() {
                assert_eq!(metadata.mode() & 0o777, 0o700);
            } else {
                assert!(metadata.is_file());
                assert_eq!(metadata.mode() & 0o777, 0o600);
                assert_eq!(metadata.nlink(), 1);
            }
        }

        let record = fallback_record(root.path());
        let hardlink = root.path().join("unexpected-record-link");
        fs::hard_link(&record, &hardlink)?;
        assert!(matches!(
            vault.load(TEST_RECORD_ID),
            Err(CredentialVaultError::Corrupt)
        ));
        fs::remove_file(hardlink)?;

        fs::set_permissions(&record, fs::Permissions::from_mode(0o640))?;
        assert!(matches!(
            vault.load(TEST_RECORD_ID),
            Err(CredentialVaultError::Corrupt)
        ));
        fs::set_permissions(&record, fs::Permissions::from_mode(0o600))?;

        let status = Command::new("chmod")
            .args(["+a", "everyone allow read"])
            .arg(&record)
            .status()?;
        assert!(status.success(), "failed to create test extended ACL");
        assert!(matches!(
            vault.load(TEST_RECORD_ID),
            Err(CredentialVaultError::Corrupt)
        ));
        let status = Command::new("chmod").arg("-N").arg(&record).status()?;
        assert!(status.success(), "failed to clear test extended ACL");

        let records = root.path().join("pro/.ctx-pro.credentials-v1");
        let displaced = root.path().join("displaced-records");
        fs::rename(&records, &displaced)?;
        symlink(&displaced, &records)?;
        assert!(matches!(
            vault.load(TEST_RECORD_ID),
            Err(CredentialVaultError::Corrupt)
        ));
        fs::remove_file(records)?;
        fs::rename(displaced, root.path().join("pro/.ctx-pro.credentials-v1"))?;
        Ok(())
    }

    #[test]
    fn fallback_concurrent_first_use_returns_one_persisted_candidate(
    ) -> Result<(), Box<dyn std::error::Error>> {
        const WORKERS: usize = 8;

        let root = private_root()?;
        let backend = MockBackend::default();
        force_error(&backend, Some(ForcedError::Unavailable))?;
        let vault = Arc::new(MacBackend::new(root.path(), backend));
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
    fn status_codes_map_to_sanitized_types() {
        assert!(matches!(
            map_keychain_error(SecurityFrameworkError::from_code(ERR_SEC_ITEM_NOT_FOUND)),
            CredentialVaultError::NotFound
        ));
        assert!(matches!(
            map_keychain_error(SecurityFrameworkError::from_code(ERR_SEC_DUPLICATE_ITEM)),
            CredentialVaultError::Ambiguous
        ));
        for code in [
            ERR_SEC_INTERACTION_NOT_ALLOWED,
            ERR_SEC_INTERACTION_REQUIRED,
            ERR_SEC_AUTH_FAILED,
            ERR_SEC_NO_ACCESS_FOR_ITEM,
            ERR_SEC_MISSING_ENTITLEMENT,
            ERR_SEC_USER_CANCELED,
        ] {
            assert!(matches!(
                map_keychain_error(SecurityFrameworkError::from_code(code)),
                CredentialVaultError::Locked
            ));
        }
        for code in [ERR_SEC_NOT_AVAILABLE, ERR_SEC_NO_DEFAULT_KEYCHAIN] {
            assert!(matches!(
                map_keychain_error(SecurityFrameworkError::from_code(code)),
                CredentialVaultError::Unavailable { platform: "macos" }
            ));
        }
        assert!(matches!(
            map_keychain_error(SecurityFrameworkError::from_code(ERR_SEC_NO_SUCH_KEYCHAIN)),
            CredentialVaultError::Backend
        ));
    }

    #[test]
    fn lock_file_rejects_symlink_and_permissive_mode() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let target = directory.path().join("target");
        let link = directory.path().join("link");
        fs::write(&target, "not a lock")?;
        symlink(&target, &link)?;
        assert!(open_lock_file_at(&link).is_err());

        let permissive = directory.path().join("permissive");
        fs::write(&permissive, "")?;
        fs::set_permissions(&permissive, fs::Permissions::from_mode(0o640))?;
        assert!(open_lock_file_at(&permissive).is_err());
        Ok(())
    }
}
