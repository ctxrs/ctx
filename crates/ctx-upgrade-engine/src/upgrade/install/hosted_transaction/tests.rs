use super::*;
use ctx_managed_pair_engine::{
    stage_managed_pair_under_installation_lock, ManagedPairApplyInput, ManagedPairStageOutcome,
    MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH, MANAGED_PAIR_ENVELOPE_RELATIVE_PATH,
    MANAGED_PAIR_STATE_RELATIVE_PATH,
};
use std::{fs, os::unix::fs::PermissionsExt as _, process::Command, time::Duration};

use crate::upgrade::{
    download::DownloadedArtifact,
    install::{InstallFingerprint, InstallationLock},
    managed_pair::{apply_prepared_install, ManagedPairMode, PreparedCoreArtifact},
    metadata::ReleaseMetadata,
    state::{begin_manual_attempt_locked, UpgradeLock},
    UpgradePlan, TEST_RELEASE_PROCESS, TEST_SEMANTIC_LAYOUT,
};

const OLD_OWNERSHIP: &[u8] = b"CTX_INSTALL_INTEGRATIONS_V1\nrecords_sha256\told\n";
const NEW_OWNERSHIP: &[u8] = b"CTX_INSTALL_INTEGRATIONS_V1\nrecords_sha256\tnew\n";
const THIRD_OWNERSHIP: &[u8] = b"CTX_INSTALL_INTEGRATIONS_V1\nrecords_sha256\tthird\n";
const SELF_UPGRADE_CHILD_TARGET_ENV: &str = "CTX_SELF_UPGRADE_FENCE_CHILD_TARGET";

#[derive(Clone)]
struct TestPairVerifier {
    envelope: Vec<u8>,
    identity: ctx_managed_pair_engine::VerifiedManagedPairIdentity,
}

impl ManagedPairVerifier for TestPairVerifier {
    fn verify_signed_envelope(
        &self,
        signed_envelope: &[u8],
    ) -> Result<ctx_managed_pair_engine::VerifiedManagedPairIdentity> {
        if signed_envelope != self.envelope {
            bail!("test managed-pair envelope is not authenticated");
        }
        Ok(self.identity.clone())
    }
}

struct PairFixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    install: PathBuf,
    source: PathBuf,
    state: PathBuf,
    envelope: PathBuf,
    companion: PathBuf,
    verifier: TestPairVerifier,
}

