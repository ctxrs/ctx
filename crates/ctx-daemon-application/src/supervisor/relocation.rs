//! Preserve the installed launch environment while changing only its root.
use super::*;

pub struct ManagedRootSupervisor {
    environment: SupervisorEnvironmentSnapshot,
}

impl ManagedRootSupervisor {
    /// Keep launch-environment writers excluded until relocation either
    /// activates the copy or releases its quiescence guards on failure.
    pub fn capture_locked(
        host: &dyn DaemonApplicationHost,
        source: &Path,
    ) -> Result<(Self, SupervisorInstallationLock)> {
        let lock = SupervisorInstallationLock::acquire(source)?;
        let snapshot = Self::capture(host, source)?;
        Ok((snapshot, lock))
    }

    pub fn capture(host: &dyn DaemonApplicationHost, source: &Path) -> Result<Self> {
        let receipt = stored_supervisor_report(source);
        let environment = if ctx_daemon_runtime::supervisor_environment_path(source).exists() {
            environment::installed_supervisor_environment_snapshot(source, &receipt)?
        } else {
            configured_supervisor_environment(host, source, None)?
        };
        Ok(Self { environment })
    }

    /// Only non-secret provider discovery inputs cross into the discovery layer.
    pub fn provider_environment(&self) -> impl Iterator<Item = (&'static str, &str)> {
        self.environment.values.iter().filter_map(|(name, value)| {
            environment::DISCOVERY_ENV_ALLOWLIST
                .iter()
                .find(|allowed| **allowed == name)
                .map(|name| (*name, value.as_str()))
        })
    }

    /// A disabled receipt carries the validated environment without claiming
    /// that old service artifacts or PID ownership survived the copy.
    pub fn persist(&self, destination: &Path) -> Result<()> {
        let executable = env::current_exe()?;
        let identity = SupervisorIdentity::new("ctx", destination.join("daemon/relocation"))?;
        let spec = environment::supervisor_artifact_spec(
            identity,
            &executable,
            destination,
            &self.environment,
        )?;
        ctx_daemon_runtime::write_supervisor_environment(&spec)?;
        write_supervisor_receipt_with_environment_snapshot(
            destination,
            &SupervisorReceipt {
                kind: "cli_self_heal".to_owned(),
                status: "disabled",
                autostart_supported: false,
                restart_supported: false,
                registration_verified: false,
                live_owner_verified: false,
                owner_pid: None,
                artifact_path: None,
                executable_path: Some(executable),
                limitation: None,
                last_error: None,
            },
            Some(self.environment.contract_report()),
        )
    }

    pub fn resume(&self, host: &dyn DaemonApplicationHost, destination: &Path) -> Result<()> {
        ensure_hosted_uninstall_supervisor_admission(host)?;
        if let Some(executable) = safely_supported_managed_install(host, destination)? {
            let input = ManagedSupervisorInput {
                data_root: destination.to_path_buf(),
                executable,
                daemon_environment: self.environment.clone(),
                manager_environment: supervisor_manager_environment(host)?,
            };
            let backend = PlatformNativeSupervisor::new(
                host,
                destination,
                Some(&input.daemon_environment),
                &input.manager_environment,
            )?;
            ensure_native_supervisor_with(host, &input, &backend)?;
        }
        let config = host.daemon_config(destination)?;
        lifecycle::start_daemon_and_wait(host, destination, &config, crate::DaemonTrigger::Setup)
            .map_err(|error| anyhow!("resume relocated daemon: {error:?}"))?;
        Ok(())
    }
}

pub(super) fn disable_for_move_with(
    root: &Path,
    executable: Option<PathBuf>,
    manual: bool,
    receipt: &Value,
    backend: &dyn NativeSupervisorBackend<SupervisorEnvironmentSnapshot>,
) -> Result<()> {
    if manual && registration_is_absent(root, receipt, backend)? {
        return Ok(());
    }
    disable_native_supervisor_candidate_with(root, executable, backend)
}

