use super::*;
use std::{fs, os::unix::fs::PermissionsExt as _};

const OLD_OWNERSHIP: &[u8] = b"CTX_INSTALL_INTEGRATIONS_V1\nrecords_sha256\told\n";
const NEW_OWNERSHIP: &[u8] = b"CTX_INSTALL_INTEGRATIONS_V1\nrecords_sha256\tnew\n";

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