fn pair_fixture() -> PairFixture {
    use ctx_managed_pair_engine::{
        ManagedPairComponentIdentity, ManagedPairTarget, VerifiedManagedPairIdentity,
    };

    let temp = tempfile::tempdir().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let root = temp.path().join("install");
    for directory in [
        root.clone(),
        root.join("bin"),
        root.join("libexec"),
        root.join("share"),
        root.join("share/ctx"),
    ] {
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let core_bytes = b"paired ctx";
    let companion_bytes = b"paired ctx-pro";
    let envelope_bytes = b"authenticated test envelope\n";
    let install = root.join("bin/ctx");
    let source = temp.path().join("source-ctx");
    let companion = root.join("libexec/ctx-pro");
    let envelope = root.join(MANAGED_PAIR_ENVELOPE_RELATIVE_PATH);
    let state = root.join(MANAGED_PAIR_STATE_RELATIVE_PATH);
    fs::write(&install, core_bytes).unwrap();
    fs::write(&source, core_bytes).unwrap();
    fs::write(&companion, companion_bytes).unwrap();
    fs::write(&envelope, envelope_bytes).unwrap();
    fs::set_permissions(&install, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&companion, fs::Permissions::from_mode(0o700)).unwrap();

    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "aarch64") => ManagedPairTarget::LinuxArm64,
        ("linux", "x86_64") => ManagedPairTarget::LinuxX64,
        ("macos", "aarch64") => ManagedPairTarget::MacosArm64,
        ("macos", "x86_64") => ManagedPairTarget::MacosX64,
        (os, arch) => panic!("unsupported pair-test target {os}-{arch}"),
    };
    let identity = VerifiedManagedPairIdentity::new(
        "test-release",
        target,
        1,
        "a".repeat(64),
        ManagedPairComponentIdentity::new(
            sha256_hex(core_bytes),
            u64::try_from(core_bytes.len()).unwrap(),
        )
        .unwrap(),
        ManagedPairComponentIdentity::new(
            sha256_hex(companion_bytes),
            u64::try_from(companion_bytes.len()).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let state_body = serde_json::to_vec_pretty(&json!({
        "contract": "ctx-managed-pair-state",
        "schema_version": 1,
        "identity": identity,
        "envelope_sha256": sha256_hex(envelope_bytes),
        "envelope_size_bytes": envelope_bytes.len(),
    }))
    .unwrap();
    fs::write(&state, state_body).unwrap();
    fs::write(
        install_marker_path(&install),
        paired_marker(&install, &sha256_hex(core_bytes)),
    )
    .unwrap();
    fs::set_permissions(
        install_marker_path(&install),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    PairFixture {
        _temp: temp,
        root,
        install,
        source,
        state,
        envelope,
        companion,
        verifier: TestPairVerifier {
            envelope: envelope_bytes.to_vec(),
            identity,
        },
    }
}

fn fixture() -> (tempfile::TempDir, PathBuf, String, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let install = temp.path().join("ctx");
    let binary = b"new ctx";
    let digest = sha256_hex(binary);
    let source = temp.path().join("candidate");
    fs::write(&source, binary).unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
    (temp, install, digest, source)
}

fn marker(install: &Path, digest: &str) -> String {
    serde_json::to_string_pretty(&json!({
        "schema_version": 1,
        "manager": "ctx-hosted-installer",
        "install_attempt_id": "ia_12345678",
        "install_path": install,
        "platform": platform_key().unwrap(),
        "channel": "stable",
        "version": "1.0.0",
        "sha256": digest,
    }))
    .unwrap()
        + "\n"
}

fn paired_marker(install: &Path, digest: &str) -> String {
    let mut value: Value = serde_json::from_str(&marker(install, digest)).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("managed_pair".to_owned(), Value::Bool(true));
    serde_json::to_string_pretty(&value).unwrap() + "\n"
}

fn owned_install_journal(
    install: &Path,
    binary_sha256: &str,
    ownership_body: &[u8],
    attempt_id: &str,
) -> Journal {
    let owned_path = ownership_path(install);
    let ownership_sha256 = sha256_hex(ownership_body);
    let marker_body = serde_json::to_string_pretty(&json!({
        "schema_version": 1,
        "manager": "ctx-hosted-installer",
        "install_attempt_id": attempt_id,
        "install_path": install,
        "platform": platform_key().unwrap(),
        "channel": "stable",
        "version": "1.0.0",
        "sha256": binary_sha256,
        "integrations_path": owned_path,
        "integrations_sha256": ownership_sha256,
    }))
    .unwrap()
        + "\n";
    let mut journal = Journal {
        schema_version: SCHEMA_VERSION,
        kind: TransactionKind::Install,
        attempt_id: attempt_id.into(),
        install_path: install.to_owned(),
        marker_path: install_marker_path(install),
        binary_sha256: binary_sha256.into(),
        marker_sha256: sha256_hex(marker_body.as_bytes()),
        marker_body,
        prior_binary_sha256: None,
        prior_marker_sha256: None,
        prior_ownership_sha256: None,
        ownership_path: Some(owned_path),
        ownership_sha256: Some(ownership_sha256),
        ownership_body: Some(ownership_body.to_vec()),
        managed_pair_state_sha256: None,
        managed_pair_envelope_sha256: None,
        managed_pair_companion_sha256: None,
        phase: Phase::Prepared,
        binding_sha256: String::new(),
    };
    journal.binding_sha256 = journal_binding(&journal);
    journal
}

fn commit_install(source: &Path, journal: &mut Journal) {
    validate_journal(journal, &journal.install_path, TransactionKind::Install).unwrap();
    let path = journal_path(&journal.install_path);
    write_initial_journal(&path, journal).unwrap();
    complete_install(source, &path, journal).unwrap();
}

fn arm_uninstall(install: &Path, source: &Path) -> (PathBuf, PathBuf, Journal) {
    let helper = uninstall_helper_path(install);
    fs::copy(source, &helper).unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    let mut journal = new_uninstall_journal(install, "ia_87654321").unwrap();
    verify_recorded_ownership(&journal).unwrap();
    journal.phase = Phase::Armed;
    let path = journal_path(install);
    write_initial_journal(&path, &journal).unwrap();
    (helper, path, journal)
}

fn arm_pair_uninstall(fixture: &PairFixture) -> (PathBuf, PathBuf, Journal) {
    let helper = uninstall_helper_path(&fixture.install);
    fs::copy(&fixture.source, &helper).unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    let mut journal = new_uninstall_journal_with_optional_verifier(
        &fixture.install,
        "ia_87654321",
        Some(&fixture.verifier),
    )
    .unwrap();
    verify_recorded_pair_files(&journal, true).unwrap();
    journal.phase = Phase::Armed;
    let path = journal_path(&fixture.install);
    write_initial_journal(&path, &journal).unwrap();
    (helper, path, journal)
}

#[test]
fn integration_reconciliation_crash_retries_and_uninstalls_from_one_marker_authority() {
    let fixture = pair_fixture();
    let old_source = fixture.root.join("old-integrations");
    let new_source = fixture.root.join("new-integrations");
    fs::write(&old_source, OLD_OWNERSHIP).unwrap();
    fs::write(&new_source, NEW_OWNERSHIP).unwrap();

    let installation = InstallationLock::try_acquire(&fixture.install)
        .unwrap()
        .unwrap();
    super::super::marker::reconcile_managed_pair_integration_under_installation_lock(
        &fixture.root,
        &old_source,
    )
    .unwrap();
    drop(installation);
    let marker_path = install_marker_path(&fixture.install);
    let old_marker = fs::read(&marker_path).unwrap();
    let old_value: Value = serde_json::from_slice(&old_marker).unwrap();
    let old_generation = PathBuf::from(old_value["integrations_path"].as_str().unwrap());
    assert_eq!(fs::read(&old_generation).unwrap(), OLD_OWNERSHIP);
    let new_generation = fixture.root.join(format!(
        "bin/ctx.install-integrations.{}",
        sha256_hex(NEW_OWNERSHIP)
    ));

    let mut stale = old_value.clone();
    stale["sha256"] = json!("0".repeat(64));
    fs::write(&marker_path, serde_json::to_vec_pretty(&stale).unwrap()).unwrap();
    let installation = InstallationLock::try_acquire(&fixture.install)
        .unwrap()
        .unwrap();
    let stale_error =
        super::super::marker::reconcile_managed_pair_integration_under_installation_lock(
            &fixture.root,
            &new_source,
        )
        .unwrap_err();
    drop(installation);
    assert!(stale_error.to_string().contains("hash mismatch"));
    assert!(!new_generation.exists());
    fs::write(&marker_path, &old_marker).unwrap();

    let installation = InstallationLock::try_acquire(&fixture.install)
        .unwrap()
        .unwrap();
    let error = super::super::marker::reconcile_managed_pair_integration_with_fault(
        &fixture.root,
        &new_source,
        &mut || bail!("injected after generation publication"),
    )
    .unwrap_err();
    drop(installation);
    assert!(error.to_string().contains("injected after generation"));
    assert_eq!(fs::read(&marker_path).unwrap(), old_marker);
    assert_eq!(fs::read(&old_generation).unwrap(), OLD_OWNERSHIP);
    assert_eq!(fs::read(&new_generation).unwrap(), NEW_OWNERSHIP);

    let installation = InstallationLock::try_acquire(&fixture.install)
        .unwrap()
        .unwrap();
    super::super::marker::reconcile_managed_pair_integration_under_installation_lock(
        &fixture.root,
        &new_source,
    )
    .unwrap();
    drop(installation);
    let new_value: Value = serde_json::from_slice(&fs::read(&marker_path).unwrap()).unwrap();
    let mut expected = old_value;
    expected["integrations_path"] = json!(new_generation);
    expected["integrations_sha256"] = json!(sha256_hex(NEW_OWNERSHIP));
    assert_eq!(new_value, expected);
    assert!(!old_generation.exists());

    let third_source = fixture.root.join("third-integrations");
    fs::write(&third_source, THIRD_OWNERSHIP).unwrap();
    let installation = InstallationLock::try_acquire(&fixture.install)
        .unwrap()
        .unwrap();
    super::super::marker::reconcile_managed_pair_integration_under_installation_lock(
        &fixture.root,
        &third_source,
    )
    .unwrap();
    drop(installation);
    let third_marker: Value = serde_json::from_slice(&fs::read(&marker_path).unwrap()).unwrap();
    let third_generation = PathBuf::from(third_marker["integrations_path"].as_str().unwrap());
    assert_eq!(fs::read(&third_generation).unwrap(), THIRD_OWNERSHIP);
    assert!(!new_generation.exists());

    let (helper, path, mut uninstall) = arm_pair_uninstall(&fixture);
    complete_uninstall_commit(&helper, &path, &mut uninstall, &mut |_| Ok(())).unwrap();
    remove_journal(&path).unwrap();
    assert!(!third_generation.exists());
    assert!(!marker_path.exists());
    assert!(!fixture.install.exists());
    assert!(fs::read_dir(fixture.root.join("bin"))
        .unwrap()
        .all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("ctx.install-integrations.")));
}

fn ordinary_upgrade_plan(install: &Path, next_core: &[u8]) -> UpgradePlan {
    let artifact_sha256 = sha256_hex(next_core);
    UpgradePlan {
        current_version: "1.0.0".to_owned(),
        latest_version: "1.1.0".to_owned(),
        channel: "stable".to_owned(),
        platform: platform_key().unwrap().to_owned(),
        metadata_url: "https://cli.ctx.rs/releases/stable/metadata".to_owned(),
        artifact_url: "https://cli.ctx.rs/releases/1.1.0/ctx".to_owned(),
        artifact_sha256: artifact_sha256.clone(),
        install_path: install.to_owned(),
        install_fingerprint: InstallFingerprint {
            binary_sha256: sha256_hex(&fs::read(install).unwrap()),
            marker_sha256: sha256_hex(&fs::read(install_marker_path(install)).unwrap()),
        },
        update_available: true,
        managed: true,
        warnings: Vec::new(),
        managed_pair_release: None,
        metadata: ReleaseMetadata {
            version: "1.1.0".to_owned(),
            base_url: "https://cli.ctx.rs/releases/1.1.0".to_owned(),
            artifact: "ctx".to_owned(),
            sha256: artifact_sha256,
            source_commit: None,
            published_at: None,
            self_upgrade_allowed: true,
            auto_upgrade_allowed: true,
            store_schema_version: None,
            managed_pair: None,
            onnxruntime: None,
            semantic: None,
        },
        semantic_provisioning: None,
    }
}

fn create_crash_before_pending_candidate(fixture: &PairFixture) -> PathBuf {
    let candidate = fixture.root.join("share/ctx/.managed-pair-apply-v1");
    for directory in [
        candidate.clone(),
        candidate.join("bin"),
        candidate.join("libexec"),
        candidate.join("share"),
        candidate.join("share/ctx"),
    ] {
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
    for (source, destination) in [
        (&fixture.install, candidate.join("bin/ctx")),
        (
            &install_marker_path(&fixture.install),
            candidate.join("bin/ctx.install.json"),
        ),
        (&fixture.companion, candidate.join("libexec/ctx-pro")),
        (
            &fixture.envelope,
            candidate.join(MANAGED_PAIR_ENVELOPE_RELATIVE_PATH),
        ),
    ] {
        fs::copy(source, destination).unwrap();
    }
    candidate
}

fn assert_installed(install: &Path, ownership_body: &[u8], point: &str) {
    assert_eq!(fs::read(install).unwrap(), b"new ctx", "{point}");
    assert_eq!(
        fs::read(ownership_path(install)).unwrap(),
        ownership_body,
        "{point}"
    );
    let marker = fs::read_to_string(install_marker_path(install)).unwrap();
    let recorded = read_recorded_ownership(&marker, install).unwrap().unwrap();
    assert_eq!(recorded.2, ownership_body, "{point}");
    assert!(!journal_path(install).exists(), "{point}");
}

#[test]
fn hosted_transaction_receipts_keep_the_stable_machine_schema() {
    let (_temp, install, digest, _source) = fixture();
    let journal = owned_install_journal(&install, &digest, NEW_OWNERSHIP, "receipt-attempt");

    let install_value = install_receipt(&journal);
    assert_eq!(install_value["schema_version"], 1);
    assert_eq!(install_value["command"], "hosted_install_transaction");
    assert_eq!(install_value["status"], "committed");
    assert_eq!(install_value["attempt_id"], "receipt-attempt");
    assert_eq!(install_value["binary_sha256"], digest);

    let helper = uninstall_helper_path(&install);
    let uninstall_value = uninstall_receipt(&journal, &helper, "armed");
    assert_eq!(uninstall_value["schema_version"], 2);
    assert_eq!(uninstall_value["command"], "hosted_uninstall_transaction");
    assert_eq!(uninstall_value["status"], "armed");
    assert_eq!(uninstall_value["daemon_admission_fenced"], true);
    assert_eq!(
        uninstall_value["helper_path"],
        helper.to_string_lossy().as_ref()
    );
}

#[test]
fn hosted_uninstall_journal_fences_daemon_admission_through_commit() {
    let (_temp, install, digest, source) = fixture();
    fs::copy(&source, &install).unwrap();
    fs::write(install_marker_path(&install), marker(&install, &digest)).unwrap();
    let path = journal_path(&install);
    let mut journal = new_uninstall_journal(&install, "ia_87654321").unwrap();
    write_initial_journal(&path, &journal).unwrap();

    for phase in [
        Phase::Prepared,
        Phase::HelperStaged,
        Phase::Armed,
        Phase::RemovingBinary,
        Phase::BinaryRemoved,
        Phase::RemovingMarker,
        Phase::Committed,
    ] {
        journal.phase = phase;
        write_journal(&path, &journal).unwrap();
        assert!(
            hosted_uninstall_is_active_for(&install).unwrap(),
            "{phase:?}"
        );
    }

    remove_journal(&path).unwrap();
    assert!(!hosted_uninstall_is_active_for(&install).unwrap());
}

#[test]
fn hosted_uninstall_admission_fence_fails_closed_on_identity_changes() {
    let (_temp, install, digest, source) = fixture();
    fs::copy(&source, &install).unwrap();
    fs::write(install_marker_path(&install), marker(&install, &digest)).unwrap();
    let path = journal_path(&install);
    let journal = new_uninstall_journal(&install, "ia_87654321").unwrap();
    write_initial_journal(&path, &journal).unwrap();

    fs::write(&install, b"changed ctx").unwrap();
    assert!(hosted_uninstall_is_active_for(&install).is_err());

    fs::copy(&source, &install).unwrap();
    fs::write(install_marker_path(&install), b"changed marker").unwrap();
    assert!(hosted_uninstall_is_active_for(&install).is_err());
}

#[test]
fn upgrade_handoff_explicit_restart_path_preserves_uninstall_validation() {
    let (_temp, install, digest, source) = fixture();
    fs::copy(&source, &install).unwrap();
    fs::write(install_marker_path(&install), marker(&install, &digest)).unwrap();
    assert!(!hosted_uninstall_is_active_for_executable(&install).unwrap());

    let journal = new_uninstall_journal(&install, "ia_87654321").unwrap();
    write_initial_journal(&journal_path(&install), &journal).unwrap();
    assert!(hosted_uninstall_is_active_for_executable(&install).unwrap());

    fs::write(install_marker_path(&install), b"changed marker").unwrap();
    assert!(hosted_uninstall_is_active_for_executable(&install).is_err());
}

#[test]
fn hosted_uninstall_helper_uses_the_installation_admission_fence() {
    let (_temp, install, digest, source) = fixture();
    fs::copy(&source, &install).unwrap();
    fs::write(install_marker_path(&install), marker(&install, &digest)).unwrap();
    let journal = new_uninstall_journal(&install, "ia_87654321").unwrap();
    write_initial_journal(&journal_path(&install), &journal).unwrap();
    let helper = uninstall_helper_path(&install);
    fs::copy(&install, &helper).unwrap();

    assert!(hosted_uninstall_is_active_for_executable(&helper).unwrap());
    remove_journal(&journal_path(&install)).unwrap();
    assert!(hosted_uninstall_is_active_for_executable(&helper).unwrap());
    write_initial_journal(&journal_path(&install), &journal).unwrap();
    fs::write(&helper, b"changed helper").unwrap();
    assert!(hosted_uninstall_is_active_for_executable(&helper).is_err());
}

#[test]
fn fresh_uninstall_discards_the_previous_windows_style_helper() {
    let (_temp, install, digest, source) = fixture();
    fs::copy(&source, &install).unwrap();
    fs::write(install_marker_path(&install), marker(&install, &digest)).unwrap();
    let helper = uninstall_helper_path(&install);
    fs::write(&helper, b"older installed Core").unwrap();

    let journal = new_uninstall_journal(&install, "ia_87654321").unwrap();
    begin_fresh_uninstall(&journal_path(&install), &journal, &helper).unwrap();
    stage_file(&install, &helper, true).unwrap();

    verify_file_digest(
        &helper,
        &journal.binary_sha256,
        MAX_BINARY_BYTES,
        "replacement hosted uninstall helper",
    )
    .unwrap();
}

#[test]
fn hosted_journal_rejects_cross_kind_phases() {
    let (_temp, install, digest, _source) = fixture();
    let mut journal = owned_install_journal(&install, &digest, NEW_OWNERSHIP, "ia_12345678");
    journal.phase = Phase::HelperStaged;
    assert!(validate_journal(&journal, &install, TransactionKind::Install).is_err());

    journal.kind = TransactionKind::Uninstall;
    journal.phase = Phase::BinaryStaged;
    journal.binding_sha256 = journal_binding(&journal);
    assert!(validate_journal(&journal, &install, TransactionKind::Uninstall).is_err());
}

#[test]
fn journal_binding_rejects_path_and_digest_changes() {
    let (_temp, install, digest, _source) = fixture();
    let body = marker(&install, &digest);
    let mut journal = Journal {
        schema_version: 1,
        kind: TransactionKind::Install,
        attempt_id: "ia_12345678".into(),
        install_path: install.clone(),
        marker_path: install_marker_path(&install),
        binary_sha256: digest,
        marker_sha256: sha256_hex(body.as_bytes()),
        marker_body: body,
        prior_binary_sha256: None,
        prior_marker_sha256: None,
        prior_ownership_sha256: None,
        ownership_path: None,
        ownership_sha256: None,
        ownership_body: None,
        managed_pair_state_sha256: None,
        managed_pair_envelope_sha256: None,
        managed_pair_companion_sha256: None,
        phase: Phase::Prepared,
        binding_sha256: String::new(),
    };
    journal.binding_sha256 = journal_binding(&journal);
    assert!(validate_journal(&journal, &install, TransactionKind::Install).is_ok());
    journal.install_path = install.with_file_name("other-ctx");
    assert!(validate_journal(&journal, &install, TransactionKind::Install).is_err());
    journal.install_path = install.clone();
    journal.binary_sha256 = "0".repeat(64);
    assert!(validate_journal(&journal, &install, TransactionKind::Install).is_err());
}

#[test]
fn markerless_existing_binary_remains_unmanaged() {
    let (_temp, install, _digest, _source) = fixture();
    fs::write(&install, b"new ctx").unwrap();
    assert!(validate_existing_pair_for_install(&install).is_err());
}

#[test]
fn posix_publication_consumes_a_sibling_without_truncating_target() {
    let temp = tempfile::tempdir().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let target = temp.path().join("ctx");
    let staged = temp.path().join(".ctx.new");
    fs::write(&target, b"old-complete").unwrap();
    fs::write(&staged, b"new-complete").unwrap();
    atomic_publish(&staged, &target).unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"new-complete");
    assert!(!staged.exists());
}

#[test]
fn install_retry_converges_from_every_durable_phase() {
    const POINTS: &[&str] = &[
        "journal_prepared",
        "binary_staged",
        "binary_replaced",
        "binary_published",
        "ownership_replaced",
        "ownership_published",
        "marker_replaced",
        "marker_published",
        "committed",
    ];
    for point in POINTS {
        let (_temp, install, digest, source) = fixture();
        let mut journal = owned_install_journal(&install, &digest, OLD_OWNERSHIP, "ia_12345678");
        let path = journal_path(&install);
        write_initial_journal(&path, &journal).unwrap();
        let mut injected = false;
        let error = complete_install_with_fault(&source, &path, &mut journal, &mut |observed| {
            if !injected && observed == *point {
                injected = true;
                bail!("injected interruption after {observed}");
            }
            Ok(())
        })
        .unwrap_err();
        assert!(
            error.to_string().contains("injected interruption"),
            "{point}"
        );
        let mut recovered = read_journal(&path).unwrap().unwrap();
        validate_journal(&recovered, &install, TransactionKind::Install).unwrap();
        complete_install(&source, &path, &mut recovered).unwrap();
        assert_installed(&install, OLD_OWNERSHIP, point);
    }
}

#[test]
fn install_uninstall_fault_matrix_reinstalls_new_integration_body() {
    const POINTS: &[&str] = &[
        "armed",
        "removing_binary",
        "binary_removed",
        "binary_removed_recorded",
        "removing_ownership",
        "ownership_removed",
        "ownership_removed_recorded",
        "removing_marker",
        "marker_removed",
        "committed",
    ];
    for point in POINTS {
        let (_temp, install, digest, source) = fixture();
        let mut initial = owned_install_journal(&install, &digest, OLD_OWNERSHIP, "ia_12345678");
        commit_install(&source, &mut initial);
        assert_installed(&install, OLD_OWNERSHIP, point);

        let (helper, path, mut uninstall) = arm_uninstall(&install, &source);
        assert_eq!(uninstall.ownership_body.as_deref(), Some(OLD_OWNERSHIP));
        let mut injected = false;
        let error = complete_uninstall_commit(&helper, &path, &mut uninstall, &mut |observed| {
            if !injected && observed == *point {
                injected = true;
                bail!("injected interruption after {observed}");
            }
            Ok(())
        })
        .unwrap_err();
        assert!(
            error.to_string().contains("injected interruption"),
            "{point}"
        );
        assert!(path.exists(), "{point}");

        let mut recovered = read_journal(&path).unwrap().unwrap();
        validate_journal(&recovered, &install, TransactionKind::Uninstall).unwrap();
        complete_uninstall_commit(&helper, &path, &mut recovered, &mut |_| Ok(())).unwrap();
        remove_journal(&path).unwrap();
        assert!(!install.exists(), "{point}");
        assert!(!install_marker_path(&install).exists(), "{point}");
        assert!(!ownership_path(&install).exists(), "{point}");
        assert!(!path.exists(), "{point}");

        let mut reinstalled =
            owned_install_journal(&install, &digest, NEW_OWNERSHIP, "ia_23456789");
        commit_install(&source, &mut reinstalled);
        assert_installed(&install, NEW_OWNERSHIP, point);
    }
}

#[test]
fn hosted_uninstall_removes_only_the_exact_authenticated_pair_files() {
    let fixture = pair_fixture();
    let unrelated_share = fixture.root.join("share/ctx/user-owned.json");
    let unrelated_libexec = fixture.root.join("libexec/user-helper");
    fs::write(&unrelated_share, b"keep share").unwrap();
    fs::write(&unrelated_libexec, b"keep libexec").unwrap();
    let (helper, path, mut journal) = arm_pair_uninstall(&fixture);

    complete_uninstall_commit(&helper, &path, &mut journal, &mut |_| Ok(())).unwrap();
    remove_journal(&path).unwrap();

    for removed in [
        fixture.state,
        fixture.envelope,
        fixture.companion,
        fixture.install.clone(),
        install_marker_path(&fixture.install),
    ] {
        assert!(
            !removed.exists(),
            "retained managed file {}",
            removed.display()
        );
    }
    assert_eq!(fs::read(unrelated_share).unwrap(), b"keep share");
    assert_eq!(fs::read(unrelated_libexec).unwrap(), b"keep libexec");
}

#[test]
fn hosted_uninstall_refuses_a_pending_pair_upgrade() {
    use ctx_managed_pair_engine::{ManagedPairComponentIdentity, VerifiedManagedPairIdentity};

    let fixture = pair_fixture();
    let candidate_root = fixture.root.join("next-pair");
    fs::create_dir(&candidate_root).unwrap();
    fs::set_permissions(&candidate_root, fs::Permissions::from_mode(0o700)).unwrap();
    let candidate = |name: &str| candidate_root.join(name);
    let core = candidate("ctx");
    let companion = candidate("ctx-pro");
    let envelope = candidate("managed-pair-envelope.json");
    let marker_source = candidate("ctx.install.json");
    let next_core = b"next paired ctx";
    let next_companion = b"next paired ctx-pro";
    fs::write(&core, next_core).unwrap();
    fs::write(&companion, next_companion).unwrap();
    fs::write(&envelope, &fixture.verifier.envelope).unwrap();
    fs::write(
        &marker_source,
        marker(&fixture.install, &sha256_hex(next_core)),
    )
    .unwrap();
    for path in [&core, &companion, &envelope, &marker_source] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    let staged_verifier = TestPairVerifier {
        envelope: fixture.verifier.envelope.clone(),
        identity: VerifiedManagedPairIdentity::new(
            "test-release-next",
            fixture.verifier.identity.target(),
            fixture.verifier.identity.rollback_generation() + 1,
            "b".repeat(64),
            ManagedPairComponentIdentity::new(sha256_hex(next_core), next_core.len() as u64)
                .unwrap(),
            ManagedPairComponentIdentity::new(
                sha256_hex(next_companion),
                next_companion.len() as u64,
            )
            .unwrap(),
        )
        .unwrap(),
    };
    let staged = stage_managed_pair_under_installation_lock(
        &fixture.root,
        &ManagedPairApplyInput::new(&envelope, &core, &companion, &marker_source),
        &staged_verifier,
    )
    .unwrap();
    assert!(matches!(staged, ManagedPairStageOutcome::Staged { .. }));
    let pending = fixture
        .root
        .join(MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH);
    let owned_candidate = fixture.root.join("share/ctx/.managed-pair-apply-v1");
    let pending_before = fs::read(&pending).unwrap();
    let candidate_core_before = fs::read(owned_candidate.join("bin/ctx")).unwrap();

    let error = new_uninstall_journal_with_optional_verifier(
        &fixture.install,
        "ia_87654321",
        Some(&fixture.verifier),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("finish the pending managed-pair upgrade"));
    assert_eq!(fs::read(&pending).unwrap(), pending_before);
    assert_eq!(
        fs::read(owned_candidate.join("bin/ctx")).unwrap(),
        candidate_core_before
    );
}

#[test]
fn ordinary_self_upgrade_is_fenced_while_hosted_uninstall_waits_for_parent_exit() {
    let fixture = pair_fixture();
    let (_helper, path, journal) = arm_pair_uninstall(&fixture);
    let core_before = fs::read(&fixture.install).unwrap();
    let marker_before = fs::read(install_marker_path(&fixture.install)).unwrap();

    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "upgrade::install::hosted_transaction::tests::ordinary_self_upgrade_child_probe",
        ])
        .env(SELF_UPGRADE_CHILD_TARGET_ENV, &fixture.install)
        .env("CTX_UPGRADE_TEST_TARGET", &fixture.install)
        .status()
        .unwrap();

    assert!(status.success());
    assert_eq!(fs::read(&fixture.install).unwrap(), core_before);
    assert_eq!(
        fs::read(install_marker_path(&fixture.install)).unwrap(),
        marker_before
    );
    assert_eq!(read_journal(&path).unwrap().unwrap().phase, Phase::Armed);
    assert_eq!(journal.phase, Phase::Armed);
}

