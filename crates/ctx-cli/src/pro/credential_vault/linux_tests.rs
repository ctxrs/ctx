use std::{
    collections::{BTreeSet, HashMap},
    ffi::OsStr,
    fs::OpenOptions,
    io::{BufRead as _, BufReader, Write as _},
    os::unix::fs::{symlink, OpenOptionsExt as _},
    path::{Path, PathBuf},
    process::{Child, ChildStdout, Command, Stdio},
    sync::{Arc, Barrier, Mutex},
};

use ctx_pro_host_protocol::{
    base64url, installation_key_thumbprint, EntitlementAccessKind, EntitlementCapability,
    EntitlementGrant, SignedEntitlement, ED25519_SIGNATURE_BYTES, INSTALLATION_PUBLIC_KEY_BYTES,
};

use super::super::{
    load_record, store_record, AnonymousTrialMaterial, BoundedSignedEntitlement, CredentialRecord,
    CredentialRecordIds, CredentialRecordKind, CredentialVaultNamespace, WorkOsSessionMaterial,
    ENTITLEMENT_SCHEMA_VERSION, PROTOCOL_VERSION,
};
use super::*;

const HELPER_MODE_ENV: &str = "CTX_TEST_LINUX_CREDENTIAL_VAULT_HELPER";
const HELPER_RESULT_ENV: &str = "CTX_TEST_LINUX_CREDENTIAL_VAULT_RESULT";
const HELPER_DATA_ROOT_ENV: &str = "CTX_TEST_LINUX_CREDENTIAL_VAULT_DATA_ROOT";
const HELPER_TEST_NAME: &str =
    "pro::credential_vault::linux::tests::platform_credential_vault_subprocess_helper";

#[derive(Debug, Clone, Copy)]
enum AdapterMode {
    Available,
    Unavailable,
    Locked,
    Backend,
    Corrupt,
}

struct FakeState {
    mode: AdapterMode,
    fail_operations_unavailable: bool,
    probes: usize,
    operations: usize,
    records: HashMap<String, Vec<u8>>,
}

#[derive(Clone)]
struct FakeAdapter(Arc<Mutex<FakeState>>);

impl FakeAdapter {
    fn new(mode: AdapterMode) -> Self {
        Self(Arc::new(Mutex::new(FakeState {
            mode,
            fail_operations_unavailable: false,
            probes: 0,
            operations: 0,
            records: HashMap::new(),
        })))
    }

    fn set_mode(&self, mode: AdapterMode) {
        self.0.lock().unwrap().mode = mode;
    }

    fn set_fail_operations_unavailable(&self, value: bool) {
        self.0.lock().unwrap().fail_operations_unavailable = value;
    }

    fn counts(&self) -> (usize, usize) {
        let state = self.0.lock().unwrap();
        (state.probes, state.operations)
    }

    fn error(mode: AdapterMode) -> Result<(), CredentialVaultError> {
        match mode {
            AdapterMode::Available => Ok(()),
            AdapterMode::Unavailable => Err(CredentialVaultError::Unavailable {
                platform: "test-linux",
            }),
            AdapterMode::Locked => Err(CredentialVaultError::Locked),
            AdapterMode::Backend => Err(CredentialVaultError::Backend),
            AdapterMode::Corrupt => Err(CredentialVaultError::Corrupt),
        }
    }

    fn with_records<T>(
        &self,
        operation: impl FnOnce(&mut HashMap<String, Vec<u8>>) -> Result<T, CredentialVaultError>,
    ) -> Result<T, CredentialVaultError> {
        let mut state = self.0.lock().map_err(|_| CredentialVaultError::Backend)?;
        Self::error(state.mode)?;
        if state.fail_operations_unavailable {
            return Err(CredentialVaultError::Unavailable {
                platform: "test-linux",
            });
        }
        state.operations += 1;
        operation(&mut state.records)
    }
}

