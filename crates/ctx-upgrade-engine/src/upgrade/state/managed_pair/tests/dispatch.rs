use std::sync::{Arc, Mutex};

use super::*;
use crate::upgrade::{
    managed_pair::{
        recover_foreground_before_generic, tests::with_fixture_verifier,
        ForegroundManagedPairRecovery,
    },
    AutomaticUpgradeObservation, AutomaticUpgradePolicyProvider, AutomaticUpgradePolicySnapshot,
    DaemonRestart, DaemonUpgradeLease, DaemonUpgradePort, ProductBuildIdentity, ReleaseTransport,
    UpgradeEngine, UpgradeObserver, UpgradeTerminalStatus, TEST_RELEASE_PROCESS,
    TEST_SEMANTIC_LAYOUT,
};
use ctx_companion_bridge::ReleaseChannel;

struct NoTransport;
impl ReleaseTransport for NoTransport {
    fn get_bytes_limited(&self, _: &str, _: usize) -> Result<Vec<u8>> {
        Err(anyhow!("unexpected recovery download"))
    }
    fn download_artifact(&self, _: &str, _: &mut fs::File, _: u64, _: Duration) -> Result<u64> {
        Err(anyhow!("unexpected recovery download"))
    }
}

#[derive(Clone, Copy)]
struct Policy {
    enabled: bool,
    channel: &'static str,
}
impl AutomaticUpgradePolicySnapshot for Policy {
    fn daemon_maintenance_enabled(&self) -> bool {
        self.enabled
    }
    fn automatic_upgrade_enabled(&self) -> bool {
        self.enabled
    }
    fn interval(&self) -> Duration {
        Duration::from_secs(60)
    }
    fn channel(&self) -> &str {
        self.channel
    }
    fn semantic_enabled(&self) -> bool {
        false
    }
}
struct Policies {
    origin: PathBuf,
    policy: Policy,
    reads: Mutex<Vec<PathBuf>>,
}
impl AutomaticUpgradePolicyProvider for Policies {
    type Snapshot = Policy;
    fn reload(&self, root: &Path) -> Result<Policy> {
        self.reads.lock().unwrap().push(root.to_owned());
        Ok(if root == self.origin {
            self.policy
        } else {
            Policy {
                enabled: true,
                channel: "stable",
            }
        })
    }
}

struct Trace {
    root: PathBuf,
    origin: PathBuf,
    install: PathBuf,
    attempt: String,
    before_core: Vec<u8>,
    pending_attempt: Option<String>,
    calls: Mutex<Vec<&'static str>>,
    terminals: Mutex<Vec<(PathBuf, String, UpgradeTerminalStatus, bool)>>,
}
impl DaemonUpgradeLease for Arc<Trace> {
    fn wait_for_installation_quiescence(&self) -> Result<()> {
        Ok(())
    }
    fn replacement_restart(&self) -> Option<DaemonRestart<'_>> {
        Some(DaemonRestart {
            trigger: "automatic_upgrade",
            loop_interval_seconds: Some(60),
        })
    }
    fn resume_with(self, executable: &Path) -> Result<()> {
        assert_eq!(executable, self.install);
        assert!(InstallationLock::try_acquire_at_root(&self.root)?.is_some());
        assert_eq!(read_state_object(&self.install).status, "applied");
        assert_eq!(fs::read(executable)?, b"new-core");
        self.calls.lock().unwrap().push("resume");
        Ok(())
    }
    fn transfer_to_replacement_helper(self, _: u32) -> Result<()> {
        Err(anyhow!("unexpected helper"))
    }
    fn release_for_current_format_reexec(self) -> Result<()> {
        Err(anyhow!("unexpected reexec"))
    }
}
impl DaemonUpgradePort for Arc<Trace> {
    type Lease = Self;
    fn begin(&self, root: &Path, attempt: &str) -> Result<Self> {
        assert_eq!(root, self.origin);
        assert_eq!(attempt, self.attempt);
        assert!(InstallationLock::try_acquire_at_root(&self.root)?.is_none());
        let state = read_state_object(&self.install);
        assert_eq!(state.status, "recovering");
        assert_eq!(state.attempt_id.as_deref(), Some(self.attempt.as_str()));
        assert_eq!(state.plan[RESTART_TRIGGER_KEY], "automatic_upgrade");
        assert_eq!(fs::read(&self.install)?, self.before_core);
        let pending: Option<Value> = fs::read(
            self.root
                .join(MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH),
        )
        .ok()
        .map(|bytes| serde_json::from_slice(&bytes).unwrap());
        assert_eq!(
            pending
                .as_ref()
                .and_then(|value| value["attempt_id"].as_str()),
            self.pending_attempt.as_deref()
        );
        self.calls.lock().unwrap().push("begin");
        Ok(self.clone())
    }
    fn begin_current(&self, _: &Path, _: &str, _: &str, _: Option<u64>) -> Result<Self> {
        unreachable!()
    }
    fn mark_replacement_helper_handoff(&self, _: &Path, _: &str, _: u32) -> Result<()> {
        unreachable!()
    }
    fn complete_replacement_handoff(
        &self,
        _: &Path,
        _: &Path,
        _: &str,
        _: Option<DaemonRestart<'_>>,
    ) -> Result<()> {
        unreachable!()
    }
    fn finish_replacement_handoff(&self, _: &Path, _: &str) -> Result<()> {
        unreachable!()
    }
}
impl UpgradeObserver<Policy> for Trace {
    fn observe_automatic_terminal(
        &self,
        root: &Path,
        _: &Policy,
        event: AutomaticUpgradeObservation<'_>,
    ) {
        assert!(event.failure_kind.is_none());
        self.terminals.lock().unwrap().push((
            root.to_owned(),
            event.attempt_id.to_owned(),
            event.status,
            event.applied,
        ));
    }
}

