use std::{cell::Cell, fs};

use super::*;
use crate::upgrade::install::transaction::journal::{
    JournalPath, JournalPathKind, JournalPathState, WindowsDaemonRestart, WindowsHelperJournal,
};

fn helper_journal(temp: &Path) -> WindowsHelperJournal {
    WindowsHelperJournal {
        parent_pid: 42,
        helper_pid: Some(43),
        helper_path: temp.join(".ctx.ctx-upgrade-attempt.helper.exe"),
        expected_binary_sha256: "a".repeat(64),
        expected_marker_sha256: "b".repeat(64),
        daemon_restart: None,
        failure: None,
        terminal: None,
    }
}

fn path_record(
    label: &str,
    staged: &Path,
    target: &Path,
    backup: &Path,
    state: JournalPathState,
) -> JournalPath {
    JournalPath {
        label: label.to_owned(),
        staged: staged.to_path_buf(),
        target: target.to_path_buf(),
        backup: backup.to_path_buf(),
        kind: JournalPathKind::File,
        target_preexisted: true,
        state,
        staged_identity: None,
        original_target_identity: None,
        backup_identity: None,
    }
}

fn transaction(temp: &Path, paths: Vec<JournalPath>) -> InstallTransactionJournal {
    InstallTransactionJournal::new(
        "attempt".to_owned(),
        temp.to_path_buf(),
        temp.join("runtime"),
        temp.join("ctx"),
        paths,
        Some(helper_journal(temp)),
    )
}

#[test]
fn readiness_receipt_is_exact_and_bounded() {
    let receipt = protocol::ready_receipt("attempt", 43);
    protocol::validate_ready_receipt(&receipt, "attempt", 43).unwrap();
    assert!(protocol::validate_ready_receipt("ready\n", "attempt", 43).is_err());
}

#[test]
fn helper_journal_contains_transaction_and_restart_data_only() {
    let temp = tempfile::tempdir().unwrap();
    let value = serde_json::to_value(helper_journal(temp.path())).unwrap();
    let object = value.as_object().unwrap();
    for obsolete in [
        "current_version",
        "latest_version",
        "channel",
        "platform",
        "metadata_url",
        "artifact_url",
        "self_upgrade_allowed",
        "auto_upgrade_allowed",
        "attempt_origin",
        "telemetry_event_id",
        "scheduler_recorded",
        "telemetry_attempted",
    ] {
        assert!(!object.contains_key(obsolete), "{obsolete}");
    }
}

#[test]
fn new_restart_descriptor_serializes_persistent_trigger_and_maintenance_cadence() {
    let restart = WindowsDaemonRestart {
        trigger: "search".to_owned(),
        legacy_idle_exit_seconds: None,
        loop_interval_seconds: Some(23),
    };

    assert_eq!(
        serde_json::to_value(restart).unwrap(),
        serde_json::json!({"trigger": "search", "loop_interval_seconds": 23})
    );

    let default_cadence = WindowsDaemonRestart {
        trigger: "search".to_owned(),
        legacy_idle_exit_seconds: None,
        loop_interval_seconds: None,
    };
    assert_eq!(
        serde_json::to_value(default_cadence).unwrap(),
        serde_json::json!({"trigger": "search"})
    );
}

#[test]
fn current_format_journal_ignores_legacy_restart_timing_when_rewritten() {
    let temp = tempfile::tempdir().unwrap();
    let transaction = transaction(temp.path(), Vec::new());
    let mut value = serde_json::to_value(transaction).unwrap();
    value["windows_helper"]["daemon_restart"] = serde_json::json!({
        "trigger": "search",
        "idle_exit_seconds": 5,
        "loop_interval_seconds": 7,
    });

    let decoded: InstallTransactionJournal = serde_json::from_value(value).unwrap();
    let restart = decoded
        .windows_helper
        .as_ref()
        .and_then(|helper| helper.daemon_restart.as_ref())
        .unwrap();
    assert_eq!(restart.trigger, "search");
    assert_eq!(restart.legacy_idle_exit_seconds, Some(5));
    assert_eq!(restart.loop_interval_seconds, Some(7));

    let rewritten = serde_json::to_value(decoded).unwrap();
    let restart = rewritten["windows_helper"]["daemon_restart"]
        .as_object()
        .unwrap();
    assert_eq!(restart.len(), 2);
    assert_eq!(restart.get("trigger"), Some(&serde_json::json!("search")));
    assert_eq!(
        restart.get("loop_interval_seconds"),
        Some(&serde_json::json!(7))
    );
}

