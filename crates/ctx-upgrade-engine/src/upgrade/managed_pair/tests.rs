use std::{fs, sync::Mutex, time::Duration};

use ctx_history_platform::platform_security::restrict_private_directory;
use sha2::{Digest as _, Sha256};
use tempfile::tempdir;

use super::*;

thread_local! {
    static DISPATCH_VERIFIER: std::cell::RefCell<Option<(ReleaseChannel, Vec<u8>, VerifiedManagedPairIdentity)>> = const { std::cell::RefCell::new(None) };
}

pub(in crate::upgrade) fn with_fixture_verifier<T>(
    channel: ReleaseChannel,
    envelope: Vec<u8>,
    identity: VerifiedManagedPairIdentity,
    operation: impl FnOnce() -> T,
) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            DISPATCH_VERIFIER.with(|slot| {
                slot.replace(None);
            });
        }
    }
    DISPATCH_VERIFIER.with(|slot| {
        assert!(slot.replace(Some((channel, envelope, identity))).is_none());
    });
    let _reset = Reset;
    operation()
}

pub(super) fn verify_fixture(
    channel: ReleaseChannel,
    bytes: &[u8],
) -> Option<Result<VerifiedManagedPairIdentity>> {
    DISPATCH_VERIFIER.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|(expected_channel, envelope, identity)| {
                if channel != *expected_channel || bytes != envelope {
                    Err(anyhow!("fixture envelope/channel rejected"))
                } else {
                    Ok(identity.clone())
                }
            })
    })
}

struct RecordingLease;

impl DaemonUpgradeLease for RecordingLease {
    fn wait_for_installation_quiescence(&self) -> Result<()> {
        Ok(())
    }

    fn replacement_restart(&self) -> Option<DaemonRestart<'_>> {
        None
    }

    fn resume_with(self, _executable: &Path) -> Result<()> {
        Ok(())
    }

    fn transfer_to_replacement_helper(self, _helper_pid: u32) -> Result<()> {
        Ok(())
    }

    fn release_for_current_format_reexec(self) -> Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct RecordingDaemon {
    calls: Mutex<Vec<&'static str>>,
}

impl DaemonUpgradePort for RecordingDaemon {
    type Lease = RecordingLease;

    fn begin(&self, _data_root: &Path, _attempt_id: &str) -> Result<Self::Lease> {
        Ok(RecordingLease)
    }

    fn begin_current(
        &self,
        _data_root: &Path,
        _attempt_id: &str,
        _restart_trigger: &str,
        _loop_interval_seconds: Option<u64>,
    ) -> Result<Self::Lease> {
        Ok(RecordingLease)
    }

    fn mark_replacement_helper_handoff(
        &self,
        _data_root: &Path,
        _attempt_id: &str,
        _helper_pid: u32,
    ) -> Result<()> {
        Ok(())
    }

    fn complete_replacement_handoff(
        &self,
        _data_root: &Path,
        _executable: &Path,
        _attempt_id: &str,
        _restart: Option<DaemonRestart<'_>>,
    ) -> Result<()> {
        self.calls.lock().unwrap().push("complete");
        Ok(())
    }

    fn finish_replacement_handoff(&self, _data_root: &Path, _attempt_id: &str) -> Result<()> {
        self.calls.lock().unwrap().push("finish");
        Ok(())
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[test]
fn missing_pending_record_after_windows_scheduler_publication_records_terminal_failure_and_resumes_daemon_handoff(
) -> Result<()> {
    let fixture = tempdir()?;
    restrict_private_directory(fixture.path())?;
    let data_root = fixture.path().join("data");
    fs::create_dir(&data_root)?;
    restrict_private_directory(&data_root)?;
    let install_path = fixture.path().join("ctx");
    fs::write(&install_path, b"core")?;
    let installation = InstallationLock::try_acquire(&install_path)?
        .ok_or_else(|| anyhow!("test installation lock is unavailable"))?;
    let lock = UpgradeLock::from_installation(install_path.clone(), installation);
    let recovery = ManagedPairRecovery {
        attempt_id: "recovery-boundary".to_owned(),
        data_root: data_root.clone(),
        install_path: install_path.clone(),
        channel: "stable".to_owned(),
        interval: Duration::from_secs(60),
        automatic: false,
        restart_trigger: Some("managed_pair_maintenance".to_owned()),
        restart_interval_seconds: Some(60),
        core_sha256: digest(b"core"),
        envelope_sha256: digest(b"envelope"),
        #[cfg(windows)]
        helper_path: None,
        #[cfg(windows)]
        helper_parent_pid: None,
    };
    let daemon = RecordingDaemon::default();

    finish_windows_managed_pair_helper_recovery(
        &daemon,
        &recovery,
        lock,
        &recovery.attempt_id,
        Some(DaemonRestart {
            trigger: "managed_pair_maintenance",
            loop_interval_seconds: Some(60),
        }),
        false,
        Some("managed-pair recovery record disappeared before publication"),
    )?;

    let state: serde_json::Value = serde_json::from_slice(&fs::read(
        install_path.with_file_name(".ctx.upgrade-state.json"),
    )?)?;
    assert_eq!(state["status"], "error");
    assert_eq!(
        state["error"],
        "managed-pair recovery record disappeared before publication"
    );
    assert_eq!(
        daemon.calls.lock().unwrap().as_slice(),
        ["complete", "finish"]
    );
    Ok(())
}
