use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Error, Result};

mod artifact;
mod lock;
mod platform;
#[cfg(test)]
mod preserve_live_owner_tests;
mod state;
mod windows;

pub use artifact::*;
pub use lock::SupervisorInstallationLock;
pub use platform::*;
pub use state::{write_atomic_supervisor_file, write_atomic_supervisor_file_if_changed};
pub use windows::*;

const SUPERVISOR_HANDOFF_TIMEOUT: Duration = Duration::from_secs(5);
const SUPERVISOR_POLL_INTERVAL: Duration = Duration::from_millis(25);
const SUPERVISOR_STARTUP_GRACE: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum SupervisorManagerOperability {
    Operational,
    Unavailable { reason: String },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SupervisorMutationPolicy {
    AllowReinstall,
    PreserveLiveOwner,
}

pub trait NativeSupervisorBackend<E>: Sync {
    fn probe_manager(&self, data_root: &Path) -> Result<SupervisorManagerOperability>;
    /// Perform any owner handoff required before native registration state may
    /// be changed. The runtime calls this only after an operational manager
    /// probe, so an unavailable manager leaves all existing state untouched.
    fn prepare_mutation(&self, data_root: &Path, executable: &Path) -> Result<()>;
    fn artifact_path(&self, data_root: &Path) -> Result<Option<PathBuf>>;
    fn install(&self, data_root: &Path, executable: &Path, environment: &E) -> Result<PathBuf>;
    fn disable(&self, data_root: &Path) -> Result<Option<PathBuf>>;
    fn verify_registration(&self, data_root: &Path, executable: &Path) -> Result<()>;
    fn verify_live_owner(&self, data_root: &Path, executable: &Path) -> Result<u32>;
    /// Verify whether a live, identity-verified, same-executable daemon already
    /// owns the singleton lock, independent of native registration matching.
    fn verify_existing_live_owner(&self, data_root: &Path, executable: &Path) -> Result<u32> {
        crate::verify_daemon_owner_identity(data_root, executable, None)
    }
    /// Reverify manager ownership immediately before handing off a detached
    /// owner. Returning an owner PID suppresses the start; returning `None`
    /// means any detached owner was released and the manager may start safely.
    fn prepare_start(&self, data_root: &Path, executable: &Path) -> Result<Option<u32>>;
    fn start(&self, data_root: &Path) -> Result<()>;
}

#[derive(Debug)]
pub enum PreservedLiveOwner {
    Verified { pid: u32 },
    InspectionDenied { error: Error },
}

#[derive(Debug)]
pub enum SupervisorEnsureOutcome {
    Native {
        artifact: Option<PathBuf>,
        owner_pid: u32,
        environment_installed: bool,
        recovery_reason: Option<String>,
    },
    LiveOwnerPreserved {
        artifact: Option<PathBuf>,
        registration_verified: bool,
        owner: PreservedLiveOwner,
        reason: Option<Error>,
    },
    RegisteredNotRunning {
        artifact: Option<PathBuf>,
        initial_error: Error,
        recovery_error: Error,
        environment_installed: bool,
        recovery_reason: Option<String>,
    },
    InstallFailed {
        artifact: Option<PathBuf>,
        error: Error,
        cleanup_error: Option<Error>,
        recovery_reason: Option<String>,
    },
    ManagerUnavailable {
        artifact: Option<PathBuf>,
        reason: String,
        native_state_preserved: bool,
        preceding_error: Option<String>,
    },
}

pub fn ensure_native_supervisor_with<E>(
    data_root: &Path,
    executable: &Path,
    environment: &E,
    backend: &dyn NativeSupervisorBackend<E>,
) -> Result<SupervisorEnsureOutcome> {
    ensure_native_supervisor_with_policy(
        data_root,
        executable,
        environment,
        backend,
        SupervisorMutationPolicy::AllowReinstall,
    )
}

pub fn ensure_native_supervisor_with_policy<E>(
    data_root: &Path,
    executable: &Path,
    environment: &E,
    backend: &dyn NativeSupervisorBackend<E>,
    policy: SupervisorMutationPolicy,
) -> Result<SupervisorEnsureOutcome> {
    if let Some(outcome) = manager_unavailable_after_probe(data_root, backend, false, None)? {
        return Ok(outcome);
    }
    let artifact = backend.artifact_path(data_root)?;
    let registration_error = match backend.verify_registration(data_root, executable) {
        Ok(()) => match backend.verify_live_owner(data_root, executable) {
            Ok(owner_pid) => {
                return Ok(SupervisorEnsureOutcome::Native {
                    artifact,
                    owner_pid,
                    environment_installed: false,
                    recovery_reason: None,
                });
            }
            Err(initial_live_error) => {
                if policy == SupervisorMutationPolicy::PreserveLiveOwner {
                    if initial_live_error.is::<crate::ProcessExecutableInspectionDenied>() {
                        return Ok(SupervisorEnsureOutcome::LiveOwnerPreserved {
                            artifact,
                            registration_verified: true,
                            owner: PreservedLiveOwner::InspectionDenied {
                                error: initial_live_error,
                            },
                            reason: None,
                        });
                    }
                    if let Some(owner) = preserved_live_owner(data_root, executable, backend) {
                        return Ok(SupervisorEnsureOutcome::LiveOwnerPreserved {
                            artifact,
                            registration_verified: true,
                            owner,
                            reason: Some(initial_live_error),
                        });
                    }
                }
                if let Some(outcome) = manager_unavailable_after_probe(
                    data_root,
                    backend,
                    true,
                    Some(format!(
                        "initial live-owner verification: {initial_live_error:#}"
                    )),
                )? {
                    return Ok(outcome);
                }
                let start_reason =
                    format!("started_unrunning_registration: {initial_live_error:#}");
                return match start_and_wait_for_native_live_owner(data_root, executable, backend) {
                    Ok(owner_pid) => Ok(SupervisorEnsureOutcome::Native {
                        artifact,
                        owner_pid,
                        environment_installed: false,
                        recovery_reason: Some(start_reason),
                    }),
                    Err(recovery_error) => {
                        let preceding_error = format!(
                                "initial live-owner verification: {initial_live_error:#}; recovery: {recovery_error:#}"
                            );
                        if let Some(outcome) = manager_unavailable_after_probe(
                            data_root,
                            backend,
                            true,
                            Some(preceding_error),
                        )? {
                            Ok(outcome)
                        } else {
                            Ok(SupervisorEnsureOutcome::RegisteredNotRunning {
                                artifact,
                                initial_error: initial_live_error,
                                recovery_error,
                                environment_installed: false,
                                recovery_reason: Some(start_reason),
                            })
                        }
                    }
                };
            }
        },
        Err(error) => error,
    };

    if policy == SupervisorMutationPolicy::PreserveLiveOwner {
        if let Some(owner) = preserved_live_owner(data_root, executable, backend) {
            return Ok(SupervisorEnsureOutcome::LiveOwnerPreserved {
                artifact,
                registration_verified: false,
                owner,
                reason: Some(registration_error),
            });
        }
    }

    // Registration verification can execute an external manager command. Probe
    // again immediately before the first native mutation so a manager that
    // disappeared during that read-only check never leaves a new partial
    // registration behind.
    if let Some(outcome) = manager_unavailable_after_probe(data_root, backend, false, None)? {
        return Ok(outcome);
    }
    // A valid registration is handled above without mutating or handing off
    // its live owner. Prepare the daemon handoff only when registration really
    // must be replaced.
    backend.prepare_mutation(data_root, executable)?;

    let reinstall_reason = format!("reinstalled_registration: {registration_error:#}");
    let installation = backend
        .install(data_root, executable, environment)
        .and_then(|installed_artifact| {
            wait_for_native_live_owner(data_root, executable, backend)
                .map(|owner_pid| (installed_artifact, owner_pid))
        });
    match installation {
        Ok((installed_artifact, owner_pid)) => Ok(SupervisorEnsureOutcome::Native {
            artifact: Some(installed_artifact),
            owner_pid,
            environment_installed: true,
            recovery_reason: Some(reinstall_reason),
        }),
        Err(error) => {
            let installation_error = format!("native supervisor installation: {error:#}");
            if let Some(outcome) = manager_unavailable_after_probe(
                data_root,
                backend,
                true,
                Some(installation_error.clone()),
            )? {
                return Ok(outcome);
            }
            let registration = backend.verify_registration(data_root, executable);
            if registration.is_ok() {
                let recovery = match backend.verify_live_owner(data_root, executable) {
                    Ok(owner_pid) => Ok(owner_pid),
                    Err(live_error) => {
                        if let Some(outcome) = manager_unavailable_after_probe(
                            data_root,
                            backend,
                            true,
                            Some(format!(
                                "{installation_error}; live-owner verification: {live_error:#}"
                            )),
                        )? {
                            return Ok(outcome);
                        }
                        start_and_wait_for_native_live_owner(data_root, executable, backend)
                    }
                };
                match recovery {
                    Ok(owner_pid) => Ok(SupervisorEnsureOutcome::Native {
                        artifact,
                        owner_pid,
                        environment_installed: true,
                        recovery_reason: Some(reinstall_reason),
                    }),
                    Err(recovery_error) => {
                        let preceding_error = format!(
                            "{installation_error}; registration recovery: {recovery_error:#}"
                        );
                        if let Some(outcome) = manager_unavailable_after_probe(
                            data_root,
                            backend,
                            true,
                            Some(preceding_error),
                        )? {
                            Ok(outcome)
                        } else {
                            Ok(SupervisorEnsureOutcome::RegisteredNotRunning {
                                artifact,
                                initial_error: error,
                                recovery_error,
                                environment_installed: true,
                                recovery_reason: Some(reinstall_reason),
                            })
                        }
                    }
                }
            } else {
                let registration_error = registration
                    .expect_err("failed native registration verification must carry an error");
                if let Some(outcome) = manager_unavailable_after_probe(
                    data_root,
                    backend,
                    true,
                    Some(format!(
                        "{installation_error}; registration verification: {registration_error:#}"
                    )),
                )? {
                    return Ok(outcome);
                }
                let cleanup_error = backend.disable(data_root).err();
                if let Some(cleanup_error) = cleanup_error.as_ref() {
                    let preceding_error = format!(
                        "{installation_error}; native supervisor cleanup: {cleanup_error:#}"
                    );
                    if let Some(outcome) = manager_unavailable_after_probe(
                        data_root,
                        backend,
                        true,
                        Some(preceding_error),
                    )? {
                        return Ok(outcome);
                    }
                }
                Ok(SupervisorEnsureOutcome::InstallFailed {
                    artifact: if cleanup_error.is_some() {
                        backend.artifact_path(data_root)?
                    } else {
                        None
                    },
                    error,
                    cleanup_error,
                    recovery_reason: Some(format!("reinstall_failed: {reinstall_reason}")),
                })
            }
        }
    }
}

fn preserved_live_owner<E>(
    data_root: &Path,
    executable: &Path,
    backend: &dyn NativeSupervisorBackend<E>,
) -> Option<PreservedLiveOwner> {
    match backend.verify_existing_live_owner(data_root, executable) {
        Ok(pid) => Some(PreservedLiveOwner::Verified { pid }),
        Err(error) if error.is::<crate::ProcessExecutableInspectionDenied>() => {
            Some(PreservedLiveOwner::InspectionDenied { error })
        }
        Err(_) => None,
    }
}

fn manager_unavailable_after_probe<E>(
    data_root: &Path,
    backend: &dyn NativeSupervisorBackend<E>,
    native_state_may_remain: bool,
    preceding_error: Option<String>,
) -> Result<Option<SupervisorEnsureOutcome>> {
    let SupervisorManagerOperability::Unavailable { reason } = backend.probe_manager(data_root)?
    else {
        return Ok(None);
    };
    let artifact = backend.artifact_path(data_root)?;
    let artifact_present = artifact
        .as_deref()
        .map(supervisor_artifact_present)
        .transpose()?
        .unwrap_or(false);
    Ok(Some(SupervisorEnsureOutcome::ManagerUnavailable {
        artifact: (artifact_present || native_state_may_remain)
            .then_some(artifact)
            .flatten(),
        reason,
        native_state_preserved: native_state_may_remain || artifact_present,
        preceding_error,
    }))
}

fn supervisor_artifact_present(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error)
            .with_context(|| format!("inspect native supervisor artifact {}", path.display())),
    }
}

