use std::cell::{Cell, RefCell};

use ctx_history_core::platform_security::{restrict_private_directory, restrict_private_file};
use tempfile::tempdir;

use super::*;

#[derive(Default)]
struct RecordingBackend {
    targets: BTreeSet<GraphKeyDeletionTarget>,
    inventory_reads: Cell<usize>,
    corrupt_inventory: bool,
    graph_key_missing: bool,
    fail_graph_key_verification: bool,
    fail_commercial_credentials_after_delete: bool,
    require_cleanup_phase_for_graph_delete: Option<PathBuf>,
    require_graph_absent_for_graph_delete: Option<PathBuf>,
    deleted: RefCell<Vec<String>>,
    deletion_thumbprints: RefCell<Vec<String>>,
    deletion_namespaces: RefCell<Vec<GraphKeyCredentialNamespace>>,
    partial_credentials_deleted: RefCell<bool>,
    credentials_deleted: RefCell<bool>,
}

impl DeletionBackend for RecordingBackend {
    fn graph_key_deletion_targets(
        &self,
        _data_root: &Path,
    ) -> Result<BTreeSet<GraphKeyDeletionTarget>> {
        self.inventory_reads
            .set(self.inventory_reads.get().saturating_add(1));
        if self.corrupt_inventory {
            return Err(vault_error(CredentialVaultError::Corrupt));
        }
        Ok(self.targets.clone())
    }

    fn delete_graph_record(
        &self,
        _data_root: &Path,
        target: &GraphKeyDeletionTarget,
        graph_id: &str,
    ) -> Result<()> {
        if let Some(root) = &self.require_cleanup_phase_for_graph_delete {
            match local_pro_graph_key_cleanup_phase_exists(root) {
                Ok(true) => {}
                Ok(false) | Err(_) => {
                    bail!("key_store_unavailable: cleanup phase was not durable")
                }
            }
        }
        if let Some(graph) = &self.require_graph_absent_for_graph_delete {
            if graph.exists() {
                bail!("key_store_unavailable: graph artifacts still existed before key deletion");
            }
        }
        self.deleted.borrow_mut().push(graph_id.to_owned());
        self.deletion_thumbprints
            .borrow_mut()
            .push(target.installation_key_thumbprint.clone());
        self.deletion_namespaces.borrow_mut().push(target.namespace);
        if self.fail_graph_key_verification {
            bail!("key_store_unavailable: simulated graph-key verification failure");
        }
        let _ = self.graph_key_missing;
        Ok(())
    }

    fn delete_partial_bootstrap_credentials(&self, _data_root: &Path) -> Result<()> {
        self.partial_credentials_deleted.replace(true);
        if self.fail_commercial_credentials_after_delete {
            bail!("key_store_unavailable: simulated partial credential deletion failure");
        }
        Ok(())
    }

    fn delete_commercial_credentials(&self, _data_root: &Path) -> Result<()> {
        self.credentials_deleted.replace(true);
        if self.fail_commercial_credentials_after_delete {
            bail!("key_store_unavailable: simulated late credential deletion failure");
        }
        Ok(())
    }
}

fn test_thumbprint(seed: u8) -> String {
    installation_key_thumbprint(
        &SigningKey::from_bytes(&[seed; INSTALLATION_PUBLIC_KEY_BYTES])
            .verifying_key()
            .to_bytes(),
    )
}

fn test_target(namespace: GraphKeyCredentialNamespace, seed: u8) -> GraphKeyDeletionTarget {
    GraphKeyDeletionTarget {
        namespace,
        installation_key_thumbprint: test_thumbprint(seed),
    }
}

struct ValidKeyCorruptEntitlementReader {
    loads: Cell<usize>,
}

impl CredentialRecordReader for ValidKeyCorruptEntitlementReader {
    fn load_record(
        &self,
        kind: CredentialRecordKind,
    ) -> Result<CredentialRecord, CredentialVaultError> {
        self.loads.set(self.loads.get().saturating_add(1));
        match kind {
            CredentialRecordKind::InstallationSigningKey => {
                Ok(CredentialRecord::InstallationSigningKey(
                    super::super::credential_vault::InstallationSigningKeySeed::from_bytes(
                        [10; INSTALLATION_PUBLIC_KEY_BYTES],
                    ),
                ))
            }
            CredentialRecordKind::SignedEntitlement => Err(CredentialVaultError::Corrupt),
            CredentialRecordKind::WorkOsSession | CredentialRecordKind::AnonymousTrial => {
                Err(CredentialVaultError::Backend)
            }
        }
    }
}

