#![cfg(unix)]

use super::*;
use crate::upgrade::{
    install::{install_marker_path, InstallFingerprint, InstallationLock},
    metadata::{ManagedPairReleaseMetadata, ReleaseMetadata},
    sha256_hex,
    state::{managed_pair_recovery_locked, write_managed_pair_attempt_locked},
};
use crate::DaemonRestart;
use anyhow::anyhow as anyhow_error;
use serde_json::Value;
use std::{fs, os::unix::fs::PermissionsExt as _, sync::Mutex};

static APPLIED_STATE_WRITE_ENV_LOCK: Mutex<()> = Mutex::new(());

#[derive(Default)]
struct RecordingObserver {
    terminals: Mutex<Vec<(String, UpgradeTerminalStatus, bool)>>,
    warnings: Mutex<Vec<String>>,
}

impl AutomaticUpgradePolicySnapshot for () {
    fn daemon_maintenance_enabled(&self) -> bool {
        true
    }
    fn automatic_upgrade_enabled(&self) -> bool {
        true
    }
    fn interval(&self) -> Duration {
        Duration::from_secs(60)
    }
    fn channel(&self) -> &str {
        "stable"
    }
    fn semantic_enabled(&self) -> bool {
        false
    }
}

impl UpgradeObserver<()> for RecordingObserver {
    fn observe_automatic_warnings(&self, _data_root: &Path, _policy: &(), warnings: &[String]) {
        self.warnings.lock().unwrap().extend_from_slice(warnings);
    }

    fn observe_automatic_terminal(
        &self,
        _data_root: &Path,
        _policy: &(),
        observation: AutomaticUpgradeObservation<'_>,
    ) {
        self.terminals.lock().unwrap().push((
            observation.attempt_id.to_owned(),
            observation.status,
            observation.applied,
        ));
    }
}

impl DaemonUpgradeLease for Result<()> {
    fn wait_for_installation_quiescence(&self) -> Result<()> {
        Ok(())
    }
    fn replacement_restart(&self) -> Option<DaemonRestart<'_>> {
        None
    }
    fn resume_with(self, _executable: &Path) -> Result<()> {
        self
    }
    fn transfer_to_replacement_helper(self, _helper_pid: u32) -> Result<()> {
        unreachable!()
    }
    fn release_for_current_format_reexec(self) -> Result<()> {
        unreachable!()
    }
}

