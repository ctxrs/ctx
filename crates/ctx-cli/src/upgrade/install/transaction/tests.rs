#[cfg(unix)]
use std::fs;

#[cfg(unix)]
use tempfile::tempdir;

use super::journal::{
    self, InstallTransactionJournal, JournalPath, JournalPathIdentity, JournalPathKind,
    JournalPathState, JournalPhase,
};
#[cfg(unix)]
use super::RecoveryOutcome;

fn semantic_journal_path(
    attempt_id: &str,
    label: &str,
    target: std::path::PathBuf,
    backup_label: &str,
    kind: JournalPathKind,
) -> JournalPath {
    let name = target.file_name().unwrap().to_string_lossy();
    JournalPath {
        label: label.to_owned(),
        staged: target.with_file_name(format!(".{name}.ctx-upgrade-{attempt_id}.new")),
        backup: journal::transaction_backup_path(&target, attempt_id, backup_label),
        target,
        kind,
        target_preexisted: false,
        state: JournalPathState::Staged,
        staged_identity: Some(JournalPathIdentity {
            device: 1,
            inode: 1,
            length: 1,
        }),
        original_target_identity: None,
        backup_identity: None,
    }
}

fn coreml_semantic_journal(root: &std::path::Path) -> InstallTransactionJournal {
    let attempt_id = "coreml-clean";
    let cache_root = root.join("semantic-model-cache");
    let runtime_root = root.join("runtime");
    let manifest_sha = "a".repeat(64);
    let bundle = cache_root
        .join("semantic-model-bundles")
        .join("sha256")
        .join("aa")
        .join(&manifest_sha);
    let paths = vec![
        semantic_journal_path(
            attempt_id,
            "Semantic model",
            crate::semantic::semantic_managed_model_snapshot_dir(&cache_root),
            "model",
            JournalPathKind::Directory,
        ),
        semantic_journal_path(
            attempt_id,
            "Semantic CPU runtime",
            runtime_root
                .join("onnxruntime")
                .join("1.27.0")
                .join("macos-arm64"),
            "cpu-runtime",
            JournalPathKind::Directory,
        ),
        semantic_journal_path(
            attempt_id,
            "Semantic Core ML bundle",
            bundle.clone(),
            "coreml-bundle",
            JournalPathKind::Directory,
        ),
        semantic_journal_path(
            attempt_id,
            "Semantic Core ML completion marker",
            bundle.with_file_name(format!("{manifest_sha}.complete.json")),
            "coreml-marker",
            JournalPathKind::File,
        ),
    ];
    let mut journal = InstallTransactionJournal::new(
        attempt_id.to_owned(),
        root.to_path_buf(),
        runtime_root,
        root.join("ctx"),
        paths,
        None,
    );
    journal.semantic_cache_root = Some(cache_root);
    journal
}

#[test]
fn coreml_clean_install_journals_exact_fallback_composition_and_paths() {
    let temp = tempfile::tempdir().unwrap();
    let journal = coreml_semantic_journal(temp.path());

    journal::validate_paths_for_platform_for_test(&journal, "macos-arm64").unwrap();

    for missing in [
        "Semantic model",
        "Semantic CPU runtime",
        "Semantic Core ML bundle",
        "Semantic Core ML completion marker",
    ] {
        let mut incomplete = coreml_semantic_journal(temp.path());
        incomplete.paths.retain(|path| path.label != missing);
        assert!(
            journal::validate_paths_for_platform_for_test(&incomplete, "macos-arm64").is_err(),
            "missing {missing} must invalidate Core ML publication"
        );
    }
    assert!(
        journal::validate_paths_for_platform_for_test(&journal, "linux-x64").is_err(),
        "Core ML publication must remain Apple-only"
    );
}