impl SecretServiceAdapter for FakeAdapter {
    fn probe(&self) -> Result<(), CredentialVaultError> {
        let mut state = self.0.lock().map_err(|_| CredentialVaultError::Backend)?;
        state.probes += 1;
        Self::error(state.mode)
    }

    fn load(&self, record_id: &str) -> Result<SecretBytes, CredentialVaultError> {
        self.with_records(|records| {
            SecretBytes::new(
                records
                    .get(record_id)
                    .cloned()
                    .ok_or(CredentialVaultError::NotFound)?,
            )
        })
    }

    fn load_or_store(
        &self,
        record_id: &str,
        candidate: &[u8],
    ) -> Result<SecretBytes, CredentialVaultError> {
        self.with_records(|records| {
            SecretBytes::new(
                records
                    .entry(record_id.to_owned())
                    .or_insert_with(|| candidate.to_vec())
                    .clone(),
            )
        })
    }

    fn store(&self, record_id: &str, value: &[u8]) -> Result<(), CredentialVaultError> {
        self.with_records(|records| {
            records.insert(record_id.to_owned(), value.to_vec());
            Ok(())
        })
    }

    fn delete(&self, record_id: &str) -> Result<(), CredentialVaultError> {
        self.with_records(|records| {
            records
                .remove(record_id)
                .map(|mut value| value.zeroize())
                .ok_or(CredentialVaultError::NotFound)
        })
    }
}

fn test_root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(
        root.path(),
        fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE),
    )
    .unwrap();
    let pro = root.path().join("pro");
    fs::create_dir(&pro).unwrap();
    fs::set_permissions(&pro, fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).unwrap();
    root
}

fn ids(namespace: CredentialVaultNamespace) -> CredentialRecordIds {
    CredentialRecordIds::new("6a1de1ab-c732-45ed-b3f8-bbf6ab1048e8", namespace).unwrap()
}

fn fixture_entitlement() -> SignedEntitlement {
    let public_key = ed25519_dalek::SigningKey::from_bytes(&[23; INSTALLATION_PUBLIC_KEY_BYTES])
        .verifying_key()
        .to_bytes();
    SignedEntitlement {
        grant: EntitlementGrant {
            schema_version: ENTITLEMENT_SCHEMA_VERSION,
            issuer: "https://commercial.ctx.rs".to_owned(),
            key_id: "key-1".to_owned(),
            grant_id: "grant-1".to_owned(),
            subject: "subject-1".to_owned(),
            account_id: "account-1".to_owned(),
            product: "ctx-local-pro".to_owned(),
            access_kind: EntitlementAccessKind::Active,
            installation_key_thumbprint: installation_key_thumbprint(&public_key),
            issued_at_unix: 1_800_000_000,
            not_before_unix: 1_799_999_700,
            refresh_after_unix: 1_800_345_600,
            access_deadline_unix: 1_802_592_000,
            grace_deadline_unix: 1_803_196_800,
            expires_at_unix: 1_800_604_800,
            minimum_helper_protocol: PROTOCOL_VERSION,
            revocation_epoch: 0,
            capabilities: BTreeSet::from([EntitlementCapability::GraphRead]),
        },
        signature_base64url: base64url(&[7; ED25519_SIGNATURE_BYTES]),
    }
}