#[test]
fn ordinary_self_upgrade_child_probe() -> Result<()> {
    let Some(install) = std::env::var_os(SELF_UPGRADE_CHILD_TARGET_ENV) else {
        return Ok(());
    };
    let install = PathBuf::from(install);
    let data_root = install
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("upgrade-data");
    fs::create_dir(&data_root)?;
    fs::set_permissions(&data_root, fs::Permissions::from_mode(0o700))?;
    let next_core = b"ordinary self-upgrade Core";
    let plan = ordinary_upgrade_plan(&install, next_core);
    let lock = UpgradeLock::acquire(&data_root)?;
    let attempt = begin_manual_attempt_locked(&data_root, &lock, "manual_apply")?;
    let mut core = PreparedCoreArtifact::Legacy(DownloadedArtifact::from_bytes(
        &data_root,
        next_core,
        MAX_BINARY_BYTES,
        "ordinary self-upgrade Core",
    )?);
    let mut before_publish_called = false;
    let error = apply_prepared_install(
        &TEST_RELEASE_PROCESS,
        &TEST_SEMANTIC_LAYOUT,
        &lock,
        &plan,
        &ManagedPairMode::CoreOnly,
        &mut core,
        None,
        &mut [],
        &data_root,
        &attempt,
        Duration::from_secs(3600),
        None,
        &mut || {
            before_publish_called = true;
            Ok(())
        },
    )
    .unwrap_err();

    assert!(format!("{error:#}").contains("finish the pending hosted installation transaction"));
    assert!(!before_publish_called);
    Ok(())
}