struct MismatchedKeyAndEntitlementReader;

impl CredentialRecordReader for MismatchedKeyAndEntitlementReader {
    fn load_record(
        &self,
        kind: CredentialRecordKind,
    ) -> Result<CredentialRecord, CredentialVaultError> {
        match kind {
            CredentialRecordKind::InstallationSigningKey => {
                Ok(CredentialRecord::InstallationSigningKey(
                    super::super::credential_vault::InstallationSigningKeySeed::from_bytes(
                        [20; INSTALLATION_PUBLIC_KEY_BYTES],
                    ),
                ))
            }
            CredentialRecordKind::SignedEntitlement => {
                use ctx_pro_host_protocol::{
                    base64url, EntitlementAccessKind, EntitlementCapability, EntitlementGrant,
                    SignedEntitlement, ED25519_SIGNATURE_BYTES, ENTITLEMENT_SCHEMA_VERSION,
                    PROTOCOL_VERSION,
                };

                let public_key = SigningKey::from_bytes(&[21; INSTALLATION_PUBLIC_KEY_BYTES])
                    .verifying_key()
                    .to_bytes();
                let entitlement = SignedEntitlement {
                    grant: EntitlementGrant {
                        schema_version: ENTITLEMENT_SCHEMA_VERSION,
                        issuer: "https://pro-staging.ctx.rs".to_owned(),
                        key_id: "staging-2026-07-v3".to_owned(),
                        grant_id: "grant-mismatch".to_owned(),
                        subject: "subject".to_owned(),
                        account_id: "account".to_owned(),
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
                };
                Ok(CredentialRecord::SignedEntitlement(
                    super::super::credential_vault::BoundedSignedEntitlement::new(entitlement)?,
                ))
            }
            CredentialRecordKind::WorkOsSession | CredentialRecordKind::AnonymousTrial => {
                Err(CredentialVaultError::Backend)
            }
        }
    }
}

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let root = tempdir().unwrap();
    crate::identity::installation_id(root.path()).unwrap();
    let pro = ProFilesystemLayout::new(root.path()).pro_root();
    fs::create_dir(&pro).unwrap();
    restrict_private_directory(&pro).unwrap();
    create_current_graph(&pro);
    (root, pro)
}

fn graph_dir(pro: &Path) -> PathBuf {
    pro.join(ctx_pro_host_protocol::PRO_GRAPH_DIRECTORY_NAME)
}

fn graph_manifest(pro: &Path) -> PathBuf {
    graph_dir(pro).join("graph-manifest.ctxm")
}

fn create_current_graph(pro: &Path) {
    let graph = graph_dir(pro);
    fs::create_dir(&graph).unwrap();
    restrict_private_directory(&graph).unwrap();
    write_private_graph_file(&graph_manifest(pro));
}

fn write_private_graph_file(path: &Path) {
    fs::write(path, "ciphertext").unwrap();
    restrict_private_file(path).unwrap();
}

fn exact_graph_artifact_paths(pro: &Path) -> Vec<PathBuf> {
    let graph = graph_dir(pro);
    let object = "a".repeat(64);
    let materialization = "b".repeat(64);
    vec![
        graph.join("graph-manifest.ctxm"),
        graph.join(".graph-materializer-control.ctxc"),
        graph.join("graph-manifest.publication-lock"),
        graph.join("graph-materializer.lock"),
        graph.join(format!(".graph-manifest-{object}.candidate")),
        graph.join(format!(".graph-materializer-control-{object}.candidate")),
        graph.join(".graph-manifest-open-1-0.encrypted-tmp"),
        graph.join(format!(".graph-materializer-open-{object}-0.encrypted-tmp")),
        graph.join(format!("graph-segment-{object}-bbbbbbbb.ctxs")),
        graph.join(format!(
            ".graph-materializer-journal-{materialization}-{object}.ctxj"
        )),
        graph.join(format!(
            ".graph-materializer-pack-{materialization}-{object}.ctxp"
        )),
    ]
}

