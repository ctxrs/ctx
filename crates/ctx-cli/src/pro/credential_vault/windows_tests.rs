use std::process::Command;
use std::sync::{Arc, Barrier, Mutex};
use std::thread;

use super::*;

const TEST_RECORD_ID: &str = "cv2-0000000000000000000000000000000000000000000000000000000000000000";

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
fn fallback_rejects_acl_hardlink_and_junction_tampering() -> Result<(), Box<dyn std::error::Error>>
{
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