fn run_case(case: &str) -> Result<()> {
    let output = std::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "upgrade::state::managed_pair::tests::dispatch::dispatch_probe",
            "--nocapture",
        ])
        .env("CTX_PAIR_DISPATCH_CASE", case)
        .output()?;
    assert!(
        output.status.success(),
        "{case}: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    Ok(())
}

#[test]
fn foreground_and_automatic_dispatch_preserve_recovery_contracts() -> Result<()> {
    for case in [
        "foreground",
        "automatic",
        "active_committed",
        "origin_disabled",
        "channel_changed",
        "manual_attempt",
        "foreground_core_mismatch",
        "automatic_core_mismatch",
        "foreground_envelope_mismatch",
        "automatic_envelope_mismatch",
        "foreground_replaced_pending",
        "automatic_replaced_pending",
    ] {
        run_case(case)?;
    }
    Ok(())
}

#[test]
fn dispatch_probe() -> Result<()> {
    let Ok(case) = std::env::var("CTX_PAIR_DISPATCH_CASE") else {
        return Ok(());
    };
    let fixture = RecoveryFixture::new()?;
    let lock = fixture.lock()?;
    let (attempt, _) = fixture.stage_failed_attempt(&lock)?;
    // Only this isolated child runs; no other test observes the target override.
    unsafe {
        std::env::set_var("CTX_UPGRADE_TEST_TARGET", &fixture.install_path);
    }
    let pending_path = fixture
        .root
        .join(MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH);
    let retained = fixture.root.join("share/ctx/.managed-pair-apply-v1");
    let mut state = read_state_object(&fixture.install_path);
    let mut envelope = b"signed-recovery-envelope".to_vec();
    let mut identity = fixture.identity.clone();
    if case.ends_with("core_mismatch") {
        state
            .plan
            .insert(CORE_SHA256_KEY.to_owned(), json!("a".repeat(64)));
    }
    if case.ends_with("envelope_mismatch") {
        state
            .plan
            .insert(ENVELOPE_SHA256_KEY.to_owned(), json!("b".repeat(64)));
    }
    if case.ends_with("replaced_pending") {
        envelope = b"replacement-signed-envelope".to_vec();
        identity = VerifiedManagedPairIdentity::new(
            "replacement",
            fixture.identity.target(),
            2,
            digest(b"replacement-manifest"),
            fixture.identity.core().clone(),
            fixture.identity.companion().clone(),
        )?;
        fs::write(
            retained.join("share/ctx/managed-pair-envelope.json"),
            &envelope,
        )?;
        let mut pending: Value = serde_json::from_slice(&fs::read(&pending_path)?)?;
        pending["attempt_id"] = json!("11111111111111111111111111111111");
        pending["candidate_envelope_identity"] =
            json!({"sha256":digest(&envelope),"size_bytes":envelope.len()});
        fs::write(&pending_path, serde_json::to_vec(&pending)?)?;
    }
    if case == "manual_attempt" {
        state.attempt_source = Some("manual_apply".to_owned());
    }
    if case == "active_committed" {
        resume_pending_managed_pair_under_installation_lock(&fixture.root, &fixture)?;
        state.status = "applying".to_owned();
    } else {
        fs::write(retained.join("share/ctx/cleanup-obstruction"), b"preserve")?;
    }
    write_state_object_locked(&lock, state)?;
    drop(lock);
    let state_path = fixture
        .install_path
        .with_file_name(".ctx.upgrade-state.json");
    let before_state = fs::read(&state_path)?;
    let before_pending = fs::read(&pending_path).ok();
    let before_envelope = fs::read(retained.join("share/ctx/managed-pair-envelope.json")).ok();
    let evidence_paths = [
        "bin/ctx",
        "libexec/ctx-pro",
        "bin/ctx.install.json",
        "share/ctx/managed-pair-envelope.json",
        "share/ctx/managed-pair-state.json",
    ];
    let before_files: Vec<_> = [&fixture.root, &retained]
        .into_iter()
        .flat_map(|root| {
            evidence_paths
                .iter()
                .map(move |path| fs::read(root.join(path)).ok())
        })
        .collect();
    let trace = Arc::new(Trace {
        root: fixture.root.clone(),
        origin: fixture.data_root.clone(),
        install: fixture.install_path.clone(),
        attempt: attempt.id().to_owned(),
        before_core: fs::read(&fixture.install_path)?,
        pending_attempt: before_pending.as_ref().map(|bytes| {
            let value: Value = serde_json::from_slice(bytes).unwrap();
            value["attempt_id"].as_str().unwrap().to_owned()
        }),
        calls: Mutex::new(vec![]),
        terminals: Mutex::new(vec![]),
    });
    let policies = Policies {
        origin: fixture.data_root.clone(),
        policy: Policy {
            enabled: case != "origin_disabled",
            channel: if case == "channel_changed" {
                "staging"
            } else {
                "stable"
            },
        },
        reads: Mutex::new(vec![]),
    };
    let engine = UpgradeEngine::new(
        ProductBuildIdentity::new("1.0.0"),
        &NoTransport,
        &TEST_RELEASE_PROCESS,
        &TEST_SEMANTIC_LAYOUT,
        &trace,
    );
    let rejects = case.ends_with("mismatch") || case.ends_with("replaced_pending");
    let skips = matches!(
        case.as_str(),
        "origin_disabled" | "channel_changed" | "manual_attempt"
    );
    let foreground = case.starts_with("foreground");
    let result = with_fixture_verifier(
        ReleaseChannel::Stable,
        envelope,
        identity,
        || -> Result<()> {
            if foreground {
                assert!(matches!(
                    recover_foreground_before_generic(&engine, false)?,
                    ForegroundManagedPairRecovery::Recovered
                ));
            } else {
                let caller = fixture.root.join("different-caller");
                let prepared = engine.prepare_automatic(
                    &policies,
                    trace.as_ref(),
                    &caller,
                    &Policy {
                        enabled: true,
                        channel: "stable",
                    },
                )?;
                if skips {
                    assert!(prepared.is_none());
                    return Ok(());
                }
                let prepared = prepared.ok_or_else(|| anyhow!("pending pair was not prepared"))?;
                assert_eq!(prepared.attempt_id(), Some(attempt.id()));
                assert_eq!(prepared.data_root(), fixture.data_root);
                assert_eq!(prepared.install_path(), fixture.install_path);
                let handoff = trace.begin(prepared.data_root(), prepared.attempt_id().unwrap())?;
                engine.finish_automatic(&policies, trace.as_ref(), prepared, Some(handoff))?;
            }
            Ok(())
        },
    );
    if rejects || skips {
        if rejects {
            assert!(
                format!("{:#}", result.unwrap_err()).contains("expected Core/envelope identity")
            );
        } else {
            result?;
        }
        assert_eq!(fs::read(state_path)?, before_state);
        assert_eq!(fs::read(&pending_path).ok(), before_pending);
        assert_eq!(
            fs::read(retained.join("share/ctx/managed-pair-envelope.json")).ok(),
            before_envelope
        );
        assert_eq!(fs::read(&fixture.install_path)?, trace.before_core);
        let after_files: Vec<_> = [&fixture.root, &retained]
            .into_iter()
            .flat_map(|root| {
                evidence_paths
                    .iter()
                    .map(move |path| fs::read(root.join(path)).ok())
            })
            .collect();
        assert_eq!(after_files, before_files);
        assert!(trace.calls.lock().unwrap().is_empty());
        assert!(trace.terminals.lock().unwrap().is_empty());
    } else {
        result?;
        let state = read_state_object(&fixture.install_path);
        assert_eq!(state.status, "applied");
        assert_eq!(state.attempt_id.as_deref(), Some(attempt.id()));
        assert_eq!(state.consecutive_failures, 0);
        assert_eq!(state.next_retry_unix_s, None);
        assert_eq!(trace.calls.lock().unwrap().as_slice(), ["begin", "resume"]);
        if !foreground {
            assert!(policies.reads.lock().unwrap().contains(&fixture.data_root));
            assert_eq!(
                trace.terminals.lock().unwrap().as_slice(),
                [(
                    fixture.data_root.clone(),
                    attempt.id().to_owned(),
                    UpgradeTerminalStatus::Applied,
                    true
                )]
            );
        }
    }
    Ok(())
}
