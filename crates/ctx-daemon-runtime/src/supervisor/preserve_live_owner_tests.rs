use std::sync::Mutex;

use super::*;

struct ExistingOwnerBackend {
    existing_owner: Option<u32>,
    install_owner: Option<u32>,
    calls: Mutex<Vec<&'static str>>,
}

impl NativeSupervisorBackend<()> for ExistingOwnerBackend {
    fn probe_manager(&self, _: &Path) -> Result<SupervisorManagerOperability> {
        self.calls.lock().unwrap().push("probe_manager");
        Ok(SupervisorManagerOperability::Operational)
    }

    fn prepare_mutation(&self, _: &Path, _: &Path) -> Result<()> {
        self.calls.lock().unwrap().push("prepare_mutation");
        Ok(())
    }

    fn artifact_path(&self, _: &Path) -> Result<Option<PathBuf>> {
        self.calls.lock().unwrap().push("artifact_path");
        Ok(Some(PathBuf::from("/tmp/rs.ctx.daemon.plist")))
    }

    fn install(&self, _: &Path, _: &Path, _: &()) -> Result<PathBuf> {
        self.calls.lock().unwrap().push("install");
        Ok(PathBuf::from("/tmp/rs.ctx.daemon.plist"))
    }

    fn disable(&self, _: &Path) -> Result<Option<PathBuf>> {
        self.calls.lock().unwrap().push("disable");
        Ok(None)
    }

    fn verify_registration(&self, _: &Path, _: &Path) -> Result<()> {
        self.calls.lock().unwrap().push("verify_registration");
        Err(anyhow!(
            "supervisor environment does not match the maintained definition"
        ))
    }

    fn verify_live_owner(&self, _: &Path, _: &Path) -> Result<u32> {
        self.calls.lock().unwrap().push("verify_live_owner");
        self.install_owner
            .ok_or_else(|| anyhow!("registration mismatch"))
    }

    fn verify_existing_live_owner(&self, _: &Path, _: &Path) -> Result<u32> {
        self.calls
            .lock()
            .unwrap()
            .push("verify_existing_live_owner");
        self.existing_owner
            .ok_or_else(|| anyhow!("no live daemon owner"))
    }

    fn prepare_start(&self, _: &Path, _: &Path) -> Result<Option<u32>> {
        self.calls.lock().unwrap().push("prepare_start");
        Ok(None)
    }

    fn start(&self, _: &Path) -> Result<()> {
        self.calls.lock().unwrap().push("start");
        Ok(())
    }
}

#[test]
fn preserve_live_owner_policy_skips_reinstall_when_existing_owner_verifies() {
    let backend = ExistingOwnerBackend {
        existing_owner: Some(37_687),
        install_owner: None,
        calls: Mutex::new(Vec::new()),
    };
    let outcome = ensure_native_supervisor_with_policy(
        Path::new("/tmp/data"),
        Path::new("/tmp/ctx"),
        &(),
        &backend,
        SupervisorMutationPolicy::PreserveLiveOwner,
    )
    .unwrap();
    match outcome {
        SupervisorEnsureOutcome::LiveOwnerPreserved {
            artifact,
            owner_pid,
            registration_error,
        } => {
            assert_eq!(artifact, Some(PathBuf::from("/tmp/rs.ctx.daemon.plist")));
            assert_eq!(owner_pid, 37_687);
            assert!(registration_error
                .to_string()
                .contains("supervisor environment does not match"));
        }
        other => panic!("expected LiveOwnerPreserved, got {other:?}"),
    }
    assert_eq!(
        backend.calls.lock().unwrap().as_slice(),
        [
            "probe_manager",
            "artifact_path",
            "verify_registration",
            "verify_existing_live_owner",
        ]
    );
}

#[test]
fn preserve_live_owner_policy_reinstalls_when_no_live_owner_exists_and_records_reason() {
    let backend = ExistingOwnerBackend {
        existing_owner: None,
        install_owner: Some(16_149),
        calls: Mutex::new(Vec::new()),
    };
    let outcome = ensure_native_supervisor_with_policy(
        Path::new("/tmp/data"),
        Path::new("/tmp/ctx"),
        &(),
        &backend,
        SupervisorMutationPolicy::PreserveLiveOwner,
    )
    .unwrap();
    match outcome {
        SupervisorEnsureOutcome::Native {
            owner_pid,
            environment_installed,
            recovery_reason,
            ..
        } => {
            assert_eq!(owner_pid, 16_149);
            assert!(environment_installed);
            let reason = recovery_reason.expect("reinstall must record why it ran");
            assert!(reason.contains("reinstalled_registration"), "{reason}");
            assert!(
                reason.contains("supervisor environment does not match"),
                "{reason}"
            );
        }
        other => panic!("expected Native after reinstall, got {other:?}"),
    }
    assert!(backend.calls.lock().unwrap().contains(&"install"));
}