#[test]
fn direct_deletion_accepts_an_already_missing_graph_key() {
    let (root, pro) = fixture();
    let backend = RecordingBackend {
        targets: BTreeSet::from([test_target(GraphKeyCredentialNamespace::Production, 1)]),
        graph_key_missing: true,
        ..RecordingBackend::default()
    };
    let mut service = LocalDeletionService::with_backend_for_test(backend);
    service.delete_graph_data(root.path()).unwrap();
    assert!(!graph_dir(&pro).exists());
    assert_eq!(service.backend.deleted.borrow().len(), 1);
}

#[test]
fn mixed_valid_and_corrupt_record_inventory_fails_closed() {
    let reader = ValidKeyCorruptEntitlementReader {
        loads: Cell::new(0),
    };
    let error =
        recorded_graph_key_targets(GraphKeyCredentialNamespace::Production, &reader).unwrap_err();
    assert!(error.to_string().starts_with("key_store_unavailable:"));
    assert_eq!(reader.loads.get(), 2);
}

#[test]
fn mismatched_key_and_entitlement_in_one_namespace_fail_closed() {
    let error = recorded_graph_key_targets(
        GraphKeyCredentialNamespace::Staging,
        &MismatchedKeyAndEntitlementReader,
    )
    .unwrap_err();
    assert!(error.to_string().starts_with("key_store_unavailable:"));
}

#[test]
fn mixed_valid_and_corrupt_namespace_inventory_deletes_nothing() {
    let (root, pro) = fixture();
    let backend = RecordingBackend {
        // Model a valid thumbprint observed in one exact namespace before a
        // corrupt record makes the complete two-namespace inventory unverifiable.
        targets: BTreeSet::from([test_target(GraphKeyCredentialNamespace::Production, 11)]),
        corrupt_inventory: true,
        ..RecordingBackend::default()
    };
    let mut service = LocalDeletionService::with_backend_for_test(backend);
    let error = service.delete_graph_data(root.path()).unwrap_err();
    assert!(error.to_string().starts_with("key_store_unavailable:"));
    assert_eq!(service.backend.inventory_reads.get(), 1);
    assert!(service.backend.deleted.borrow().is_empty());
    assert!(graph_manifest(&pro).exists());
    assert!(!local_pro_graph_key_cleanup_phase_exists(root.path()).unwrap());
}

#[test]
fn missing_or_empty_roots_are_vault_free_idempotent_noops() {
    let parent = tempdir().unwrap();
    let missing = parent.path().join("missing");
    let mut missing_service =
        LocalDeletionService::with_backend_for_test(RecordingBackend::default());
    missing_service.delete_graph_data(&missing).unwrap();
    assert!(!missing.exists());
    assert!(missing_service.backend.deleted.borrow().is_empty());
    assert_eq!(missing_service.backend.inventory_reads.get(), 0);

    let empty = tempdir().unwrap();
    crate::identity::installation_id(empty.path()).unwrap();
    let empty_pro = ProFilesystemLayout::new(empty.path()).pro_root();
    fs::create_dir(&empty_pro).unwrap();
    let mut empty_service =
        LocalDeletionService::with_backend_for_test(RecordingBackend::default());
    empty_service.delete_graph_data(empty.path()).unwrap();
    assert!(empty_pro.is_dir());
    assert!(empty_service.backend.deleted.borrow().is_empty());
    assert_eq!(empty_service.backend.inventory_reads.get(), 0);
}

#[test]
fn helperless_graphless_bootstrap_skips_helper_ipc_and_cleans_credentials() {
    let root = tempdir().unwrap();
    crate::identity::installation_id(root.path()).unwrap();
    let pro = ProFilesystemLayout::new(root.path()).pro_root();
    fs::create_dir(&pro).unwrap();
    restrict_private_directory(&pro).unwrap();
    write_local_pro_initialization_indicator(root.path()).unwrap();
    let backend = RecordingBackend {
        corrupt_inventory: true,
        ..RecordingBackend::default()
    };
    let mut service = LocalDeletionService::with_backend_for_test(backend);

    service.delete_graph_data(root.path()).unwrap();
    assert_eq!(service.backend.inventory_reads.get(), 0);
    assert!(service.backend.deleted.borrow().is_empty());
    assert!(local_pro_graph_key_cleanup_phase_exists(root.path()).unwrap());

    service.delete_commercial_credentials(root.path()).unwrap();
    assert!(*service.backend.partial_credentials_deleted.borrow());
    assert!(!*service.backend.credentials_deleted.borrow());
    service.finish_deletion(root.path()).unwrap();
    assert!(!local_pro_graph_key_cleanup_phase_exists(root.path()).unwrap());
}