#[test]
fn coreml_clean_install_rejects_every_path_outside_its_authority_root() {
    let temp = tempfile::tempdir().unwrap();
    for (label, backup_label, kind) in [
        ("Semantic model", "model", JournalPathKind::Directory),
        (
            "Semantic CPU runtime",
            "cpu-runtime",
            JournalPathKind::Directory,
        ),
        (
            "Semantic Core ML bundle",
            "coreml-bundle",
            JournalPathKind::Directory,
        ),
        (
            "Semantic Core ML completion marker",
            "coreml-marker",
            JournalPathKind::File,
        ),
    ] {
        let mut journal = coreml_semantic_journal(temp.path());
        let attempt_id = journal.attempt_id.clone();
        let path = journal
            .paths
            .iter_mut()
            .find(|path| path.label == label)
            .unwrap();
        *path = semantic_journal_path(
            &attempt_id,
            label,
            temp.path()
                .join("outside-authority")
                .join(label.replace(' ', "-")),
            backup_label,
            kind,
        );

        assert!(
            journal::validate_paths_for_platform_for_test(&journal, "macos-arm64").is_err(),
            "{label} outside its signed authority root must be rejected"
        );
    }
}

#[cfg(unix)]
#[test]
fn coreml_clean_install_recovery_removes_all_four_new_paths() {
    let temp = tempdir().unwrap();
    let mut journal = coreml_semantic_journal(temp.path());
    for path in &mut journal.paths {
        match path.kind {
            JournalPathKind::Directory => fs::create_dir_all(&path.target).unwrap(),
            JournalPathKind::File => {
                fs::create_dir_all(path.target.parent().unwrap()).unwrap();
                fs::write(&path.target, b"marker").unwrap();
            }
        }
        path.state = JournalPathState::Published;
        path.staged_identity = Some(super::unix::path_identity_for_test(&path.target).unwrap());
    }
    let targets = journal
        .paths
        .iter()
        .map(|path| path.target.clone())
        .collect::<Vec<_>>();

    super::unix::rollback_paths_for_test(&journal.paths, &journal.install_path).unwrap();

    assert!(targets.iter().all(|target| !target.exists()));
}

#[test]
fn journal_v2_persists_only_transaction_authority() {
    let transaction = InstallTransactionJournal::new(
        "0123456789abcdef0123456789abcdef".to_owned(),
        "/tmp/data".into(),
        "/tmp/data/runtime".into(),
        "/tmp/ctx".into(),
        vec![JournalPath {
            label: "ctx binary".to_owned(),
            staged: "/tmp/.ctx-upgrade.new".into(),
            target: "/tmp/ctx".into(),
            backup: "/tmp/.ctx.previous".into(),
            kind: JournalPathKind::File,
            target_preexisted: false,
            state: JournalPathState::Staged,
            staged_identity: None,
            original_target_identity: None,
            backup_identity: None,
        }],
        None,
    );

    let value = serde_json::to_value(transaction).unwrap();
    assert_eq!(value["schema_version"], 2);
    assert_eq!(value["phase"], "prepared");
    assert_eq!(value["paths"][0]["target_preexisted"], false);
    assert_eq!(value["paths"][0]["state"], "staged");
    assert!(value.get("attempt_generation").is_none());
    assert!(value.get("scheduler").is_none());
    assert!(value.get("telemetry").is_none());
}

#[test]
fn journal_v2_rejects_unreleased_intermediate_fields() {
    let transaction = InstallTransactionJournal::new(
        "0123456789abcdef0123456789abcdef".to_owned(),
        "/tmp/data".into(),
        "/tmp/data/runtime".into(),
        "/tmp/ctx".into(),
        vec![JournalPath {
            label: "ctx binary".to_owned(),
            staged: "/tmp/.ctx-upgrade.new".into(),
            target: "/tmp/ctx".into(),
            backup: "/tmp/.ctx.previous".into(),
            kind: JournalPathKind::File,
            target_preexisted: false,
            state: JournalPathState::Staged,
            staged_identity: None,
            original_target_identity: None,
            backup_identity: None,
        }],
        None,
    );
    let mut value = serde_json::to_value(transaction).unwrap();
    value["ownership_token"] = serde_json::json!("unreleased-shape");

    assert!(serde_json::from_value::<InstallTransactionJournal>(value).is_err());
}