fn automatic_fixture(
    managed_pair: bool,
) -> (
    tempfile::TempDir,
    PathBuf,
    PathBuf,
    UpgradeLock,
    UpgradeAttempt,
    UpgradePlan,
) {
    let temp = tempfile::tempdir().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let install = temp.path().join("ctx");
    let data_root = temp.path().join("data");
    fs::create_dir(&data_root).unwrap();
    fs::write(&install, b"applied core").unwrap();
    fs::set_permissions(&install, fs::Permissions::from_mode(0o700)).unwrap();
    let marker_path = install_marker_path(&install);
    fs::write(&marker_path, b"marker").unwrap();
    let plan = UpgradePlan {
        current_version: "1.0.0".to_owned(),
        latest_version: "1.1.0".to_owned(),
        channel: "stable".to_owned(),
        platform: platform_key().unwrap().to_owned(),
        metadata_url: "metadata".to_owned(),
        artifact_url: "artifact".to_owned(),
        artifact_sha256: sha256_hex(b"applied core"),
        install_path: install.clone(),
        install_fingerprint: InstallFingerprint {
            binary_sha256: sha256_hex(b"applied core"),
            marker_sha256: sha256_hex(b"marker"),
        },
        update_available: true,
        managed: true,
        warnings: Vec::new(),
        managed_pair_release: managed_pair.then(|| ManagedPairReleaseMetadata {
            envelope_url: "pair-envelope".to_owned(),
            core_object_url: "pair-core".to_owned(),
            core_sha256: "a".repeat(64),
            companion_object_url: "pair-companion".to_owned(),
            companion_sha256: "c".repeat(64),
        }),
        metadata: ReleaseMetadata {
            version: "1.1.0".to_owned(),
            base_url: "releases".to_owned(),
            artifact: "ctx".to_owned(),
            sha256: sha256_hex(b"applied core"),
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
    };
    let installation = InstallationLock::try_acquire(&install).unwrap().unwrap();
    let lock = UpgradeLock::from_installation_for_test(install.clone(), installation);
    let attempt = begin_automatic_attempt_locked(&lock, ().interval())
        .unwrap()
        .unwrap();
    if managed_pair {
        assert!(write_managed_pair_attempt_locked(
            &data_root,
            &lock,
            &attempt,
            &plan,
            "applying",
            ().interval(),
            None,
            &"b".repeat(64),
        )
        .unwrap());
    } else {
        assert!(write_state_checked_locked(
            &data_root,
            &lock,
            &attempt,
            &plan,
            "applying",
            ().interval(),
        )
        .unwrap());
    }
    (temp, data_root, install, lock, attempt, plan)
}

fn state_value(install: &Path) -> Value {
    let name = install.file_name().unwrap().to_string_lossy();
    serde_json::from_slice(
        &fs::read(install.with_file_name(format!(".{name}.upgrade-state.json"))).unwrap(),
    )
    .unwrap()
}

#[test]
fn post_apply_state_and_restart_faults_recover_one_truthful_applied_event() {
    let _env_lock = APPLIED_STATE_WRITE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_temp, data_root, install, lock, attempt, plan) = automatic_fixture(true);
    let observer = RecordingObserver::default();
    let fault_key = "CTX_UPGRADE_FAIL_APPLIED_STATE_WRITE_FOR_TESTS";
    let previous_fault = std::env::var_os(fault_key);
    std::env::set_var(fault_key, attempt.id());
    finalize_automatic_applied(
        &data_root,
        ().interval(),
        Instant::now(),
        lock,
        &attempt,
        &plan,
        &(),
        &observer,
        Err(anyhow_error!("injected daemon restart failure")),
        None,
    )
    .unwrap();
    match previous_fault {
        Some(value) => std::env::set_var(fault_key, value),
        None => std::env::remove_var(fault_key),
    }
    assert!(observer.terminals.lock().unwrap().is_empty());
    assert_eq!(state_value(&install)["status"], "applying");

    let installation = InstallationLock::try_acquire(&install).unwrap().unwrap();
    let recovery_lock = UpgradeLock::from_installation_for_test(install.clone(), installation);
    let recovery = managed_pair_recovery_locked(&recovery_lock, attempt.id()).unwrap();
    finalize_automatic_recovery(
        recovery_lock,
        &recovery.data_root,
        &recovery.install_path,
        &recovery.attempt_id,
        recovery.interval,
        true,
        None,
        false,
        Err(anyhow_error!("injected recovery restart failure")),
        &observer,
        &(),
        Instant::now(),
    )
    .unwrap();

    assert_eq!(
        *observer.terminals.lock().unwrap(),
        [(
            attempt.id().to_owned(),
            UpgradeTerminalStatus::Applied,
            true
        )]
    );
    assert_eq!(
        *observer.warnings.lock().unwrap(),
        [
            "ctx upgrade is applied, but applied-state finalization is pending: injected applied-state write failure",
            "ctx upgrade is applied, but daemon restart is pending: injected daemon restart failure",
            "ctx upgrade is applied, but daemon restart is pending: injected recovery restart failure",
        ]
    );
    let state = state_value(&install);
    assert_eq!(state["status"], "applied");
    assert_eq!(state["current_version"], "1.1.0");
    assert_eq!(state["latest_version"], "1.1.0");
    assert_eq!(state["update_available"], false);
}

#[test]
fn generic_committed_recovery_state_fault_emits_one_truthful_applied_event() {
    let _env_lock = APPLIED_STATE_WRITE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_temp, data_root, install, lock, attempt, _plan) = automatic_fixture(false);
    let observer = RecordingObserver::default();
    let fault_key = "CTX_UPGRADE_FAIL_APPLIED_STATE_WRITE_FOR_TESTS";
    let previous_fault = std::env::var_os(fault_key);
    std::env::set_var(fault_key, attempt.id());
    finalize_automatic_recovery(
        lock,
        &data_root,
        &install,
        attempt.id(),
        ().interval(),
        true,
        None,
        true,
        Err(anyhow_error!("injected daemon restart failure")),
        &observer,
        &(),
        Instant::now(),
    )
    .unwrap();
    match previous_fault {
        Some(value) => std::env::set_var(fault_key, value),
        None => std::env::remove_var(fault_key),
    }
    assert_eq!(
        *observer.terminals.lock().unwrap(),
        [(
            attempt.id().to_owned(),
            UpgradeTerminalStatus::Applied,
            true
        )]
    );
    assert_eq!(
        *observer.warnings.lock().unwrap(),
        [
            "ctx upgrade is applied, but applied-state recovery is pending: injected applied-state write failure",
            "ctx upgrade is applied, but daemon restart is pending: injected daemon restart failure",
        ]
    );
    let state = state_value(&install);
    assert_eq!(state["status"], "applying");
    assert_eq!(state["current_version"], "1.0.0");
    assert_eq!(state["latest_version"], "1.1.0");
    assert_eq!(state["update_available"], true);
}

mod first_pair;