#[test]
fn terminal_journal_has_no_scheduler_or_telemetry_receipt() {
    let temp = tempfile::tempdir().unwrap();
    let mut transaction = transaction(temp.path(), Vec::new());
    transaction.phase = JournalPhase::Committed;
    finish_terminal(
        &mut transaction,
        WindowsTerminalOutcome::Applied,
        Some("cleanup pending".to_owned()),
    )
    .unwrap();
    let terminal = transaction
        .windows_helper
        .unwrap()
        .terminal
        .expect("terminal transaction");
    assert_eq!(terminal.outcome, WindowsTerminalOutcome::Applied);
    assert_eq!(
        terminal.warning_or_error.as_deref(),
        Some("cleanup pending")
    );
    let json = serde_json::to_value(terminal).unwrap();
    assert_eq!(json.as_object().unwrap().len(), 2);
}

#[test]
fn runtime_only_publication_does_not_exempt_install_fingerprint_revalidation() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("ctx");
    let marker = super::super::super::marker::install_marker_path(&target);
    fs::write(&target, b"original executable").unwrap();
    fs::write(&marker, b"original marker").unwrap();
    let fingerprint = super::super::super::marker::install_fingerprint(&target).unwrap();

    let staged = temp.path().join("runtime.staged");
    let runtime = temp.path().join("runtime");
    let backup = temp.path().join("runtime.backup");
    fs::write(&staged, b"new runtime").unwrap();
    fs::write(&runtime, b"old runtime").unwrap();
    fs::write(&backup, b"old runtime").unwrap();
    let runtime_path = path_record(
        "ONNX Runtime sidecar",
        &staged,
        &runtime,
        &backup,
        JournalPathState::BackedUp,
    );
    let mut transaction = transaction(temp.path(), vec![runtime_path]);
    transaction.install_path = target.clone();
    let helper = transaction.windows_helper.as_mut().unwrap();
    helper.expected_binary_sha256 = fingerprint.binary_sha256;
    helper.expected_marker_sha256 = fingerprint.marker_sha256;

    layout::revalidate_fingerprint(&transaction).unwrap();
    fs::write(&target, b"replacement executable").unwrap();
    assert!(layout::revalidate_fingerprint(&transaction).is_err());
    fs::write(&target, b"original executable").unwrap();
    fs::write(&marker, b"replacement marker").unwrap();
    assert!(layout::revalidate_fingerprint(&transaction).is_err());
}

#[test]
fn binary_publication_retains_the_fingerprint_revalidation_exemption() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("ctx");
    let staged = temp.path().join("ctx.new");
    let backup = temp.path().join("ctx.backup");
    fs::write(&target, b"published executable").unwrap();
    fs::write(&backup, b"original executable").unwrap();
    let binary_path = path_record(
        "ctx binary",
        &staged,
        &target,
        &backup,
        JournalPathState::Published,
    );
    let mut transaction = transaction(temp.path(), vec![binary_path]);
    transaction.install_path = target;

    layout::revalidate_fingerprint(&transaction).unwrap();
}

#[test]
fn failed_replace_repairs_missing_executable_before_any_wait() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("ctx");
    let staged = temp.path().join("ctx.new");
    let backup = temp.path().join("ctx.backup");
    fs::write(&target, b"old").unwrap();
    fs::write(&staged, b"new").unwrap();
    fs::write(&backup, b"old").unwrap();
    let path = path_record(
        "ctx binary",
        &staged,
        &target,
        &backup,
        JournalPathState::BackedUp,
    );
    let mut transaction = transaction(temp.path(), vec![path.clone()]);
    let waits = Cell::new(0);
    let error = layout::replace_binary_with_repair(
        &mut transaction,
        0,
        &path,
        |target, _| {
            fs::remove_file(target)?;
            Err(std::io::Error::other("injected ReplaceFileW failure"))
        },
        || waits.set(waits.get() + 1),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("injected"));
    assert_eq!(waits.get(), 0);
    assert_eq!(fs::read(&target).unwrap(), b"old");
    assert_eq!(transaction.paths[0].state, JournalPathState::Staged);
}

#[test]
fn retry_waits_only_while_target_and_staged_remain_safe() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("ctx");
    let staged = temp.path().join("ctx.new");
    let backup = temp.path().join("ctx.backup");
    fs::write(&target, b"old").unwrap();
    fs::write(&staged, b"new").unwrap();
    fs::write(&backup, b"old").unwrap();
    let path = path_record(
        "ctx binary",
        &staged,
        &target,
        &backup,
        JournalPathState::BackedUp,
    );
    let mut transaction = transaction(temp.path(), vec![path.clone()]);
    let calls = Cell::new(0);
    let waits = Cell::new(0);
    layout::replace_binary_with_repair(
        &mut transaction,
        0,
        &path,
        |_, _| {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                Err(std::io::Error::other("sharing violation"))
            } else {
                Ok(())
            }
        },
        || waits.set(waits.get() + 1),
    )
    .unwrap();
    assert_eq!(calls.get(), 2);
    assert_eq!(waits.get(), 1);
}
