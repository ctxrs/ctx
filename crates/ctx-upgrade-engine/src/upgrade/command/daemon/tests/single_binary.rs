use super::super::*;
use crate::upgrade::command::single_binary_tests::{child, Fixture};
use std::{fs, sync::Mutex};

struct Policy(bool);
impl AutomaticUpgradePolicySnapshot for Policy {
    fn daemon_maintenance_enabled(&self) -> bool {
        self.0
    }
    fn automatic_upgrade_enabled(&self) -> bool {
        self.0
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
impl AutomaticUpgradePolicyProvider for Policy {
    type Snapshot = Policy;
    fn reload(&self, _: &Path) -> Result<Policy> {
        Ok(Policy(self.0))
    }
}
#[derive(Default)]
struct Observer(Mutex<Vec<(UpgradeTerminalStatus, bool)>>);
impl UpgradeObserver<Policy> for Observer {
    fn observe_automatic_terminal(
        &self,
        _: &Path,
        _: &Policy,
        observation: AutomaticUpgradeObservation<'_>,
    ) {
        self.0
            .lock()
            .unwrap()
            .push((observation.status, observation.applied));
    }
}

#[test]
fn single_binary_automatic_owner_and_policy() -> Result<()> {
    for case in [
        "automatic_newer",
        "automatic_disabled",
        "automatic_no_handoff",
        "automatic_off",
    ] {
        child(
            case,
            "upgrade::command::daemon::tests::single_binary::automatic_single_probe",
        )?;
    }
    Ok(())
}

#[test]
fn automatic_single_probe() -> Result<()> {
    let Ok(case) = std::env::var("CTX_SINGLE_BINARY_CASE") else {
        return Ok(());
    };
    let f = Fixture::new(&case)?;
    let observer = Observer::default();
    if case == "automatic_off" {
        assert!(f
            .engine()
            .prepare_automatic(&Policy(false), &observer, &f.data, &Policy(false))?
            .is_none());
        assert!(f.trace.calls.lock().unwrap().is_empty());
        assert!(f.requests().is_empty());
        assert_eq!(
            fs::read(&f.plan.install_path)?,
            b"#!/bin/sh\necho 'ctx 1.5.0'\n"
        );
        assert!(!f.root.join("libexec/ctx-pro").exists());
        return Ok(());
    }
    let lock = UpgradeLock::acquire(&f.data)?;
    let attempt = begin_automatic_attempt_locked(&lock, Duration::from_secs(60))?.unwrap();
    // Authenticated-plan fixture enters the actual automatic completion owner.
    // The ordinary prepare owner uses these same download operation.
    let core = download_core_artifact(&f.transport, &f.data, &f.plan)?;
    write_state_checked_locked(
        &f.data,
        &lock,
        &attempt,
        &f.plan,
        "staged",
        Duration::from_secs(60),
    )?;
    let prepared = PreparedAutomaticUpgrade(PreparedAutomaticUpgradeKind::Apply {
        data_root: f.data.clone(),
        interval: Duration::from_secs(60),
        started: Instant::now(),
        lock,
        attempt,
        plan: f.plan.clone(),
        core,
        provisioning: PreparedProvisioningArtifacts {
            runtime: None,
            semantic: vec![],
        },
    });
    let handoff = if case == "automatic_no_handoff" {
        None
    } else {
        Some(f.trace.begin(&f.data, prepared.attempt_id().unwrap())?)
    };
    let result = f.engine().finish_automatic(
        &Policy(case != "automatic_disabled"),
        &observer,
        prepared,
        handoff,
    );
    if case == "automatic_no_handoff" {
        assert!(format!("{:#}", result.unwrap_err()).contains("no daemon lifecycle handoff"));
        assert!(f.trace.calls.lock().unwrap().is_empty());
        assert_eq!(
            fs::read(&f.plan.install_path)?,
            b"#!/bin/sh\necho 'ctx 1.5.0'\n"
        );
        assert!(!f.root.join("libexec/ctx-pro").exists());
    } else {
        result?;
        assert_eq!(
            fs::read_dir(f.data.join(".ctx-upgrade-downloads"))?.count(),
            0,
            "automatic downloads must be released after completion"
        );
        assert_eq!(*f.trace.calls.lock().unwrap(), ["begin", "resume"]);
        assert_eq!(
            *observer.0.lock().unwrap(),
            [if case == "automatic_disabled" {
                (UpgradeTerminalStatus::Skipped, false)
            } else {
                (UpgradeTerminalStatus::Applied, true)
            }]
        );
    }
    Ok(())
}