#[test]
fn no_dbus_adapter_persists_all_public_activation_records_in_file_vault() -> anyhow::Result<()> {
    let root = test_root();
    let adapter = FakeAdapter::new(AdapterMode::Unavailable);
    let backend = LinuxBackend::new(root.path(), adapter.clone());
    let record_ids = ids(CredentialVaultNamespace::Production);

    backend.load_or_store(
        record_ids.get(CredentialRecordKind::InstallationSigningKey),
        &[23; INSTALLATION_PUBLIC_KEY_BYTES],
    )?;
    store_record(
        &backend,
        &record_ids,
        &CredentialRecord::AnonymousTrial(AnonymousTrialMaterial::new(
            "a".repeat(32),
            1_900_000_000,
        )?),
    )?;
    store_record(
        &backend,
        &record_ids,
        &CredentialRecord::SignedEntitlement(BoundedSignedEntitlement::new(fixture_entitlement())?),
    )?;
    store_record(
        &backend,
        &record_ids,
        &CredentialRecord::WorkOsSession(WorkOsSessionMaterial::new(
            "access-token".to_owned(),
            Some("refresh-token".to_owned()),
            1_900_000_000,
        )?),
    )?;

    for kind in [
        CredentialRecordKind::InstallationSigningKey,
        CredentialRecordKind::AnonymousTrial,
        CredentialRecordKind::SignedEntitlement,
        CredentialRecordKind::WorkOsSession,
    ] {
        load_record(&backend, record_ids.get(kind), kind)?;
    }
    assert_eq!(adapter.counts(), (1, 0));
    assert_eq!(
        fs::read(root.path().join("pro").join(BACKEND_MARKER))?,
        FILE_SELECTION
    );
    for file_name in [BACKEND_MARKER, FILE_VAULT_LOCK] {
        let metadata = fs::metadata(root.path().join("pro").join(file_name))?;
        assert_eq!(metadata.permissions().mode() & 0o7777, PRIVATE_FILE_MODE);
        assert_eq!(metadata.uid(), effective_uid());
        assert_eq!(metadata.nlink(), 1);
    }
    let vault = root.path().join("pro").join(FILE_VAULT_DIRECTORY);
    assert_eq!(
        fs::metadata(&vault)?.permissions().mode() & 0o7777,
        PRIVATE_DIRECTORY_MODE
    );
    for entry in fs::read_dir(&vault)? {
        let entry = entry?;
        assert!(entry.file_name().to_string_lossy().starts_with("cv2-"));
        let metadata = entry.metadata()?;
        assert_eq!(metadata.permissions().mode() & 0o7777, PRIVATE_FILE_MODE);
        assert_eq!(metadata.uid(), effective_uid());
        assert_eq!(metadata.nlink(), 1);
    }
    Ok(())
}

#[test]
fn live_session_bus_without_provider_selects_persistent_file_vault() -> anyhow::Result<()> {
    let bus = TestSessionBus::start()?;
    let root = test_root();
    let results = test_root();
    let first = results.path().join("first");
    let second = results.path().join("second");
    run_platform_helper(root.path(), &first, &bus.address)?;
    run_platform_helper(root.path(), &second, &bus.address)?;
    assert_eq!(fs::read(first)?, fs::read(second)?);
    assert_eq!(
        fs::read(root.path().join("pro").join(BACKEND_MARKER))?,
        FILE_SELECTION
    );
    Ok(())
}

#[test]
fn platform_credential_vault_subprocess_helper() -> anyhow::Result<()> {
    if std::env::var_os(HELPER_MODE_ENV).as_deref() != Some(OsStr::new("load-or-store")) {
        return Ok(());
    }
    let data_root = std::env::var_os(HELPER_DATA_ROOT_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("missing credential-vault helper data root"))?;
    let result_path = std::env::var_os(HELPER_RESULT_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("missing credential-vault helper result path"))?;
    let record_id = ids(CredentialVaultNamespace::Production)
        .get(CredentialRecordKind::InstallationSigningKey)
        .to_owned();
    let backend = PlatformBackend::production(&data_root);
    let secret = match backend.load(&record_id) {
        Ok(secret) => secret,
        Err(CredentialVaultError::NotFound) => backend.load_or_store(&record_id, &[0x47; 32])?,
        Err(error) => return Err(error.into()),
    };
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .open(result_path)?;
    output.write_all(secret.as_slice())?;
    output.sync_all()?;
    Ok(())
}