#[test]
fn hosted_uninstall_cleans_candidate_orphaned_before_pending_publication() {
    let fixture = pair_fixture();
    let orphan = create_crash_before_pending_candidate(&fixture);
    assert!(orphan.is_dir());
    assert!(!fixture
        .root
        .join(MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH)
        .exists());

    let (helper, path, mut journal) = arm_pair_uninstall(&fixture);
    assert!(!orphan.exists());
    complete_uninstall_commit(&helper, &path, &mut journal, &mut |_| Ok(())).unwrap();
    remove_journal(&path).unwrap();

    assert!(!fixture.install.exists());
    assert!(!fixture.state.exists());
    assert!(!fixture.envelope.exists());
    assert!(!fixture.companion.exists());
}

#[test]
fn core_only_hosted_install_refuses_managed_pair_material() {
    let fixture = pair_fixture();
    let error = reject_managed_pair_material_for_core_only_install(&fixture.install).unwrap_err();
    assert!(format!("{error:#}").contains("cannot replace a managed Core+Pro pair"));

    fs::remove_file(&fixture.state).unwrap();
    fs::remove_file(&fixture.envelope).unwrap();
    fs::remove_file(&fixture.companion).unwrap();
    let error = reject_managed_pair_material_for_core_only_install(&fixture.install).unwrap_err();
    assert!(format!("{error:#}").contains("cannot replace a managed Core+Pro pair"));

    fs::write(
        install_marker_path(&fixture.install),
        marker(&fixture.install, &sha256_hex(b"paired ctx")),
    )
    .unwrap();
    reject_managed_pair_material_for_core_only_install(&fixture.install).unwrap();
}

