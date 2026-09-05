use super::super::*;
use crate::upgrade::command::first_pair_tests::{child, Fixture};
use crate::upgrade::managed_pair::tests::with_fixture_verifier;
use ctx_companion_bridge::ReleaseChannel;
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
fn first_pair_automatic_owner_and_policy() -> Result<()> {
    for case in [
        "automatic_same",
        "automatic_newer",
        "automatic_disabled",
        "automatic_no_handoff",
        "automatic_off",
    ] {
        child(
            case,
            "upgrade::command::daemon::tests::first_pair::automatic_pair_probe",
        )?;
    }
    Ok(())
}

#[test]
fn automatic_pair_probe() -> Result<()> {
    let Ok(case) = std::env::var("CTX_FIRST_PAIR_CASE") else {
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
        assert_eq!(fs::read(&f.plan.install_path)?, b"installed-core");
        assert!(!f.root.join("libexec/ctx-pro").exists());
        return Ok(());
    }
    let lock = UpgradeLock::acquire(&f.data)?;
    let attempt = begin_automatic_attempt_locked(&lock, Duration::from_secs(60))?.unwrap();
    // Authenticated-plan fixture enters the actual automatic completion owner.
    // The ordinary prepare owner uses these same mode/download operations.
    let pair_mode = inspect_plan_under_installation_lock(&f.plan, lock.installation())?;
    assert!(pair_mode.pair_apply_required(&f.plan));
    let core = f.verified(|| download_core_artifact(&f.transport, &f.data, &f.plan, &pair_mode))?;
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
        pair_mode,
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
    let result = with_fixture_verifier(
        ReleaseChannel::Stable,
        b"opaque-fixture-envelope".to_vec(),
        f.identity.clone(),
        || {
            f.engine().finish_automatic(
                &Policy(case != "automatic_disabled"),
                &observer,
                prepared,
                handoff,
            )
        },
    );
    if case == "automatic_no_handoff" {
        assert!(format!("{:#}", result.unwrap_err()).contains("no daemon lifecycle handoff"));
        assert!(f.trace.calls.lock().unwrap().is_empty());
        assert_eq!(fs::read(&f.plan.install_path)?, b"installed-core");
        assert!(!f.root.join("libexec/ctx-pro").exists());
    } else {
        result?;
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