#[test]
fn journal_v2_rejects_other_unshipped_schema_versions() {
    let transaction = InstallTransactionJournal::new(
        "0123456789abcdef0123456789abcdef".to_owned(),
        "/tmp/data".into(),
        "/tmp/data/runtime".into(),
        "/tmp/ctx".into(),
        vec![JournalPath {
            label: "ctx binary".to_owned(),
            staged: "/tmp/.ctx-upgrade.new".into(),
            target: "/tmp/ctx".into(),
            backup: "/tmp/.ctx.previous".into(),
            kind: JournalPathKind::File,
            target_preexisted: false,
            state: JournalPathState::Staged,
            staged_identity: None,
            original_target_identity: None,
            backup_identity: None,
        }],
        None,
    );
    let mut value = serde_json::to_value(transaction).unwrap();
    value["schema_version"] = serde_json::json!(3);

    let decoded = serde_json::from_value::<InstallTransactionJournal>(value).unwrap();
    assert!(journal::validate(&decoded).is_err());
}

#[cfg(unix)]
#[test]
fn data_root_scoped_state_is_not_install_recovery_authority() {
    let temp = tempdir().unwrap();
    let journal_path = temp.path().join("upgrade-install-transaction.json");
    let unrelated_state = b"not executable-scoped recovery authority";
    let unrelated_backup = temp.path().join("unowned-ctx-backup");
    fs::write(&journal_path, unrelated_state).unwrap();
    fs::write(&unrelated_backup, b"unowned binary").unwrap();

    assert!(super::pending_recovery(temp.path()).unwrap().is_none());
    assert_eq!(fs::read(&journal_path).unwrap(), unrelated_state);
    assert_eq!(fs::read(&unrelated_backup).unwrap(), b"unowned binary");
}

#[cfg(unix)]
#[test]
fn unsupported_schema_is_rejected_at_current_journal_path() {
    let temp = tempdir().unwrap();
    let mut transaction = coreml_semantic_journal(temp.path());
    transaction.schema_version = 1;
    let install_path = transaction.install_path.clone();
    let journal_path = journal::install_transaction_path(&install_path);
    journal::write(&transaction).unwrap();
    let encoded = fs::read(&journal_path).unwrap();

    let decoded = journal::read(&install_path).unwrap().unwrap();
    let error = journal::validate(&decoded).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("invalid install transaction identity"),
        "{error:#}"
    );
    assert_eq!(fs::read(&journal_path).unwrap(), encoded);
}

#[test]
fn journal_phase_rejects_power_loss_ambiguous_preexistence_state() {
    let mut transaction = InstallTransactionJournal::new(
        "0123456789abcdef0123456789abcdef".to_owned(),
        "/tmp/data".into(),
        "/tmp/data/runtime".into(),
        "/tmp/ctx".into(),
        vec![JournalPath {
            label: "ctx binary".to_owned(),
            staged: "/tmp/.ctx-upgrade.new".into(),
            target: "/tmp/ctx".into(),
            backup: "/tmp/.ctx.previous".into(),
            kind: JournalPathKind::File,
            target_preexisted: false,
            state: JournalPathState::BackedUp,
            staged_identity: None,
            original_target_identity: None,
            backup_identity: None,
        }],
        None,
    );
    transaction.phase = JournalPhase::Publishing;

    assert!(journal::validate_phase_state(&transaction).is_err());
}

#[test]
fn attempt_identity_rejects_path_syntax_and_accepts_state_ids() {
    assert!(journal::is_valid_attempt_id(
        "0123456789abcdef0123456789abcdef"
    ));
    assert!(journal::is_valid_attempt_id("upgrade-attempt.42"));
    assert!(!journal::is_valid_attempt_id("../replacement"));
    assert!(!journal::is_valid_attempt_id(""));
}