#[test]
fn initialized_empty_graph_directory_is_removed_without_key_inventory() {
    let root = tempdir().unwrap();
    crate::identity::installation_id(root.path()).unwrap();
    let layout = ProFilesystemLayout::new(root.path());
    fs::create_dir(&layout.pro_root()).unwrap();
    restrict_private_directory(&layout.pro_root()).unwrap();
    write_local_pro_initialization_indicator(root.path()).unwrap();
    fs::create_dir(layout.graph_dir()).unwrap();
    restrict_private_directory(&layout.graph_dir()).unwrap();
    let backend = RecordingBackend {
        corrupt_inventory: true,
        ..RecordingBackend::default()
    };
    let mut service = LocalDeletionService::with_backend_for_test(backend);

    service.delete_graph_data(root.path()).unwrap();

    assert!(!layout.graph_dir().exists());
    assert_eq!(service.backend.inventory_reads.get(), 0);
    assert!(service.backend.deleted.borrow().is_empty());
}

#[test]
fn flat_graph_is_deleted_before_graph_key_and_verified_absent() {
    let root = tempdir().unwrap();
    crate::identity::installation_id(root.path()).unwrap();
    let pro = ProFilesystemLayout::new(root.path()).pro_root();
    fs::create_dir(&pro).unwrap();
    restrict_private_directory(&pro).unwrap();
    write_local_pro_initialization_indicator(root.path()).unwrap();
    let graph = pro.join("graph");
    fs::create_dir(&graph).unwrap();
    restrict_private_directory(&graph).unwrap();
    let manifest = graph.join("graph-manifest.ctxm");
    fs::write(&manifest, b"encrypted manifest").unwrap();
    restrict_private_file(&manifest).unwrap();
    let backend = RecordingBackend {
        targets: BTreeSet::from([test_target(GraphKeyCredentialNamespace::Production, 24)]),
        require_graph_absent_for_graph_delete: Some(graph.clone()),
        ..RecordingBackend::default()
    };
    let mut service = LocalDeletionService::with_backend_for_test(backend);

    service.delete_graph_data(root.path()).unwrap();

    assert!(!graph.exists());
    assert_eq!(service.backend.inventory_reads.get(), 1);
    assert_eq!(service.backend.deleted.borrow().len(), 1);
}

#[test]
fn interrupted_graph_delete_retries_recorded_targets_without_helper_ipc() {
    let root = tempdir().unwrap();
    let installation_id = crate::identity::installation_id(root.path()).unwrap();
    let pro = ProFilesystemLayout::new(root.path()).pro_root();
    fs::create_dir(&pro).unwrap();
    restrict_private_directory(&pro).unwrap();
    write_local_pro_initialization_indicator(root.path()).unwrap();
    write_graph_key_cleanup_phase(
        root.path(),
        &installation_id,
        &BTreeSet::from([test_target(GraphKeyCredentialNamespace::Production, 22)]),
    )
    .unwrap();
    let backend = RecordingBackend {
        corrupt_inventory: true,
        ..RecordingBackend::default()
    };
    let mut service = LocalDeletionService::with_backend_for_test(backend);

    service.delete_graph_data(root.path()).unwrap();
    assert_eq!(service.backend.inventory_reads.get(), 0);
    assert_eq!(service.backend.deleted.borrow().len(), 1);
    service.delete_commercial_credentials(root.path()).unwrap();
    service.finish_deletion(root.path()).unwrap();
    assert!(!local_pro_graph_key_cleanup_phase_exists(root.path()).unwrap());
}

#[test]
fn helperless_bootstrap_credential_failure_is_typed_and_keeps_retry_journal() {
    let root = tempdir().unwrap();
    crate::identity::installation_id(root.path()).unwrap();
    let pro = ProFilesystemLayout::new(root.path()).pro_root();
    fs::create_dir(&pro).unwrap();
    restrict_private_directory(&pro).unwrap();
    write_local_pro_initialization_indicator(root.path()).unwrap();
    let backend = RecordingBackend {
        fail_commercial_credentials_after_delete: true,
        ..RecordingBackend::default()
    };
    let mut service = LocalDeletionService::with_backend_for_test(backend);

    service.delete_graph_data(root.path()).unwrap();
    let error = service
        .delete_commercial_credentials(root.path())
        .unwrap_err();
    assert!(error.to_string().starts_with("key_store_unavailable:"));
    assert!(local_pro_graph_key_cleanup_phase_exists(root.path()).unwrap());
    assert!(service.backend.deleted.borrow().is_empty());
}