/// A manual installation with no registration evidence needs no manager. A
/// surviving artifact, failed/unknown receipt, or active receipt still requires
/// successful native removal, even if its launch environment has disappeared.
pub(super) fn registration_is_absent(
    root: &Path,
    receipt: &Value,
    backend: &dyn NativeSupervisorBackend<SupervisorEnvironmentSnapshot>,
) -> Result<bool> {
    let receipt_path = ctx_daemon_runtime::daemon_root_path(root).join("supervisor.json");
    let receipt_absent = match std::fs::symlink_metadata(receipt_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => return Err(error.into()),
        Ok(_) => false,
    };
    let inactive =
        receipt_absent || receipt["status"] == "disabled" || receipt["kind"] == "cli_self_heal";
    if !inactive {
        return Ok(false);
    }
    let Some(artifact) = backend.artifact_path(root)? else {
        return Ok(true);
    };
    match std::fs::symlink_metadata(artifact) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error.into()),
        Ok(_) => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_root_capture_waits_for_environment_writer_and_retains_exclusion() -> Result<()> {
        use std::{sync::mpsc, thread, time::Duration};

        let root = tempfile::tempdir()?;
        let original = ManagedRootSupervisor::capture(&TestHost, root.path())?;
        original.persist(root.path())?;
        let writer = SupervisorInstallationLock::acquire(root.path())?;
        let (started_tx, started_rx) = mpsc::channel();
        let (captured_tx, captured_rx) = mpsc::channel();
        let source = root.path().to_path_buf();
        let mover = thread::spawn(move || {
            started_tx.send(()).unwrap();
            let _ = captured_tx.send(ManagedRootSupervisor::capture_locked(&TestHost, &source));
        });
        started_rx.recv_timeout(Duration::from_secs(5))?;
        assert!(matches!(
            captured_rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        let mut updated = original.environment.clone();
        updated.values = vec![(
            "HTTPS_PROXY".to_owned(),
            "http://127.0.0.1:12345".to_owned(),
        )];
        let updated = updated.with_loop_interval_seconds(Some(60))?;
        ManagedRootSupervisor {
            environment: updated.clone(),
        }
        .persist(root.path())?;
        drop(writer);
        let (captured, relocation_lock) = captured_rx.recv_timeout(Duration::from_secs(5))??;
        mover.join().unwrap();
        assert_eq!(captured.environment, updated);

        let (written_tx, written_rx) = mpsc::channel();
        let source = root.path().to_path_buf();
        let next_writer = thread::spawn(move || {
            let _lock = SupervisorInstallationLock::acquire(&source).unwrap();
            let _ = written_tx.send(());
        });
        assert!(matches!(
            written_rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        drop(relocation_lock);
        written_rx.recv_timeout(Duration::from_secs(5))?;
        next_writer.join().unwrap();
        Ok(())
    }

    #[test]
    fn managed_root_snapshot_yields_to_explicit_unmanaged_environment_refresh() -> Result<()> {
        let root = tempfile::tempdir()?;
        let snapshot = ManagedRootSupervisor::capture(&TestHost, root.path())?;
        snapshot.persist(root.path())?;
        assert!(ctx_daemon_runtime::supervisor_environment_path(root.path()).exists());
        assert!(matches!(
            ensure_daemon_supervisor(&TestHost, root.path())?,
            DaemonSupervisorStart::Fallback
        ));
        assert!(!ctx_daemon_runtime::supervisor_environment_path(root.path()).exists());
        assert_eq!(stored_supervisor_report(root.path())["status"], "fallback");
        Ok(())
    }

    #[test]
    fn managed_root_relocation_retains_saved_provider_proxy_and_bound_credentials() -> Result<()> {
        let roots = tempfile::tempdir()?;
        let source = roots.path().join("source");
        let destination = roots.path().join("other volume");
        let mut environment = supervisor_environment_snapshot(&TestHost)?;
        environment.values = vec![
            (
                "CODEX_HOME".to_owned(),
                roots.path().join("provider").display().to_string(),
            ),
            ("HTTPS_PROXY".to_owned(), "http://127.0.0.1:9".to_owned()),
            (
                crate::SEMANTIC_EMBEDDING_TOKEN_ENDPOINT_ENV.to_owned(),
                "http://127.0.0.1:12345/".to_owned(),
            ),
            (
                crate::SEMANTIC_EMBEDDING_TOKEN_ENV.to_owned(),
                "synthetic-test-credential".to_owned(),
            ),
        ];
        environment.values.sort();
        let environment = environment.with_loop_interval_seconds(Some(42))?;
        let original = ManagedRootSupervisor { environment };
        original.persist(&source)?;
        let captured = ManagedRootSupervisor::capture(&TestHost, &source)?;
        captured.persist(&destination)?;
        let copied = ManagedRootSupervisor::capture(&TestHost, &destination)?;
        assert_eq!(copied.environment, original.environment);
        let receipt = stored_supervisor_report(&destination);
        assert_eq!(receipt["status"], "disabled");
        assert_eq!(receipt["registration_verified"], false);
        assert!(receipt["owner_pid"].is_null());
        let launch = environment::supervisor_artifact_spec(
            SupervisorIdentity::new("ctx", destination.join("service"))?,
            &std::env::current_exe()?,
            &destination,
            &copied.environment,
        )?;
        assert_eq!(
            launch.environment_path(),
            ctx_daemon_runtime::supervisor_environment_path(&destination)
        );
        assert_eq!(launch.launch().args().nth(1), Some(destination.as_os_str()));
        Ok(())
    }
}