fn run_platform_helper(
    data_root: &Path,
    result_path: &Path,
    bus_address: &str,
) -> anyhow::Result<()> {
    let runtime_dir = result_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("missing credential-vault helper runtime directory"))?;
    let mut child = Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg(HELPER_TEST_NAME)
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(HELPER_MODE_ENV, "load-or-store")
        .env(HELPER_DATA_ROOT_ENV, data_root)
        .env(HELPER_RESULT_ENV, result_path)
        .env("DBUS_SESSION_BUS_ADDRESS", bus_address)
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .spawn()?;
    let status = child.wait()?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "credential-vault helper exited with {status}"
        ));
    }
    Ok(())
}

struct TestSessionBus {
    child: Child,
    _stdout: ChildStdout,
    _root: tempfile::TempDir,
    address: String,
}

impl TestSessionBus {
    fn start() -> anyhow::Result<Self> {
        let root = test_root();
        let config = root.path().join("session.conf");
        fs::write(
            &config,
            br#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <keep_umask/>
  <listen>unix:tmpdir=/tmp</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
        )?;
        let mut child = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--nopidfile", "--print-address=1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("dbus-daemon did not expose stdout"))?;
        let mut address = String::new();
        BufReader::new(&mut stdout).read_line(&mut address)?;
        let address = address.trim().to_owned();
        if address.is_empty() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow::anyhow!("dbus-daemon returned an empty address"));
        }
        Ok(Self {
            child,
            _stdout: stdout,
            _root: root,
            address,
        })
    }
}

impl Drop for TestSessionBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn backend_selection_is_sticky_in_both_directions() -> anyhow::Result<()> {
    let file_root = test_root();
    let file_adapter = FakeAdapter::new(AdapterMode::Unavailable);
    let file_backend = LinuxBackend::new(file_root.path(), file_adapter.clone());
    let record_id = ids(CredentialVaultNamespace::Production)
        .get(CredentialRecordKind::InstallationSigningKey)
        .to_owned();
    file_backend.load_or_store(&record_id, &[1; 32])?;
    file_adapter.set_mode(AdapterMode::Available);
    assert_eq!(file_backend.load(&record_id)?.as_slice(), &[1; 32]);
    assert_eq!(file_adapter.counts(), (1, 0));

    let secret_root = test_root();
    let secret_adapter = FakeAdapter::new(AdapterMode::Available);
    let secret_backend = LinuxBackend::new(secret_root.path(), secret_adapter.clone());
    secret_backend.load_or_store(&record_id, &[2; 32])?;
    secret_adapter.set_mode(AdapterMode::Unavailable);
    assert!(matches!(
        secret_backend.load(&record_id),
        Err(CredentialVaultError::Unavailable { .. })
    ));
    assert!(!secret_root
        .path()
        .join("pro")
        .join(FILE_VAULT_DIRECTORY)
        .exists());
    assert_eq!(
        fs::read(secret_root.path().join("pro").join(BACKEND_MARKER))?,
        SECRET_SERVICE_SELECTION
    );
    Ok(())
}

#[test]
fn operation_level_unavailability_keeps_secret_service_selection_and_fails_closed(
) -> anyhow::Result<()> {
    let root = test_root();
    let adapter = FakeAdapter::new(AdapterMode::Available);
    adapter.set_fail_operations_unavailable(true);
    let backend = LinuxBackend::new(root.path(), adapter.clone());
    let record_id = ids(CredentialVaultNamespace::Production)
        .get(CredentialRecordKind::InstallationSigningKey)
        .to_owned();

    assert!(matches!(
        backend.load_or_store(&record_id, &[3; 32]),
        Err(CredentialVaultError::Unavailable { .. })
    ));
    assert_eq!(
        fs::read(root.path().join("pro").join(BACKEND_MARKER))?,
        SECRET_SERVICE_SELECTION
    );
    assert!(!root.path().join("pro").join(FILE_VAULT_DIRECTORY).exists());
    assert_eq!(adapter.counts(), (1, 0));
    Ok(())
}