#[test]
fn helper_present_without_graph_deletes_recorded_graph_keys() {
    let root = tempdir().unwrap();
    let installation_id = crate::identity::installation_id(root.path()).unwrap();
    let layout = ProFilesystemLayout::new(root.path());
    let pro = layout.pro_root();
    fs::create_dir(&pro).unwrap();
    restrict_private_directory(&pro).unwrap();
    fs::create_dir(layout.bin_dir()).unwrap();
    fs::write(layout.helper_path(), b"signed helper").unwrap();

    let targets = BTreeSet::from([
        test_target(GraphKeyCredentialNamespace::Production, 2),
        test_target(GraphKeyCredentialNamespace::Staging, 3),
    ]);
    let expected_graph_ids = targets
        .iter()
        .map(|target| {
            pro_graph_record_id(&installation_id, &target.installation_key_thumbprint).unwrap()
        })
        .collect::<Vec<_>>();
    let expected_thumbprints = targets
        .iter()
        .map(|target| target.installation_key_thumbprint.clone())
        .collect::<Vec<_>>();
    let expected_namespaces = targets
        .iter()
        .map(|target| target.namespace)
        .collect::<Vec<_>>();
    let backend = RecordingBackend {
        targets,
        ..RecordingBackend::default()
    };
    let mut service = LocalDeletionService::with_backend_for_test(backend);
    service.delete_graph_data(root.path()).unwrap();

    assert_eq!(service.backend.inventory_reads.get(), 1);
    assert_eq!(
        service.backend.deleted.borrow().as_slice(),
        expected_graph_ids
    );
    assert_eq!(
        service.backend.deletion_thumbprints.borrow().as_slice(),
        expected_thumbprints
    );
    assert_eq!(
        service.backend.deletion_namespaces.borrow().as_slice(),
        expected_namespaces
    );
    assert!(layout.helper_path().exists());
    assert!(!layout.graph_dir().exists());
}

#[test]
fn signed_helper_marker_without_binary_stays_on_helper_deletion_path() {
    let root = tempdir().unwrap();
    crate::identity::installation_id(root.path()).unwrap();
    let layout = ProFilesystemLayout::new(root.path());
    fs::create_dir(layout.pro_root()).unwrap();
    restrict_private_directory(&layout.pro_root()).unwrap();
    write_local_pro_initialization_indicator(root.path()).unwrap();
    fs::create_dir(layout.bin_dir()).unwrap();
    restrict_private_directory(&layout.bin_dir()).unwrap();
    fs::write(layout.helper_marker_path(), b"signed marker").unwrap();
    restrict_private_file(&layout.helper_marker_path()).unwrap();
    let backend = RecordingBackend {
        targets: BTreeSet::from([test_target(GraphKeyCredentialNamespace::Production, 23)]),
        ..RecordingBackend::default()
    };
    let mut service = LocalDeletionService::with_backend_for_test(backend);

    service.delete_graph_data(root.path()).unwrap();
    assert_eq!(service.backend.inventory_reads.get(), 1);
    assert_eq!(service.backend.deleted.borrow().len(), 1);
    service.delete_commercial_credentials(root.path()).unwrap();
    assert!(!*service.backend.partial_credentials_deleted.borrow());
    assert!(*service.backend.credentials_deleted.borrow());
}

#[test]
fn cleanup_phase_is_durable_before_graph_key_deletion() {
    let (root, _) = fixture();
    let backend = RecordingBackend {
        targets: BTreeSet::from([test_target(GraphKeyCredentialNamespace::Production, 12)]),
        require_cleanup_phase_for_graph_delete: Some(root.path().to_path_buf()),
        ..RecordingBackend::default()
    };
    let mut service = LocalDeletionService::with_backend_for_test(backend);
    service.delete_graph_data(root.path()).unwrap();
    assert_eq!(service.backend.deleted.borrow().len(), 1);
    assert!(local_pro_graph_key_cleanup_phase_exists(root.path()).unwrap());
}

