use std::sync::Mutex;

use super::*;

enum ExistingOwnerMode {
    Verified(u32),
    Denied(u32),
    Absent,
}

enum LiveOwnerMode {
    Denied(u32),
    Absent,
}

struct ExistingOwnerBackend {
    registered: bool,
    live_owner: LiveOwnerMode,
    existing_owner: ExistingOwnerMode,
    install_owner: Option<u32>,
    start_owner: Option<u32>,
    calls: Mutex<Vec<&'static str>>,
}

impl ExistingOwnerBackend {
    fn new(registered: bool, live_owner: LiveOwnerMode, existing_owner: ExistingOwnerMode) -> Self {
        Self {
            registered,
            live_owner,
            existing_owner,
            install_owner: None,
            start_owner: None,
            calls: Mutex::new(Vec::new()),
        }
    }
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
        if self.registered {
            Ok(())
        } else {
            Err(anyhow!(
                "supervisor environment does not match the maintained definition"
            ))
        }
    }

    fn verify_live_owner(&self, _: &Path, _: &Path) -> Result<u32> {
        let mut calls = self.calls.lock().unwrap();
        calls.push("verify_live_owner");
        let installed = calls.contains(&"install");
        let started = calls.contains(&"start");
        drop(calls);
        if installed {
            if let Some(pid) = self.install_owner {
                return Ok(pid);
            }
        }
        if started {
            if let Some(pid) = self.start_owner {
                return Ok(pid);
            }
        }
        match self.live_owner {
            LiveOwnerMode::Denied(pid) => {
                Err(crate::ProcessExecutableInspectionDenied { pid }.into())
            }
            LiveOwnerMode::Absent => Err(anyhow!("manager has no live owner")),
        }
    }

    fn verify_existing_live_owner(&self, _: &Path, _: &Path) -> Result<u32> {
        self.calls
            .lock()
            .unwrap()
            .push("verify_existing_live_owner");
        match self.existing_owner {
            ExistingOwnerMode::Verified(pid) => Ok(pid),
            ExistingOwnerMode::Denied(pid) => {
                Err(crate::ProcessExecutableInspectionDenied { pid }.into())
            }
            ExistingOwnerMode::Absent => Err(anyhow!("no live daemon owner")),
        }
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
    let backend = ExistingOwnerBackend::new(
        false,
        LiveOwnerMode::Absent,
        ExistingOwnerMode::Verified(37_687),
    );
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
            registration_verified,
            owner: PreservedLiveOwner::Verified { pid },
            reason,
        } => {
            assert_eq!(artifact, Some(PathBuf::from("/tmp/rs.ctx.daemon.plist")));
            assert!(!registration_verified);
            assert_eq!(pid, 37_687);
            assert!(reason
                .expect("registration error")
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
    let mut backend =
        ExistingOwnerBackend::new(false, LiveOwnerMode::Absent, ExistingOwnerMode::Absent);
    backend.install_owner = Some(16_149);
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

#[test]
fn preserve_live_owner_preserves_detached_owner_when_registration_is_valid() {
    let temp = tempfile::tempdir().unwrap();
    let _lock = crate::DaemonLock::acquire(temp.path())
        .unwrap()
        .expect("active daemon lock");
    let backend = ExistingOwnerBackend::new(
        true,
        LiveOwnerMode::Absent,
        ExistingOwnerMode::Verified(41_001),
    );
    let outcome = ensure_native_supervisor_with_policy(
        temp.path(),
        Path::new("/tmp/ctx"),
        &(),
        &backend,
        SupervisorMutationPolicy::PreserveLiveOwner,
    )
    .unwrap();
    match outcome {
        SupervisorEnsureOutcome::LiveOwnerPreserved {
            artifact,
            registration_verified,
            owner: PreservedLiveOwner::Verified { pid },
            reason,
        } => {
            assert_eq!(artifact, Some(PathBuf::from("/tmp/rs.ctx.daemon.plist")));
            assert!(registration_verified);
            assert_eq!(pid, 41_001);
            assert!(reason
                .expect("initial live error")
                .to_string()
                .contains("manager has no live owner"));
        }
        other => panic!("expected LiveOwnerPreserved, got {other:?}"),
    }
    assert_eq!(
        backend.calls.lock().unwrap().as_slice(),
        [
            "probe_manager",
            "artifact_path",
            "verify_registration",
            "verify_live_owner",
            "verify_existing_live_owner",
        ]
    );
}

#[test]
fn preserve_live_owner_starts_valid_registration_when_no_live_owner_exists() {
    let mut backend =
        ExistingOwnerBackend::new(true, LiveOwnerMode::Absent, ExistingOwnerMode::Absent);
    backend.start_owner = Some(41_002);
    let outcome = ensure_native_supervisor_with_policy(
        Path::new("/tmp/data"),
        Path::new("/tmp/ctx"),
        &(),
        &backend,
        SupervisorMutationPolicy::PreserveLiveOwner,
    )
    .unwrap();
    assert!(matches!(
        outcome,
        SupervisorEnsureOutcome::Native {
            owner_pid: 41_002,
            environment_installed: false,
            ..
        }
    ));
    let calls = backend.calls.lock().unwrap();
    assert!(calls.contains(&"verify_existing_live_owner"));
    assert!(calls.contains(&"prepare_start"));
    assert!(calls.contains(&"start"));
}

#[test]
fn preserve_live_owner_never_reinstalls_or_mutates_when_inspection_is_denied() {
    let backend = ExistingOwnerBackend::new(
        false,
        LiveOwnerMode::Absent,
        ExistingOwnerMode::Denied(51_001),
    );
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
            registration_verified,
            owner: PreservedLiveOwner::InspectionDenied { error },
            reason,
            ..
        } => {
            assert!(!registration_verified);
            assert!(error.is::<crate::ProcessExecutableInspectionDenied>());
            assert!(reason
                .expect("registration error")
                .to_string()
                .contains("supervisor environment does not match"));
        }
        other => panic!("expected LiveOwnerPreserved(InspectionDenied), got {other:?}"),
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
fn preserve_live_owner_never_migrates_or_restarts_when_registered_owner_inspection_is_denied() {
    for (live_owner, existing_owner, expected_calls) in [
        (
            LiveOwnerMode::Denied(51_002),
            ExistingOwnerMode::Absent,
            vec![
                "probe_manager",
                "artifact_path",
                "verify_registration",
                "verify_live_owner",
            ],
        ),
        (
            LiveOwnerMode::Absent,
            ExistingOwnerMode::Denied(51_003),
            vec![
                "probe_manager",
                "artifact_path",
                "verify_registration",
                "verify_live_owner",
                "verify_existing_live_owner",
            ],
        ),
    ] {
        let backend = ExistingOwnerBackend::new(true, live_owner, existing_owner);
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
                registration_verified,
                owner: PreservedLiveOwner::InspectionDenied { error },
                ..
            } => {
                assert!(registration_verified);
                assert!(error.is::<crate::ProcessExecutableInspectionDenied>());
            }
            other => panic!("expected LiveOwnerPreserved(InspectionDenied), got {other:?}"),
        }
        assert_eq!(backend.calls.lock().unwrap().as_slice(), expected_calls);
    }
}