#[test]
fn secret_service_operation_does_not_run_until_selection_is_durable() -> anyhow::Result<()> {
    let root = test_root();
    let adapter = FakeAdapter::new(AdapterMode::Available);
    let backend = LinuxBackend::new(root.path(), adapter.clone());
    let record_id = ids(CredentialVaultNamespace::Production)
        .get(CredentialRecordKind::InstallationSigningKey)
        .to_owned();
    fs::create_dir(root.path().join("pro").join(BACKEND_MARKER_STAGE))?;

    assert!(backend.load_or_store(&record_id, &[4; 32]).is_err());
    assert_eq!(adapter.counts(), (1, 0));
    assert!(!root.path().join("pro").join(BACKEND_MARKER).exists());
    Ok(())
}

#[test]
fn locked_denied_and_corrupt_adapter_fail_without_fallback() {
    for mode in [
        AdapterMode::Locked,
        AdapterMode::Backend,
        AdapterMode::Corrupt,
    ] {
        let root = test_root();
        let backend = LinuxBackend::new(root.path(), FakeAdapter::new(mode));
        let record_id = ids(CredentialVaultNamespace::Production)
            .get(CredentialRecordKind::WorkOsSession)
            .to_owned();
        assert!(backend.load(&record_id).is_err());
        assert!(backend.store(&record_id, b"credential").is_err());
        assert!(!root.path().join("pro").join(BACKEND_MARKER).exists());
        assert!(!root.path().join("pro").join(FILE_VAULT_DIRECTORY).exists());
    }
}

#[test]
fn pristine_unselected_unavailable_vault_is_not_found_without_creating_local_files() {
    let root = test_root();
    let adapter = FakeAdapter::new(AdapterMode::Unavailable);
    let backend = LinuxBackend::new(root.path(), adapter.clone());
    let record_id = ids(CredentialVaultNamespace::Production)
        .get(CredentialRecordKind::WorkOsSession)
        .to_owned();

    assert!(matches!(
        backend.load(&record_id),
        Err(CredentialVaultError::NotFound)
    ));
    assert!(matches!(
        backend.delete(&record_id),
        Err(CredentialVaultError::NotFound)
    ));
    assert_eq!(fs::read_dir(root.path().join("pro")).unwrap().count(), 0);
    assert_eq!(adapter.counts(), (2, 0));
}

#[test]
fn file_vault_rejects_symlinks_hardlinks_bad_modes_and_corruption() -> anyhow::Result<()> {
    let root = test_root();
    let backend = LinuxBackend::new(root.path(), FakeAdapter::new(AdapterMode::Unavailable));
    let record_id = ids(CredentialVaultNamespace::Production)
        .get(CredentialRecordKind::AnonymousTrial)
        .to_owned();
    assert!(matches!(
        backend.load(&record_id),
        Err(CredentialVaultError::NotFound)
    ));
    backend.load_or_store(&record_id, b"initial-secret")?;
    backend.delete(&record_id)?;

    let pro = root.path().join("pro");
    let outside = root.path().join("outside");
    fs::create_dir(&outside)?;
    fs::set_permissions(&outside, fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE))?;
    fs::remove_dir(pro.join(FILE_VAULT_DIRECTORY))?;
    symlink(&outside, pro.join(FILE_VAULT_DIRECTORY))?;
    assert_eq!(
        backend.store(&record_id, b"secret").unwrap_err(),
        CredentialVaultError::Corrupt
    );
    assert_eq!(fs::read_dir(&outside)?.count(), 0);
    fs::remove_file(pro.join(FILE_VAULT_DIRECTORY))?;

    fs::create_dir(pro.join(FILE_VAULT_DIRECTORY))?;
    fs::set_permissions(
        pro.join(FILE_VAULT_DIRECTORY),
        fs::Permissions::from_mode(0o755),
    )?;
    assert_eq!(
        backend.store(&record_id, b"secret").unwrap_err(),
        CredentialVaultError::Corrupt
    );
    assert_eq!(
        fs::metadata(pro.join(FILE_VAULT_DIRECTORY))?
            .permissions()
            .mode()
            & 0o7777,
        0o755
    );
    fs::remove_dir(pro.join(FILE_VAULT_DIRECTORY))?;

    backend.store(&record_id, b"secret")?;
    let vault = pro.join(FILE_VAULT_DIRECTORY);
    fs::remove_file(vault.join(&record_id))?;
    let target = outside.join("target");
    fs::write(&target, b"outside")?;
    fs::set_permissions(&target, fs::Permissions::from_mode(PRIVATE_FILE_MODE))?;
    symlink(&target, vault.join(&record_id))?;
    assert_eq!(
        backend.load(&record_id).unwrap_err(),
        CredentialVaultError::Corrupt
    );
    assert_eq!(fs::read(&target)?, b"outside");
    fs::remove_file(vault.join(&record_id))?;

    fs::hard_link(&target, vault.join(&record_id))?;
    assert_eq!(
        backend.load(&record_id).unwrap_err(),
        CredentialVaultError::Corrupt
    );
    fs::remove_file(vault.join(&record_id))?;
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644))?;
    fs::hard_link(&target, vault.join(&record_id))?;
    assert_eq!(
        backend.load(&record_id).unwrap_err(),
        CredentialVaultError::Corrupt
    );
    fs::remove_file(vault.join(&record_id))?;

    fs::write(vault.join(&record_id), vec![7; MAX_STORED_SECRET_BYTES + 1])?;
    fs::set_permissions(
        vault.join(&record_id),
        fs::Permissions::from_mode(PRIVATE_FILE_MODE),
    )?;
    assert_eq!(
        backend.load(&record_id).unwrap_err(),
        CredentialVaultError::Corrupt
    );
    Ok(())
}