#[test]
fn late_failure_retries_from_cleanup_phase_after_records_are_gone() {
    let (root, pro) = fixture();
    let target = test_target(GraphKeyCredentialNamespace::Staging, 13);
    let first_backend = RecordingBackend {
        targets: BTreeSet::from([target]),
        fail_commercial_credentials_after_delete: true,
        ..RecordingBackend::default()
    };
    let mut first = LocalDeletionService::with_backend_for_test(first_backend);
    first.delete_graph_data(root.path()).unwrap();
    assert!(!graph_dir(&pro).exists());
    assert!(first.delete_commercial_credentials(root.path()).is_err());
    assert!(*first.backend.credentials_deleted.borrow());
    assert!(local_pro_graph_key_cleanup_phase_exists(root.path()).unwrap());

    let retry_backend = RecordingBackend {
        corrupt_inventory: true,
        graph_key_missing: true,
        ..RecordingBackend::default()
    };
    let mut retry = LocalDeletionService::with_backend_for_test(retry_backend);
    retry.delete_graph_data(root.path()).unwrap();
    assert_eq!(retry.backend.inventory_reads.get(), 0);
    assert_eq!(retry.backend.deleted.borrow().len(), 1);
    retry.delete_commercial_credentials(root.path()).unwrap();
    retry.finish_deletion(root.path()).unwrap();
    assert!(!local_pro_graph_key_cleanup_phase_exists(root.path()).unwrap());
}

#[test]
fn cleanup_phase_cannot_cross_installation_identities() {
    let (root, pro) = fixture();
    let other_installation_id = "5d98d375-4ac4-4507-be4b-c435e373f042";
    write_graph_key_cleanup_phase(
        root.path(),
        other_installation_id,
        &BTreeSet::from([test_target(GraphKeyCredentialNamespace::Production, 14)]),
    )
    .unwrap();
    let mut service = LocalDeletionService::with_backend_for_test(RecordingBackend::default());
    let error = service.delete_graph_data(root.path()).unwrap_err();
    assert!(error.to_string().starts_with("key_store_unavailable:"));
    assert!(service.backend.deleted.borrow().is_empty());
    assert!(graph_manifest(&pro).exists());
}

#[test]
fn empty_cleanup_phase_cannot_delete_graph_artifacts() {
    let (root, pro) = fixture();
    let installation_id = crate::identity::existing_installation_id(root.path())
        .unwrap()
        .unwrap();
    write_graph_key_cleanup_phase(root.path(), &installation_id, &BTreeSet::new()).unwrap();
    let mut service = LocalDeletionService::with_backend_for_test(RecordingBackend::default());
    let error = service.delete_graph_data(root.path()).unwrap_err();
    assert!(error.to_string().starts_with("key_store_unavailable:"));
    assert!(service.backend.deleted.borrow().is_empty());
    assert!(graph_manifest(&pro).exists());
}

#[test]
fn graph_family_and_interrupted_rebuild_files_are_all_removed() {
    let (root, pro) = fixture();
    let paths = exact_graph_artifact_paths(&pro);
    for path in &paths {
        write_private_graph_file(path);
    }
    let backend = RecordingBackend {
        targets: BTreeSet::from([test_target(GraphKeyCredentialNamespace::Production, 4)]),
        ..RecordingBackend::default()
    };
    let mut service = LocalDeletionService::with_backend_for_test(backend);
    service.delete_graph_data(root.path()).unwrap();
    assert!(paths.iter().all(|path| !path.exists()));
    assert!(!graph_dir(&pro).exists());
}

#[test]
fn unexpected_near_miss_blocks_all_graph_and_key_deletion() {
    let (root, pro) = fixture();
    let near_miss = graph_dir(&pro).join(".graph-manifest-deadbeef.candidate");
    write_private_graph_file(&near_miss);
    let backend = RecordingBackend {
        targets: BTreeSet::from([test_target(GraphKeyCredentialNamespace::Production, 25)]),
        ..RecordingBackend::default()
    };
    let mut service = LocalDeletionService::with_backend_for_test(backend);

    let error = service.delete_graph_data(root.path()).unwrap_err();

    assert!(error.to_string().starts_with("invalid_request:"));
    assert_eq!(service.backend.inventory_reads.get(), 0);
    assert!(service.backend.deleted.borrow().is_empty());
    assert!(graph_manifest(&pro).exists());
    assert!(near_miss.exists());
    assert!(!local_pro_graph_key_cleanup_phase_exists(root.path()).unwrap());
}