fn wait_for_native_live_owner<E>(
    data_root: &Path,
    executable: &Path,
    backend: &dyn NativeSupervisorBackend<E>,
) -> Result<u32> {
    let deadline = Instant::now() + SUPERVISOR_HANDOFF_TIMEOUT;
    loop {
        match backend.verify_live_owner(data_root, executable) {
            Ok(owner_pid) => return Ok(owner_pid),
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => {}
        }
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "native supervisor did not start daemon lifecycle ownership"
            ));
        }
        std::thread::sleep(SUPERVISOR_POLL_INTERVAL);
    }
}

fn wait_for_native_live_owner_during_startup_grace<E>(
    data_root: &Path,
    executable: &Path,
    backend: &dyn NativeSupervisorBackend<E>,
) -> Option<u32> {
    if !crate::daemon_lock_is_active(data_root) {
        return None;
    }
    let deadline = Instant::now() + SUPERVISOR_STARTUP_GRACE;
    loop {
        if let Ok(owner_pid) = backend.verify_live_owner(data_root, executable) {
            return Some(owner_pid);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(SUPERVISOR_POLL_INTERVAL);
    }
}

fn start_and_wait_for_native_live_owner<E>(
    data_root: &Path,
    executable: &Path,
    backend: &dyn NativeSupervisorBackend<E>,
) -> Result<u32> {
    // A manager child can publish its process lock before the manager-specific
    // ownership witness (notably the Windows provenance sidecar) is visible.
    // Give that publication a bounded grace period, then reverify once more in
    // `prepare_start` before asking a detached owner to hand off.
    if let Some(owner_pid) =
        wait_for_native_live_owner_during_startup_grace(data_root, executable, backend)
    {
        return Ok(owner_pid);
    }
    if let Some(owner_pid) = backend.prepare_start(data_root, executable)? {
        return Ok(owner_pid);
    }
    backend.start(data_root)?;
    wait_for_native_live_owner(data_root, executable, backend)
}

pub trait SupervisorUpgradeFence {
    fn release(&mut self) -> Result<()>;
}

#[derive(Debug)]
pub enum SupervisorResumeOutcome {
    Native {
        artifact: Option<PathBuf>,
        owner_pid: u32,
    },
    Fallback,
    RegisteredNotRunning {
        artifact: Option<PathBuf>,
        error: Error,
    },
    ManagerUnavailable {
        artifact: Option<PathBuf>,
        reason: String,
        native_state_preserved: bool,
        preceding_error: Option<String>,
    },
}

pub fn resume_native_supervisor_with<E>(
    data_root: &Path,
    executable: &Path,
    backend: &dyn NativeSupervisorBackend<E>,
    upgrade_fence: &mut dyn SupervisorUpgradeFence,
) -> Result<SupervisorResumeOutcome> {
    if let Some(outcome) = manager_unavailable_resume_after_probe(data_root, backend, false, None)?
    {
        return Ok(outcome);
    }
    if backend.verify_registration(data_root, executable).is_err() {
        if let Some(outcome) = manager_unavailable_resume_after_probe(
            data_root,
            backend,
            true,
            Some("native registration could not be verified".to_owned()),
        )? {
            return Ok(outcome);
        }
        return Ok(SupervisorResumeOutcome::Fallback);
    }
    if let Some(outcome) = manager_unavailable_resume_after_probe(
        data_root,
        backend,
        true,
        Some("native registration was verified before upgrade-fence release".to_owned()),
    )? {
        return Ok(outcome);
    }
    upgrade_fence.release()?;
    let owner = backend
        .verify_live_owner(data_root, executable)
        .or_else(|_| start_and_wait_for_native_live_owner(data_root, executable, backend));
    match owner {
        Ok(owner_pid) => Ok(SupervisorResumeOutcome::Native {
            artifact: backend.artifact_path(data_root)?,
            owner_pid,
        }),
        Err(error) => {
            let preceding_error = format!("native supervisor resume: {error:#}");
            if let Some(outcome) = manager_unavailable_resume_after_probe(
                data_root,
                backend,
                true,
                Some(preceding_error),
            )? {
                Ok(outcome)
            } else {
                Ok(SupervisorResumeOutcome::RegisteredNotRunning {
                    artifact: backend.artifact_path(data_root)?,
                    error,
                })
            }
        }
    }
}

fn manager_unavailable_resume_after_probe<E>(
    data_root: &Path,
    backend: &dyn NativeSupervisorBackend<E>,
    native_state_may_remain: bool,
    preceding_error: Option<String>,
) -> Result<Option<SupervisorResumeOutcome>> {
    let Some(SupervisorEnsureOutcome::ManagerUnavailable {
        artifact,
        reason,
        native_state_preserved,
        preceding_error,
    }) = manager_unavailable_after_probe(
        data_root,
        backend,
        native_state_may_remain,
        preceding_error,
    )?
    else {
        return Ok(None);
    };
    Ok(Some(SupervisorResumeOutcome::ManagerUnavailable {
        artifact,
        reason,
        native_state_preserved,
        preceding_error,
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    struct StubBackend {
        registered: bool,
        owner_pid: Option<u32>,
        delayed_owner: Option<(usize, u32)>,
        prepared_owner: Option<u32>,
        install_error: bool,
        calls: Mutex<Vec<&'static str>>,
    }

    impl StubBackend {
        fn new(registered: bool, owner_pid: Option<u32>, install_error: bool) -> Self {
            Self {
                registered,
                owner_pid,
                delayed_owner: None,
                prepared_owner: None,
                install_error,
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }

        fn record(&self, call: &'static str) {
            self.calls.lock().unwrap().push(call);
        }

        fn with_delayed_owner(mut self, verification: usize, owner_pid: u32) -> Self {
            self.delayed_owner = Some((verification, owner_pid));
            self
        }

        fn with_prepared_owner(mut self, owner_pid: u32) -> Self {
            self.prepared_owner = Some(owner_pid);
            self
        }
    }

    impl NativeSupervisorBackend<()> for StubBackend {
        fn probe_manager(&self, _data_root: &Path) -> Result<SupervisorManagerOperability> {
            self.record("probe_manager");
            Ok(SupervisorManagerOperability::Operational)
        }

        fn prepare_mutation(&self, _data_root: &Path, _executable: &Path) -> Result<()> {
            self.record("prepare_mutation");
            Ok(())
        }

        fn artifact_path(&self, _data_root: &Path) -> Result<Option<PathBuf>> {
            self.record("artifact_path");
            Ok(Some(PathBuf::from("/tmp/ctx.service")))
        }

        fn install(
            &self,
            _data_root: &Path,
            _executable: &Path,
            _environment: &(),
        ) -> Result<PathBuf> {
            self.record("install");
            if self.install_error {
                Err(anyhow!("install failed"))
            } else {
                Ok(PathBuf::from("/tmp/installed.service"))
            }
        }

        fn disable(&self, _data_root: &Path) -> Result<Option<PathBuf>> {
            self.record("disable");
            Ok(Some(PathBuf::from("/tmp/ctx.service")))
        }

        fn verify_registration(&self, _data_root: &Path, _executable: &Path) -> Result<()> {
            self.record("verify_registration");
            if self.registered {
                Ok(())
            } else {
                Err(anyhow!("not registered"))
            }
        }

        fn verify_live_owner(&self, _data_root: &Path, _executable: &Path) -> Result<u32> {
            self.record("verify_live_owner");
            if let Some((verification, owner_pid)) = self.delayed_owner {
                let observed = self
                    .calls
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|call| **call == "verify_live_owner")
                    .count();
                if observed >= verification {
                    return Ok(owner_pid);
                }
            }
            self.owner_pid.ok_or_else(|| anyhow!("not running"))
        }

        fn prepare_start(&self, _data_root: &Path, _executable: &Path) -> Result<Option<u32>> {
            self.record("prepare_start");
            Ok(self.prepared_owner)
        }

        fn start(&self, _data_root: &Path) -> Result<()> {
            self.record("start");
            Ok(())
        }
    }

    #[derive(Default)]
    struct StubFence {
        released: bool,
    }

    impl SupervisorUpgradeFence for StubFence {
        fn release(&mut self) -> Result<()> {
            self.released = true;
            Ok(())
        }
    }

    #[test]
    fn repeated_ensure_reuses_registered_live_owner_without_mutation() {
        let backend = StubBackend::new(true, Some(41), false);
        for _ in 0..2 {
            let outcome = ensure_native_supervisor_with(
                Path::new("/tmp/data"),
                Path::new("/tmp/ctx"),
                &(),
                &backend,
            )
            .unwrap();
            assert!(matches!(
                outcome,
                SupervisorEnsureOutcome::Native {
                    owner_pid: 41,
                    environment_installed: false,
                    ..
                }
            ));
        }
        assert_eq!(
            backend.calls(),
            [
                "probe_manager",
                "artifact_path",
                "verify_registration",
                "verify_live_owner",
                "probe_manager",
                "artifact_path",
                "verify_registration",
                "verify_live_owner",
            ]
        );
    }

    #[test]
    fn ensure_waits_for_manager_provenance_before_handing_off_a_live_lock() {
        let temp = tempfile::tempdir().unwrap();
        let _lock = crate::DaemonLock::acquire(temp.path())
            .unwrap()
            .expect("test daemon lock");
        let backend = StubBackend::new(true, None, false).with_delayed_owner(3, 73);

        let outcome =
            ensure_native_supervisor_with(temp.path(), Path::new("/tmp/ctx"), &(), &backend)
                .unwrap();

        assert!(matches!(
            outcome,
            SupervisorEnsureOutcome::Native { owner_pid: 73, .. }
        ));
        assert!(!backend.calls().contains(&"prepare_start"));
        assert!(!backend.calls().contains(&"start"));
    }

    #[test]
    fn ensure_reverifies_immediately_before_handoff_and_suppresses_restart() {
        let temp = tempfile::tempdir().unwrap();
        let _lock = crate::DaemonLock::acquire(temp.path())
            .unwrap()
            .expect("test daemon lock");
        let backend = StubBackend::new(true, None, false).with_prepared_owner(89);

        let outcome =
            ensure_native_supervisor_with(temp.path(), Path::new("/tmp/ctx"), &(), &backend)
                .unwrap();

        assert!(matches!(
            outcome,
            SupervisorEnsureOutcome::Native { owner_pid: 89, .. }
        ));
        assert!(backend.calls().contains(&"prepare_start"));
        assert!(!backend.calls().contains(&"start"));
    }

    #[test]
    fn ensure_cleans_up_when_installation_does_not_register() {
        let backend = StubBackend::new(false, None, true);
        let outcome = ensure_native_supervisor_with(
            Path::new("/tmp/data"),
            Path::new("/tmp/ctx"),
            &(),
            &backend,
        )
        .unwrap();
        assert!(matches!(
            outcome,
            SupervisorEnsureOutcome::InstallFailed {
                artifact: None,
                cleanup_error: None,
                ..
            }
        ));
        assert_eq!(
            backend.calls(),
            [
                "probe_manager",
                "artifact_path",
                "verify_registration",
                "probe_manager",
                "prepare_mutation",
                "install",
                "probe_manager",
                "verify_registration",
                "probe_manager",
                "disable"
            ]
        );
    }

    #[test]
    fn resume_preserves_upgrade_fence_when_registration_is_absent() {
        let backend = StubBackend::new(false, None, false);
        let mut fence = StubFence::default();
        let outcome = resume_native_supervisor_with(
            Path::new("/tmp/data"),
            Path::new("/tmp/ctx"),
            &backend,
            &mut fence,
        )
        .unwrap();
        assert!(matches!(outcome, SupervisorResumeOutcome::Fallback));
        assert!(!fence.released);
        assert_eq!(
            backend.calls(),
            ["probe_manager", "verify_registration", "probe_manager"]
        );
    }

    #[test]
    fn ensure_hands_same_binary_fallback_to_preserved_registration_once() {
        #[derive(Default)]
        struct State {
            fallback_owns_lock: bool,
            manager_owner: Option<u32>,
            calls: Vec<&'static str>,
        }

        struct PreservedRegistrationBackend {
            state: Mutex<State>,
        }

        impl NativeSupervisorBackend<()> for PreservedRegistrationBackend {
            fn probe_manager(&self, _data_root: &Path) -> Result<SupervisorManagerOperability> {
                self.state.lock().unwrap().calls.push("probe_manager");
                Ok(SupervisorManagerOperability::Operational)
            }

            fn prepare_mutation(&self, _data_root: &Path, _executable: &Path) -> Result<()> {
                self.state.lock().unwrap().calls.push("prepare_mutation");
                Ok(())
            }

            fn artifact_path(&self, _data_root: &Path) -> Result<Option<PathBuf>> {
                self.state.lock().unwrap().calls.push("artifact_path");
                Ok(Some(PathBuf::from("/tmp/ctx.service")))
            }

            fn install(
                &self,
                _data_root: &Path,
                _executable: &Path,
                _environment: &(),
            ) -> Result<PathBuf> {
                self.state.lock().unwrap().calls.push("install");
                Err(anyhow!("preserved registration must not be reinstalled"))
            }

            fn disable(&self, _data_root: &Path) -> Result<Option<PathBuf>> {
                self.state.lock().unwrap().calls.push("disable");
                Err(anyhow!("preserved registration must not be disabled"))
            }

            fn verify_registration(&self, _data_root: &Path, _executable: &Path) -> Result<()> {
                self.state.lock().unwrap().calls.push("verify_registration");
                Ok(())
            }

            fn verify_live_owner(&self, _data_root: &Path, _executable: &Path) -> Result<u32> {
                let mut state = self.state.lock().unwrap();
                state.calls.push("verify_live_owner");
                state
                    .manager_owner
                    .ok_or_else(|| anyhow!("same-binary detached fallback is not manager-owned"))
            }

            fn prepare_start(&self, _data_root: &Path, _executable: &Path) -> Result<Option<u32>> {
                let mut state = self.state.lock().unwrap();
                state.calls.push("supervisor_handoff");
                if !state.fallback_owns_lock {
                    return Err(anyhow!("no detached fallback owns the singleton"));
                }
                state.fallback_owns_lock = false;
                Ok(None)
            }

            fn start(&self, _data_root: &Path) -> Result<()> {
                let mut state = self.state.lock().unwrap();
                state.calls.push("start");
                if state.fallback_owns_lock {
                    return Err(anyhow!("manager start raced the detached fallback"));
                }
                state.manager_owner = Some(73);
                Ok(())
            }
        }

        let backend = PreservedRegistrationBackend {
            state: Mutex::new(State {
                fallback_owns_lock: true,
                ..State::default()
            }),
        };
        let first = ensure_native_supervisor_with(
            Path::new("/tmp/data"),
            Path::new("/tmp/ctx"),
            &(),
            &backend,
        )
        .unwrap();
        assert!(matches!(
            first,
            SupervisorEnsureOutcome::Native {
                owner_pid: 73,
                environment_installed: false,
                ..
            }
        ));
        let second = ensure_native_supervisor_with(
            Path::new("/tmp/data"),
            Path::new("/tmp/ctx"),
            &(),
            &backend,
        )
        .unwrap();
        assert!(matches!(
            second,
            SupervisorEnsureOutcome::Native {
                owner_pid: 73,
                environment_installed: false,
                ..
            }
        ));
        assert_eq!(
            backend.state.lock().unwrap().calls,
            [
                "probe_manager",
                "artifact_path",
                "verify_registration",
                "verify_live_owner",
                "probe_manager",
                "supervisor_handoff",
                "start",
                "verify_live_owner",
                "probe_manager",
                "artifact_path",
                "verify_registration",
                "verify_live_owner",
            ]
        );
    }
}