#[test]
fn marker_symlink_and_corrupt_marker_fail_before_adapter_probe() -> anyhow::Result<()> {
    let linked_root = test_root();
    let link_parent = tempfile::tempdir()?;
    let root_link = link_parent.path().join("linked-data-root");
    symlink(linked_root.path(), &root_link)?;
    let linked_adapter = FakeAdapter::new(AdapterMode::Unavailable);
    let linked_backend = LinuxBackend::new(&root_link, linked_adapter.clone());
    let record_id = ids(CredentialVaultNamespace::Production)
        .get(CredentialRecordKind::WorkOsSession)
        .to_owned();
    assert_eq!(
        linked_backend.load(&record_id).unwrap_err(),
        CredentialVaultError::InvalidDataRoot
    );
    assert_eq!(linked_adapter.counts(), (0, 0));

    let symlink_root = test_root();
    let symlink_adapter = FakeAdapter::new(AdapterMode::Unavailable);
    let symlink_backend = LinuxBackend::new(symlink_root.path(), symlink_adapter.clone());
    let outside = symlink_root.path().join("outside-marker");
    fs::write(&outside, FILE_SELECTION)?;
    fs::set_permissions(&outside, fs::Permissions::from_mode(PRIVATE_FILE_MODE))?;
    symlink(
        &outside,
        symlink_root.path().join("pro").join(BACKEND_MARKER),
    )?;
    assert_eq!(
        symlink_backend.load(&record_id).unwrap_err(),
        CredentialVaultError::Corrupt
    );
    assert_eq!(symlink_adapter.counts(), (0, 0));

    let corrupt_root = test_root();
    let marker = corrupt_root.path().join("pro").join(BACKEND_MARKER);
    fs::write(&marker, b"unknown-backend\n")?;
    fs::set_permissions(&marker, fs::Permissions::from_mode(PRIVATE_FILE_MODE))?;
    let corrupt_backend = LinuxBackend::new(
        corrupt_root.path(),
        FakeAdapter::new(AdapterMode::Unavailable),
    );
    assert_eq!(
        corrupt_backend.load(&record_id).unwrap_err(),
        CredentialVaultError::Corrupt
    );
    Ok(())
}