#[test]
fn failed_graph_key_verification_keeps_retry_phase_after_graph_removal() {
    let (root, pro) = fixture();
    let backend = RecordingBackend {
        targets: BTreeSet::from([test_target(GraphKeyCredentialNamespace::Production, 5)]),
        fail_graph_key_verification: true,
        ..RecordingBackend::default()
    };
    let mut service = LocalDeletionService::with_backend_for_test(backend);
    assert!(service.delete_graph_data(root.path()).is_err());
    assert!(!graph_dir(&pro).exists());
    assert!(local_pro_graph_key_cleanup_phase_exists(root.path()).unwrap());
}

#[cfg(unix)]
#[test]
fn symlink_graph_or_graph_root_fails_before_key_deletion() {
    use std::os::unix::fs::symlink;

    let root = tempdir().unwrap();
    let outside = tempdir().unwrap();
    crate::identity::installation_id(root.path()).unwrap();
    symlink(
        outside.path(),
        ProFilesystemLayout::new(root.path()).pro_root(),
    )
    .unwrap();
    let backend = RecordingBackend {
        targets: BTreeSet::from([test_target(GraphKeyCredentialNamespace::Production, 7)]),
        ..RecordingBackend::default()
    };
    let mut service = LocalDeletionService::with_backend_for_test(backend);
    assert!(service.delete_graph_data(root.path()).is_err());
    assert!(service.backend.deleted.borrow().is_empty());

    let pro_root = ProFilesystemLayout::new(root.path()).pro_root();
    fs::remove_file(&pro_root).unwrap();
    fs::create_dir(&pro_root).unwrap();
    fs::set_permissions(
        &pro_root,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let outside_graph = outside.path().join("graph");
    fs::create_dir(&outside_graph).unwrap();
    symlink(&outside_graph, graph_dir(&pro_root)).unwrap();
    assert!(service.delete_graph_data(root.path()).is_err());
    assert!(service.backend.deleted.borrow().is_empty());
    assert!(outside_graph.is_dir());
}

#[cfg(unix)]
#[test]
fn shared_graph_root_fails_before_key_deletion() {
    use std::os::unix::fs::PermissionsExt as _;

    let (root, pro) = fixture();
    fs::set_permissions(&pro, fs::Permissions::from_mode(0o755)).unwrap();
    let backend = RecordingBackend {
        targets: BTreeSet::from([test_target(GraphKeyCredentialNamespace::Production, 8)]),
        ..RecordingBackend::default()
    };
    let mut service = LocalDeletionService::with_backend_for_test(backend);
    assert!(service.delete_graph_data(root.path()).is_err());
    assert!(service.backend.deleted.borrow().is_empty());
    assert!(graph_manifest(&pro).exists());
}

#[cfg(windows)]
#[test]
fn shared_graph_root_fails_before_key_derivation_on_windows() {
    let (root, pro) = fixture();
    make_private_directory_unsafe(&pro).unwrap();
    let backend = RecordingBackend {
        targets: BTreeSet::from([test_target(GraphKeyCredentialNamespace::Production, 9)]),
        ..RecordingBackend::default()
    };
    let mut service = LocalDeletionService::with_backend_for_test(backend);
    assert!(service.delete_graph_data(root.path()).is_err());
    assert!(service.backend.deleted.borrow().is_empty());
    assert!(graph_manifest(&pro).exists());
}

#[test]
fn graph_identity_matches_private_protocol_v1_format() {
    let id = pro_graph_record_id("6a1de1ab-c732-45ed-b3f8-bbf6ab1048e8", "thumbprint").unwrap();
    assert_eq!(
        id,
        "ctx-pro-installation-graph-v1:6a1de1ab-c732-45ed-b3f8-bbf6ab1048e8:thumbprint"
    );
}

#[test]
fn missing_identity_fails_closed_while_ciphertext_remains() {
    let (root, pro) = fixture();
    let mut service = LocalDeletionService::with_backend_for_test(RecordingBackend::default());
    assert!(service
        .delete_graph_data(root.path())
        .unwrap_err()
        .to_string()
        .starts_with("key_store_unavailable:"));
    assert!(graph_manifest(&pro).exists());
}