#[test]
fn hosted_uninstall_refuses_substituted_pair_file_before_removing_state() {
    let fixture = pair_fixture();
    let (helper, path, mut journal) = arm_pair_uninstall(&fixture);
    fs::write(&fixture.companion, b"substituted companion").unwrap();

    let error =
        complete_uninstall_commit(&helper, &path, &mut journal, &mut |_| Ok(())).unwrap_err();
    assert!(
        format!("{error:#}").contains("refusing substituted managed-pair companion"),
        "{error:#}"
    );
    assert!(fixture.state.exists());
    assert!(fixture.envelope.exists());
    assert_eq!(
        fs::read(&fixture.companion).unwrap(),
        b"substituted companion"
    );
    assert!(fixture.install.exists());
    assert!(path.exists());
}

#[test]
fn hosted_uninstall_refuses_symlinked_pair_file_before_removing_state() {
    let fixture = pair_fixture();
    let (helper, path, mut journal) = arm_pair_uninstall(&fixture);
    let moved = fixture.root.join("libexec/original-companion");
    fs::rename(&fixture.companion, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &fixture.companion).unwrap();

    assert!(complete_uninstall_commit(&helper, &path, &mut journal, &mut |_| Ok(())).is_err());
    assert!(fixture.state.exists());
    assert!(fs::symlink_metadata(&fixture.companion)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(fixture.install.exists());
}

#[test]
fn authenticated_pair_uninstall_retries_every_pair_removal_boundary() {
    const POINTS: &[&str] = &[
        "removing_pair_state",
        "pair_state_removed",
        "removing_pair_envelope",
        "pair_envelope_removed",
        "removing_pair_companion",
        "pair_companion_removed",
    ];
    for point in POINTS {
        let fixture = pair_fixture();
        let (helper, path, mut journal) = arm_pair_uninstall(&fixture);
        let mut injected = false;
        let error = complete_uninstall_commit(&helper, &path, &mut journal, &mut |observed| {
            if !injected && observed == *point {
                injected = true;
                bail!("injected interruption after {observed}");
            }
            Ok(())
        })
        .unwrap_err();
        assert!(
            error.to_string().contains("injected interruption"),
            "{point}"
        );
        if *point == "pair_state_removed" {
            assert!(!fixture.state.exists());
            assert!(fixture.envelope.exists());
            assert!(fixture.companion.exists());
            assert!(fixture.install.exists());
        }

        let mut recovered = read_journal(&path).unwrap().unwrap();
        complete_uninstall_commit(&helper, &path, &mut recovered, &mut |_| Ok(())).unwrap();
        remove_journal(&path).unwrap();
        assert!(!fixture.state.exists(), "{point}");
        assert!(!fixture.envelope.exists(), "{point}");
        assert!(!fixture.companion.exists(), "{point}");
        assert!(!fixture.install.exists(), "{point}");
    }
}

#[test]
fn changed_owned_sidecar_fails_closed_with_recovery_journal() {
    let (_temp, install, digest, source) = fixture();
    let mut initial = owned_install_journal(&install, &digest, OLD_OWNERSHIP, "ia_12345678");
    commit_install(&source, &mut initial);
    let (helper, path, mut uninstall) = arm_uninstall(&install, &source);
    let owned_path = ownership_path(&install);
    let changed = b"CTX_INSTALL_INTEGRATIONS_V1\nrecords_sha256\tchanged\n";
    fs::write(&owned_path, changed).unwrap();

    let error =
        complete_uninstall_commit(&helper, &path, &mut uninstall, &mut |_| Ok(())).unwrap_err();
    assert!(format!("{error:#}").contains("restore the transaction-owned sidecar"));
    assert_eq!(fs::read(&owned_path).unwrap(), changed);
    assert!(path.exists());
    assert!(install_marker_path(&install).exists());
    assert!(!install.exists());
    assert_eq!(
        read_journal(&path).unwrap().unwrap().phase,
        Phase::BinaryRemoved
    );

    let mut retry = read_journal(&path).unwrap().unwrap();
    assert!(complete_uninstall_commit(&helper, &path, &mut retry, &mut |_| Ok(())).is_err());
    assert_eq!(fs::read(&owned_path).unwrap(), changed);
    assert!(path.exists());

    let rescued = install.with_file_name("ctx.install-integrations.changed");
    fs::rename(&owned_path, &rescued).unwrap();
    let mut recovered = read_journal(&path).unwrap().unwrap();
    complete_uninstall_commit(&helper, &path, &mut recovered, &mut |_| Ok(())).unwrap();
    remove_journal(&path).unwrap();
    assert_eq!(fs::read(&rescued).unwrap(), changed);

    let mut reinstalled = owned_install_journal(&install, &digest, NEW_OWNERSHIP, "ia_23456789");
    commit_install(&source, &mut reinstalled);
    assert_installed(&install, NEW_OWNERSHIP, "ownership mismatch recovery");
    assert_eq!(fs::read(&rescued).unwrap(), changed);
}

#[test]
fn dangling_changed_sidecar_fails_closed_with_recovery_journal() {
    let (_temp, install, digest, source) = fixture();
    let mut initial = owned_install_journal(&install, &digest, OLD_OWNERSHIP, "ia_12345678");
    commit_install(&source, &mut initial);
    let (helper, path, mut uninstall) = arm_uninstall(&install, &source);
    let owned_path = ownership_path(&install);
    fs::remove_file(&owned_path).unwrap();
    std::os::unix::fs::symlink("missing-integration-body", &owned_path).unwrap();

    let error =
        complete_uninstall_commit(&helper, &path, &mut uninstall, &mut |_| Ok(())).unwrap_err();
    assert!(format!("{error:#}").contains("restore the transaction-owned sidecar"));
    assert!(fs::symlink_metadata(&owned_path)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(path.exists());
    assert!(install_marker_path(&install).exists());
}

#[test]
fn install_refuses_unowned_dangling_sidecar_and_retains_journal() {
    let (_temp, install, digest, source) = fixture();
    let owned_path = ownership_path(&install);
    std::os::unix::fs::symlink("missing-integration-body", &owned_path).unwrap();
    let mut journal = owned_install_journal(&install, &digest, NEW_OWNERSHIP, "ia_12345678");
    let path = journal_path(&install);
    write_initial_journal(&path, &journal).unwrap();

    assert!(complete_install(&source, &path, &mut journal).is_err());
    assert!(fs::symlink_metadata(&owned_path)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(path.exists());
    assert!(!install_marker_path(&install).exists());
}

#[test]
fn unowned_sidecar_is_never_removed() {
    let (_temp, install, digest, source) = fixture();
    fs::copy(&source, &install).unwrap();
    fs::write(install_marker_path(&install), marker(&install, &digest)).unwrap();
    let unowned = ownership_path(&install);
    fs::write(&unowned, b"not transaction owned").unwrap();
    let (helper, path, mut uninstall) = arm_uninstall(&install, &source);
    assert!(uninstall.ownership_path.is_none());

    complete_uninstall_commit(&helper, &path, &mut uninstall, &mut |_| Ok(())).unwrap();
    remove_journal(&path).unwrap();
    assert_eq!(fs::read(&unowned).unwrap(), b"not transaction owned");
    assert!(!install.exists());
    assert!(!install_marker_path(&install).exists());
    assert!(!path.exists());
}
