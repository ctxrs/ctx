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

use super::{validate_record_id, CredentialVaultBackend, CredentialVaultError, SecretBytes};

const SERVICE: &str = "com.ctx.pro.credentials.v1";
const ACCOUNT_PREFIX: &str = "record-v1-";
const LOCK_FILE: &str = ".ctx-commercial-vault-v1.lock";
// Keep the public cross-platform contract inside Credential Manager's native
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

pub(super) struct PlatformBackend<B = SecurityFrameworkBackend> {
    backend: B,
}

impl PlatformBackend<SecurityFrameworkBackend> {
    pub(super) const fn production() -> Self {
        Self {
            backend: SecurityFrameworkBackend { service: SERVICE },
        }
    }
}

impl<B> PlatformBackend<B> {
    #[cfg(test)]
    const fn new(backend: B) -> Self {
        Self { backend }
    }
}

impl<B: KeychainBackend> CredentialVaultBackend for PlatformBackend<B> {
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
        ERR_SEC_NOT_AVAILABLE | ERR_SEC_NO_SUCH_KEYCHAIN | ERR_SEC_NO_DEFAULT_KEYCHAIN => {
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
            state.secret = Some(secret.to_vec());
            state.writes += 1;
            Ok(())
        }

        fn delete_secret(&self, _account: &str) -> Result<(), CredentialVaultError> {
            let mut state = self
                .state
                .lock()
                .map_err(|_| CredentialVaultError::Backend)?;
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
        let vault = PlatformBackend::new(MockBackend::default());
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
        let vault = PlatformBackend::new(MockBackend::default());
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
        let vault = Arc::new(PlatformBackend::new(MockBackend::default()));
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
            let result = PlatformBackend::new(backend).load(TEST_RECORD_ID);
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
    fn status_codes_map_to_sanitized_types() {
        assert!(matches!(
            map_keychain_error(SecurityFrameworkError::from_code(ERR_SEC_ITEM_NOT_FOUND)),
            CredentialVaultError::NotFound
        ));
        assert!(matches!(
            map_keychain_error(SecurityFrameworkError::from_code(ERR_SEC_DUPLICATE_ITEM)),
            CredentialVaultError::Ambiguous
        ));
        assert!(matches!(
            map_keychain_error(SecurityFrameworkError::from_code(ERR_SEC_AUTH_FAILED)),
            CredentialVaultError::Locked
        ));
        assert!(matches!(
            map_keychain_error(SecurityFrameworkError::from_code(ERR_SEC_NOT_AVAILABLE)),
            CredentialVaultError::Unavailable { platform: "macos" }
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