#[cfg(unix)]
#[test]
fn schema_two_rollback_restores_current_format_executable_and_reports_reexec_target() {
    let temp = tempdir().unwrap();
    let target = temp.path().join("ctx");
    let staged = temp.path().join(".ctx-upgrade-attempt.new");
    let backup = temp.path().join(".ctx-upgrade-attempt.previous");
    fs::write(&target, b"current-format").unwrap();
    fs::write(&staged, b"replacement").unwrap();
    let current_format_identity = super::unix::path_identity_for_test(&target).unwrap();
    let staged_identity = super::unix::path_identity_for_test(&staged).unwrap();
    fs::hard_link(&target, &backup).unwrap();
    fs::rename(&staged, &target).unwrap();
    let path = JournalPath {
        label: "ctx binary".to_owned(),
        staged,
        target: target.clone(),
        backup,
        kind: JournalPathKind::File,
        target_preexisted: true,
        state: JournalPathState::Published,
        staged_identity: Some(staged_identity),
        original_target_identity: Some(current_format_identity),
        backup_identity: Some(current_format_identity),
    };

    let restored = super::unix::rollback_paths_for_test(&[path], &target).unwrap();
    assert_eq!(restored.as_deref(), Some(target.as_path()));
    assert_eq!(fs::read(&target).unwrap(), b"current-format");

    let recovery_lock = super::super::InstallationLock::try_acquire_for_recovery(&target)
        .unwrap()
        .expect("recovery lock");
    assert!(
        super::super::InstallationLock::try_acquire(&target)
            .unwrap()
            .is_none(),
        "restored target must remain fenced until recovery releases its lock"
    );
    drop(recovery_lock);
    assert!(
        super::super::InstallationLock::try_acquire(&target)
            .unwrap()
            .is_some(),
        "re-executed target must be able to reacquire its installation lock"
    );
}

#[cfg(unix)]
fn interrupted_schema_two_binary_publication(
    root: &std::path::Path,
    phase: JournalPhase,
) -> (
    InstallTransactionJournal,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let attempt_id = "current-format-attempt";
    let target = root.join("ctx");
    let marker = root.join("ctx.install.json");
    let staged = root.join(format!(".ctx-upgrade-{attempt_id}.new"));
    let marker_staged = root.join(format!(".ctx-upgrade-{attempt_id}.install.json.new"));
    let backup = journal::transaction_backup_path(&target, attempt_id, "binary");
    let marker_backup = journal::transaction_backup_path(&marker, attempt_id, "marker");

    fs::write(&target, b"current-format").unwrap();
    fs::write(&marker, b"current-format-marker").unwrap();
    fs::write(&staged, b"replacement").unwrap();
    fs::write(&marker_staged, b"replacement-marker").unwrap();

    let current_format_identity = super::unix::path_identity_for_test(&target).unwrap();
    let current_marker_identity = super::unix::path_identity_for_test(&marker).unwrap();
    let staged_identity = super::unix::path_identity_for_test(&staged).unwrap();
    let staged_marker_identity = super::unix::path_identity_for_test(&marker_staged).unwrap();

    fs::hard_link(&target, &backup).unwrap();
    fs::rename(&staged, &target).unwrap();
    fs::hard_link(&marker, &marker_backup).unwrap();
    fs::rename(&marker_staged, &marker).unwrap();

    let mut transaction = InstallTransactionJournal::new(
        attempt_id.to_owned(),
        root.to_path_buf(),
        root.join("runtime"),
        target.clone(),
        vec![
            JournalPath {
                label: "ctx binary".to_owned(),
                staged,
                target: target.clone(),
                backup,
                kind: JournalPathKind::File,
                target_preexisted: true,
                state: JournalPathState::Published,
                staged_identity: Some(staged_identity),
                original_target_identity: Some(current_format_identity),
                backup_identity: Some(current_format_identity),
            },
            JournalPath {
                label: "ctx install marker".to_owned(),
                staged: marker_staged,
                target: marker.clone(),
                backup: marker_backup,
                kind: JournalPathKind::File,
                target_preexisted: true,
                state: JournalPathState::Published,
                staged_identity: Some(staged_marker_identity),
                original_target_identity: Some(current_marker_identity),
                backup_identity: Some(current_marker_identity),
            },
        ],
        None,
    );
    transaction.phase = phase;
    journal::write(&transaction).unwrap();
    (transaction, target, marker)
}