#[test]
fn concurrent_first_use_persists_one_file_record() -> anyhow::Result<()> {
    const WORKERS: usize = 12;
    let root = test_root();
    let backend = Arc::new(LinuxBackend::new(
        root.path(),
        FakeAdapter::new(AdapterMode::Unavailable),
    ));
    let record_id = ids(CredentialVaultNamespace::Production)
        .get(CredentialRecordKind::InstallationSigningKey)
        .to_owned();
    let barrier = Arc::new(Barrier::new(WORKERS));
    let mut workers = Vec::new();
    for byte in 1..=WORKERS as u8 {
        let backend = Arc::clone(&backend);
        let barrier = Arc::clone(&barrier);
        let record_id = record_id.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            backend.load_or_store(&record_id, &[byte; 32])
        }));
    }
    let mut persisted = None;
    for worker in workers {
        let value = worker
            .join()
            .map_err(|_| anyhow::anyhow!("worker panicked"))??;
        if let Some(first) = &persisted {
            assert_eq!(value.as_slice(), first);
        } else {
            persisted = Some(value.as_slice().to_vec());
        }
    }
    assert!(persisted.is_some());
    Ok(())
}

#[test]
fn deletion_removes_records_stages_vault_and_selection_marker() -> anyhow::Result<()> {
    let root = test_root();
    let adapter = FakeAdapter::new(AdapterMode::Unavailable);
    let backend = LinuxBackend::new(root.path(), adapter.clone());
    let mut record_ids = Vec::new();
    for namespace in [
        CredentialVaultNamespace::Production,
        CredentialVaultNamespace::Staging,
    ] {
        let namespace_ids = ids(namespace);
        for kind in [
            CredentialRecordKind::WorkOsSession,
            CredentialRecordKind::AnonymousTrial,
            CredentialRecordKind::InstallationSigningKey,
            CredentialRecordKind::SignedEntitlement,
        ] {
            let record_id = namespace_ids.get(kind).to_owned();
            backend.store(&record_id, b"credential")?;
            record_ids.push(record_id);
        }
    }
    let stage = root
        .path()
        .join("pro")
        .join(FILE_VAULT_DIRECTORY)
        .join(record_stage_name(&record_ids[0]));
    fs::write(&stage, b"staged-secret")?;
    fs::set_permissions(&stage, fs::Permissions::from_mode(PRIVATE_FILE_MODE))?;

    for record_id in &record_ids {
        backend.delete(record_id)?;
        assert_eq!(
            backend.load(record_id).unwrap_err(),
            CredentialVaultError::NotFound
        );
    }
    backend.cleanup_if_empty()?;
    assert!(!root.path().join("pro").join(FILE_VAULT_DIRECTORY).exists());
    assert!(!root.path().join("pro").join(BACKEND_MARKER).exists());

    adapter.set_mode(AdapterMode::Available);
    backend.load_or_store(&record_ids[0], &[9; 32])?;
    assert_eq!(
        fs::read(root.path().join("pro").join(BACKEND_MARKER))?,
        SECRET_SERVICE_SELECTION
    );
    Ok(())
}

#[test]
fn file_backed_load_does_not_remove_staging_state() -> anyhow::Result<()> {
    let root = test_root();
    let backend = LinuxBackend::new(root.path(), FakeAdapter::new(AdapterMode::Unavailable));
    let record_id = ids(CredentialVaultNamespace::Production)
        .get(CredentialRecordKind::AnonymousTrial)
        .to_owned();
    backend.store(&record_id, b"persisted")?;
    let stage = root
        .path()
        .join("pro")
        .join(FILE_VAULT_DIRECTORY)
        .join(record_stage_name(&record_id));
    fs::write(&stage, b"incomplete")?;
    fs::set_permissions(&stage, fs::Permissions::from_mode(PRIVATE_FILE_MODE))?;

    assert_eq!(backend.load(&record_id)?.as_slice(), b"persisted");
    assert_eq!(fs::read(stage)?, b"incomplete");
    Ok(())
}