#[cfg(unix)]
#[test]
fn schema_two_interrupted_publication_recovers_by_rolling_back_current_format_install() {
    let temp = tempdir().unwrap();
    let (mut transaction, target, marker) =
        interrupted_schema_two_binary_publication(temp.path(), JournalPhase::Publishing);

    let outcome = super::unix::recover_transaction(temp.path(), &mut transaction).unwrap();

    assert_eq!(
        outcome,
        RecoveryOutcome::RolledBack {
            restored_executable: Some(target.clone()),
        }
    );
    assert_eq!(fs::read(&target).unwrap(), b"current-format");
    assert_eq!(fs::read(&marker).unwrap(), b"current-format-marker");
    assert!(!journal::install_transaction_path(&target).exists());
}

#[cfg(unix)]
#[test]
fn schema_two_interrupted_commit_keeps_published_install_and_discards_old_binary() {
    let temp = tempdir().unwrap();
    let (mut transaction, target, marker) =
        interrupted_schema_two_binary_publication(temp.path(), JournalPhase::Committed);
    fs::write(temp.path().join("ctx.previous"), b"older-release").unwrap();

    let outcome = super::unix::recover_transaction(temp.path(), &mut transaction).unwrap();

    assert_eq!(outcome, RecoveryOutcome::Committed);
    assert_eq!(fs::read(&target).unwrap(), b"replacement");
    assert_eq!(fs::read(&marker).unwrap(), b"replacement-marker");
    assert!(!temp.path().join("ctx.previous").exists());
    assert!(!journal::install_transaction_path(&target).exists());
}

#[cfg(unix)]
#[test]
fn committed_retry_discards_durable_old_binary_after_transaction_backup_is_gone() {
    let temp = tempdir().unwrap();
    let (mut transaction, target, _marker) =
        interrupted_schema_two_binary_publication(temp.path(), JournalPhase::Committed);
    let binary_backup = transaction
        .paths
        .iter()
        .find(|path| path.label == "ctx binary")
        .unwrap()
        .backup
        .clone();
    fs::remove_file(binary_backup).unwrap();
    fs::write(temp.path().join("ctx.previous"), b"older-release").unwrap();

    let outcome = super::unix::recover_transaction(temp.path(), &mut transaction).unwrap();

    assert_eq!(outcome, RecoveryOutcome::Committed);
    assert_eq!(fs::read(&target).unwrap(), b"replacement");
    assert!(!temp.path().join("ctx.previous").exists());
    assert!(!journal::install_transaction_path(&target).exists());
}

#[cfg(unix)]
#[test]
fn legacy_bridge_discards_owner_regular_durable_old_binary() {
    let temp = tempdir().unwrap();
    let target = temp.path().join("ctx");
    fs::write(&target, b"replacement").unwrap();
    fs::write(temp.path().join("ctx.previous"), b"v0.25").unwrap();

    super::discard_legacy_previous_binary(&target).unwrap();

    assert_eq!(fs::read(target).unwrap(), b"replacement");
    assert!(!temp.path().join("ctx.previous").exists());
}

#[cfg(unix)]
#[test]
fn unix_rollback_refuses_to_delete_replacement_target() {
    let temp = tempdir().unwrap();
    let target = temp.path().join("ctx");
    let staged = temp.path().join(".ctx-upgrade-attempt.new");
    fs::write(&staged, b"ours").unwrap();
    let staged_identity = super::unix::path_identity_for_test(&staged).unwrap();
    fs::rename(&staged, &target).unwrap();
    fs::remove_file(&target).unwrap();
    fs::write(&target, b"replacement").unwrap();
    let path = JournalPath {
        label: "ctx binary".to_owned(),
        staged,
        target: target.clone(),
        backup: temp.path().join(".ctx-upgrade-attempt.previous"),
        kind: JournalPathKind::File,
        target_preexisted: false,
        state: JournalPathState::Published,
        staged_identity: Some(staged_identity),
        original_target_identity: None,
        backup_identity: None,
    };

    assert!(super::unix::rollback_paths_for_test(&[path], &target).is_err());
    assert_eq!(fs::read(&target).unwrap(), b"replacement");
}

#[cfg(unix)]
#[test]
fn unix_cleanup_refuses_even_a_broken_owner_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().unwrap();
    let path = temp.path().join("staged");
    symlink(temp.path().join("missing"), &path).unwrap();

    assert!(super::unix::remove_owner_regular_file(&path).is_err());
    assert!(fs::symlink_metadata(&path)
        .unwrap()
        .file_type()
        .is_symlink());
}

#[cfg(unix)]
#[test]
fn committed_cleanup_failure_is_success_with_cleanup_pending_warning() {
    let temp = tempdir().unwrap();
    let target = temp.path().join("ctx");
    let staged = temp.path().join(".ctx-upgrade-attempt.new");
    let backup = temp.path().join(".ctx-upgrade-attempt.previous");
    fs::write(&target, b"new").unwrap();
    fs::write(&backup, b"old").unwrap();
    fs::create_dir(temp.path().join("ctx.previous")).unwrap();
    let target_identity = super::unix::path_identity_for_test(&target).unwrap();
    let backup_identity = super::unix::path_identity_for_test(&backup).unwrap();
    let mut transaction = InstallTransactionJournal::new(
        "attempt".to_owned(),
        temp.path().to_path_buf(),
        temp.path().join("runtime"),
        target.clone(),
        vec![JournalPath {
            label: "ctx binary".to_owned(),
            staged,
            target,
            backup,
            kind: JournalPathKind::File,
            target_preexisted: true,
            state: JournalPathState::Published,
            staged_identity: Some(target_identity),
            original_target_identity: Some(backup_identity),
            backup_identity: Some(backup_identity),
        }],
        None,
    );
    transaction.phase = JournalPhase::Committed;

    let outcome = super::unix::finish_committed_for_test(temp.path(), &mut transaction).unwrap();
    assert!(matches!(outcome, RecoveryOutcome::CleanupPending { .. }));
    assert_eq!(transaction.phase, JournalPhase::CleanupPending);
    assert!(journal::install_transaction_path(&transaction.install_path).is_file());
    assert_eq!(fs::read(temp.path().join("ctx")).unwrap(), b"new");
}

#[cfg(unix)]
#[test]
fn committed_target_corruption_is_not_downgraded_to_cleanup_pending() {
    let temp = tempdir().unwrap();
    let target = temp.path().join("ctx");
    let backup = temp.path().join(".ctx-upgrade-attempt.previous");
    fs::write(&target, b"new").unwrap();
    fs::write(&backup, b"old").unwrap();
    let staged_identity = super::unix::path_identity_for_test(&target).unwrap();
    let backup_identity = super::unix::path_identity_for_test(&backup).unwrap();
    fs::remove_file(&target).unwrap();
    fs::write(&target, b"replacement").unwrap();
    let mut transaction = InstallTransactionJournal::new(
        "attempt".to_owned(),
        temp.path().to_path_buf(),
        temp.path().join("runtime"),
        target.clone(),
        vec![JournalPath {
            label: "ctx binary".to_owned(),
            staged: temp.path().join(".ctx-upgrade-attempt.new"),
            target,
            backup,
            kind: JournalPathKind::File,
            target_preexisted: true,
            state: JournalPathState::Published,
            staged_identity: Some(staged_identity),
            original_target_identity: Some(backup_identity),
            backup_identity: Some(backup_identity),
        }],
        None,
    );
    transaction.phase = JournalPhase::Committed;
    assert!(super::unix::finish_committed_for_test(temp.path(), &mut transaction).is_err());
    assert_eq!(transaction.phase, JournalPhase::Committed);
}
