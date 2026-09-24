//! Authenticated-plan fixtures exercise the real single-executable transaction.
//! These synthetic shell executables are unit evidence, not native helper proof.
use super::*;
use crate::upgrade::{
    install::{install_marker_path, InstallationLock},
    metadata::{ManagedPairReleaseMetadata, ReleaseMetadata},
    sha256_hex, ProductBuildIdentity, ReleaseTransport, TEST_RELEASE_PROCESS, TEST_SEMANTIC_LAYOUT,
};
use anyhow::bail;
use ctx_history_platform::platform_security::{
    create_private_directory_all, restrict_private_file,
};
use serde_json::{json, Value};
use std::{
    fs,
    io::Write as _,
    os::unix::fs::PermissionsExt as _,
    sync::{Arc, Mutex},
};

const CORE: &[u8] = b"#!/bin/sh\necho 'ctx 1.5.0'\n";
const NEXT: &[u8] = b"#!/bin/sh\necho 'ctx 1.5.1'\n";

pub(super) struct Fixture {
    _temp: tempfile::TempDir,
    pub(super) root: PathBuf,
    pub(super) data: PathBuf,
    pub(super) plan: UpgradePlan,
    pub(super) transport: Transport,
    pub(super) trace: Arc<Trace>,
}

pub(super) struct Transport {
    url: String,
    bytes: Vec<u8>,
    log: Arc<Mutex<Vec<String>>>,
}
impl ReleaseTransport for Transport {
    fn get_bytes_limited(&self, endpoint: &str, _: usize) -> Result<Vec<u8>> {
        bail!("unexpected metadata or legacy pair request {endpoint}")
    }
    fn download_artifact(
        &self,
        endpoint: &str,
        destination: &mut fs::File,
        max: u64,
        _: Duration,
    ) -> Result<u64> {
        assert_eq!(endpoint, self.url, "must use the ordinary executable slot");
        assert_eq!(
            max,
            256 * 1024 * 1024,
            "use the current executable download bound"
        );
        self.log.lock().unwrap().push(endpoint.to_owned());
        destination.write_all(&self.bytes)?;
        Ok(self.bytes.len() as u64)
    }
}

pub(super) struct Trace {
    core: PathBuf,
    data: PathBuf,
    case: String,
    pub(super) calls: Mutex<Vec<&'static str>>,
}
impl DaemonUpgradeLease for Arc<Trace> {
    fn wait_for_installation_quiescence(&self) -> Result<()> {
        Ok(())
    }
    fn replacement_restart(&self) -> Option<crate::DaemonRestart<'_>> {
        None
    }
    fn resume_with(self, executable: &Path) -> Result<()> {
        assert_eq!(executable, self.core);
        let expected = if self.case == "stale_at_handoff" || self.case == "automatic_disabled" {
            CORE
        } else {
            NEXT
        };
        assert_eq!(fs::read(executable)?, expected);
        self.calls.lock().unwrap().push("resume");
        if self.case == "restart_failure" {
            bail!("injected restart failure");
        }
        Ok(())
    }
    fn transfer_to_replacement_helper(self, _: u32) -> Result<()> {
        unreachable!()
    }
    fn release_for_current_format_reexec(self) -> Result<()> {
        unreachable!()
    }
}
impl DaemonUpgradePort for Arc<Trace> {
    type Lease = Self;
    fn begin(&self, root: &Path, _: &str) -> Result<Self> {
        assert_eq!(root, self.data);
        assert_eq!(fs::read(&self.core)?, CORE);
        assert!(InstallationLock::try_acquire(&self.core)?.is_none());
        self.calls.lock().unwrap().push("begin");
        if self.case == "handoff_failure" {
            bail!("injected handoff failure");
        }
        if self.case == "stale_at_handoff" {
            let marker = install_marker_path(&self.core);
            let mut bytes = fs::read(&marker)?;
            bytes.push(b' ');
            fs::write(marker, bytes)?;
        }
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
        _: Option<crate::DaemonRestart<'_>>,
    ) -> Result<()> {
        unreachable!()
    }
    fn finish_replacement_handoff(&self, _: &Path, _: &str) -> Result<()> {
        unreachable!()
    }
}

pub(super) fn policy() -> UpgradePolicy<'static> {
    UpgradePolicy {
        channel: "stable",
        interval: Duration::from_secs(60),
        semantic_enabled: false,
    }
}
impl Fixture {
    pub(super) fn new(case: &str) -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let root = fs::canonicalize(temp.path())?.join("install");
        let data = temp.path().join("data");
        create_private_directory_all(&root.join("bin"))?;
        create_private_directory_all(&data)?;
        // Run only in a dedicated --exact child, never mutate the parent test environment.
        for key in [
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "XDG_RUNTIME_DIR",
            "CTX_DATA_ROOT",
            "CODEX_HOME",
            "CLAUDE_CONFIG_DIR",
        ] {
            let path = temp.path().join(key);
            create_private_directory_all(&path)?;
            std::env::set_var(key, path);
        }
        let core = root.join("bin/ctx");
        fs::write(&core, CORE)?;
        fs::set_permissions(&core, fs::Permissions::from_mode(0o700))?;
        std::env::set_var("CTX_UPGRADE_TEST_TARGET", &core);
        let platform = platform_key()?;
        let marker = json!({"schema_version":1,"manager":"ctx-hosted-installer","install_path":core,
            "platform":platform,"channel":"stable","version":"1.5.0","sha256":sha256_hex(CORE)});
        fs::write(install_marker_path(&core), serde_json::to_vec(&marker)?)?;
        restrict_private_file(&install_marker_path(&core))?;
        let mut warnings = vec![];
        let snapshot = capture_install_snapshot(true, platform, "stable", "1.5.0", &mut warnings)?;
        let base = "https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/1.5.1";
        let url = format!("{base}/ctx");
        let sha = sha256_hex(NEXT);
        let plan = UpgradePlan {
            current_version: "1.5.0".to_owned(),
            latest_version: "1.5.1".to_owned(),
            channel: "stable".to_owned(),
            platform: platform.to_owned(),
            metadata_url: metadata_url("stable"),
            artifact_url: url.clone(),
            artifact_sha256: sha.clone(),
            install_path: core.clone(),
            install_fingerprint: snapshot.fingerprint,
            update_available: true,
            managed: true,
            warnings,
            // Retained signed metadata can advertise a pair for old clients. New upgrades never request it.
            managed_pair_release: Some(ManagedPairReleaseMetadata {
                envelope_url: format!("{base}/legacy.json"),
                core_object_url: format!("{base}/sha256/{sha}/ctx"),
                core_sha256: sha.clone(),
                companion_object_url: format!("{base}/sha256/{sha}/ctx-pro"),
                companion_sha256: sha.clone(),
            }),
            metadata: ReleaseMetadata {
                version: "1.5.1".to_owned(),
                base_url: base.to_owned(),
                artifact: "ctx".to_owned(),
                sha256: sha,
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
        let transport = Transport {
            url,
            bytes: NEXT.to_vec(),
            log: Arc::new(Mutex::new(vec![])),
        };
        let trace = Arc::new(Trace {
            core,
            data: data.clone(),
            case: case.to_owned(),
            calls: Mutex::new(vec![]),
        });
        Ok(Self {
            _temp: temp,
            root,
            data,
            plan,
            transport,
            trace,
        })
    }
    pub(super) fn requests(&self) -> Vec<String> {
        self.transport.log.lock().unwrap().clone()
    }
    pub(super) fn engine(&self) -> UpgradeEngine<'_, Arc<Trace>> {
        UpgradeEngine::new(
            ProductBuildIdentity::new("1.5.0"),
            &self.transport,
            &TEST_RELEASE_PROCESS,
            &TEST_SEMANTIC_LAYOUT,
            &self.trace,
        )
    }
    fn apply(&self, dry_run: bool) -> Result<UpgradeOutcome> {
        let lock = UpgradeLock::acquire(&self.data)?;
        let attempt = begin_manual_attempt_locked(&self.data, &lock, "manual_apply")?;
        apply_planned_upgrade(
            &self.engine(),
            &self.data,
            policy(),
            dry_run,
            &lock,
            &attempt,
            self.plan.clone(),
        )
    }
}

pub(super) fn child(case: &str, test: &str) -> Result<()> {
    let output = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", test, "--nocapture"])
        .env("CTX_SINGLE_BINARY_CASE", case)
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
fn ordinary_upgrade_uses_one_artifact_and_preserves_handoff() -> Result<()> {
    for case in [
        "newer",
        "same",
        "dry_run",
        "source",
        "tamper",
        "disallowed",
        "stale_at_handoff",
        "handoff_failure",
        "restart_failure",
    ] {
        child(
            case,
            "upgrade::command::single_binary_tests::single_binary_probe",
        )?;
    }
    Ok(())
}

#[test]
fn single_binary_probe() -> Result<()> {
    let Ok(case) = std::env::var("CTX_SINGLE_BINARY_CASE") else {
        return Ok(());
    };
    let mut f = Fixture::new(&case)?;
    match case.as_str() {
        "source" => {
            fs::remove_file(install_marker_path(&f.plan.install_path))?;
            assert!(format!(
                "{:#}",
                f.engine()
                    .apply(&f.data, policy(), None, false)
                    .unwrap_err()
            )
            .contains("not installed by the hosted installer"));
            assert!(f.requests().is_empty());
            return Ok(());
        }
        "same" => {
            f.plan.update_available = false;
            f.plan.latest_version = "1.5.0".to_owned();
        }
        "tamper" => f.transport.bytes[0] ^= 1,
        "disallowed" => f.plan.metadata.self_upgrade_allowed = false,
        _ => {}
    }
    let result = f.apply(case == "dry_run");
    match case.as_str() {
        "tamper" | "disallowed" | "stale_at_handoff" | "handoff_failure" => {
            let error = result.unwrap_err();
            let expected = match case.as_str() {
                "tamper" => "checksum",
                "disallowed" => "does not allow",
                "stale_at_handoff" => "changed after this upgrade plan",
                _ => "handoff failure",
            };
            assert!(format!("{error:#}").contains(expected), "{error:#}");
            assert_eq!(fs::read(&f.plan.install_path)?, CORE);
        }
        "same" | "dry_run" => {
            assert!(!result?.applied());
            assert!(f.requests().is_empty());
            assert!(f.trace.calls.lock().unwrap().is_empty());
        }
        _ => {
            let outcome = result?;
            assert!(outcome.applied());
            assert_eq!(f.requests(), [f.plan.artifact_url.clone()]);
            assert_eq!(fs::read(&f.plan.install_path)?, NEXT);
            let marker: Value =
                serde_json::from_slice(&fs::read(install_marker_path(&f.plan.install_path))?)?;
            assert!(marker.get("managed_pair").is_none());
            assert_eq!(*f.trace.calls.lock().unwrap(), ["begin", "resume"]);
            if case == "restart_failure" {
                assert!(outcome
                    .warnings()
                    .iter()
                    .any(|w| w.contains("restart is pending")));
            }
        }
    }
    assert!(!f.root.join("libexec/ctx-pro").exists());
    assert!(!f.root.join("share/ctx/managed-pair-state.json").exists());
    Ok(())
}

struct MigrationDaemon {
    install: PathBuf,
    calls: Arc<Mutex<Vec<&'static str>>>,
}

struct MigrationLease {
    install: PathBuf,
    calls: Arc<Mutex<Vec<&'static str>>>,
}

impl DaemonUpgradeLease for MigrationLease {
    fn wait_for_installation_quiescence(&self) -> Result<()> {
        Ok(())
    }
    fn replacement_restart(&self) -> Option<crate::DaemonRestart<'_>> {
        None
    }
    fn resume_with(self, executable: &Path) -> Result<()> {
        assert_eq!(executable, self.install);
        assert!(InstallationLock::try_acquire(executable)?.is_none());
        let state: Value = serde_json::from_slice(&fs::read(
            executable.with_file_name(".ctx.upgrade-state.json"),
        )?)?;
        assert!(matches!(
            state["status"].as_str(),
            Some("applied" | "error")
        ));
        self.calls.lock().unwrap().push("resume");
        Ok(())
    }
    fn transfer_to_replacement_helper(self, _: u32) -> Result<()> {
        unreachable!()
    }
    fn release_for_current_format_reexec(self) -> Result<()> {
        unreachable!()
    }
}

impl DaemonUpgradePort for MigrationDaemon {
    type Lease = MigrationLease;
    fn begin(&self, _: &Path, _: &str) -> Result<Self::Lease> {
        unreachable!()
    }
    fn begin_for_installation(&self, _: &Path, _: &str, install: &Path) -> Result<Self::Lease> {
        assert_eq!(install, self.install);
        assert!(InstallationLock::try_acquire(install)?.is_none());
        let state: Value = serde_json::from_slice(&fs::read(
            install.with_file_name(".ctx.upgrade-state.json"),
        )?)?;
        assert_eq!(state["status"], "quiescing");
        self.calls.lock().unwrap().push("begin");
        Ok(MigrationLease {
            install: self.install.clone(),
            calls: self.calls.clone(),
        })
    }
    fn begin_current(&self, _: &Path, _: &str, _: &str, _: Option<u64>) -> Result<Self::Lease> {
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
        _: Option<crate::DaemonRestart<'_>>,
    ) -> Result<()> {
        unreachable!()
    }
    fn finish_replacement_handoff(&self, _: &Path, _: &str) -> Result<()> {
        unreachable!()
    }
}

#[test]
fn hosted_migration_quiesces_installed_image_and_keeps_prior_on_bad_digest() -> Result<()> {
    for (bad_digest, fault_after_binary) in [(true, false), (false, false), (false, true)] {
        let temp = tempfile::tempdir()?;
        let install = temp.path().join("install/bin/ctx");
        let data = temp.path().join("data");
        create_private_directory_all(install.parent().unwrap())?;
        create_private_directory_all(&data)?;
        fs::write(&install, CORE)?;
        fs::set_permissions(&install, fs::Permissions::from_mode(0o700))?;
        let prior_marker = json!({"schema_version":1,"manager":"ctx-hosted-installer",
            "install_path":install,"platform":platform_key()?,"channel":"stable",
            "version":"1.6.3","sha256":sha256_hex(CORE)});
        fs::write(
            install_marker_path(&install),
            serde_json::to_vec(&prior_marker)?,
        )?;
        restrict_private_file(&install_marker_path(&install))?;
        let source = fs::canonicalize(std::env::current_exe()?)?;
        let digest = sha256_hex(&fs::read(&source)?);
        let candidate_marker = temp.path().join("candidate-marker.json");
        fs::write(
            &candidate_marker,
            serde_json::to_vec(&json!({
                "schema_version":1,"manager":"ctx-hosted-installer","install_path":install,
                "platform":platform_key()?,"channel":"stable","version":"1.7.0",
                "sha256":digest,
            }))?,
        )?;
        let calls = Arc::new(Mutex::new(Vec::new()));
        let daemon = MigrationDaemon {
            install: install.clone(),
            calls: calls.clone(),
        };
        let transport = Transport {
            url: String::new(),
            bytes: Vec::new(),
            log: Arc::new(Mutex::new(Vec::new())),
        };
        let engine = UpgradeEngine::new(
            ProductBuildIdentity::new("1.7.0"),
            &transport,
            &TEST_RELEASE_PROCESS,
            &TEST_SEMANTIC_LAYOUT,
            &daemon,
        );
        let args = || crate::HostedTransactionArgs {
            action: crate::HostedTransactionAction::Install,
            install_path: install.clone(),
            attempt_id: Some("ia_12345678".into()),
            marker_source: Some(candidate_marker.clone()),
            ownership_source: None,
            binary_sha256: Some(if bad_digest {
                "0".repeat(64)
            } else {
                digest.clone()
            }),
        };
        if fault_after_binary {
            crate::upgrade::install::set_hosted_install_fault_for_test(Some("binary_replaced"));
        }
        let result = engine.migrate_hosted_install(&data, args());
        if bad_digest {
            assert_eq!(*calls.lock().unwrap(), ["begin", "resume"]);
            assert!(result.is_err());
            assert_eq!(fs::read(&install)?, CORE);
            assert_eq!(
                serde_json::from_slice::<Value>(&fs::read(install_marker_path(&install))?)?,
                prior_marker
            );
        } else if fault_after_binary {
            assert!(result.is_err());
            assert_eq!(*calls.lock().unwrap(), ["begin"]);
            assert_eq!(sha256_hex(&fs::read(&install)?), digest);
            assert_eq!(
                serde_json::from_slice::<Value>(&fs::read(install_marker_path(&install))?)?,
                prior_marker
            );
            let state: Value = serde_json::from_slice(&fs::read(
                install.with_file_name(".ctx.upgrade-state.json"),
            )?)?;
            assert_eq!(state["status"], "quiescing");
            assert!(install
                .with_file_name(".ctx.hosted-install-transaction.json")
                .exists());
            engine.migrate_hosted_install(&data, args())?;
            assert_eq!(*calls.lock().unwrap(), ["begin", "begin", "resume"]);
            assert!(!install
                .with_file_name(".ctx.hosted-install-transaction.json")
                .exists());
            assert_eq!(sha256_hex(&fs::read(&install)?), digest);
            let marker: Value = serde_json::from_slice(&fs::read(install_marker_path(&install))?)?;
            assert_eq!(marker["sha256"], digest);
        } else {
            assert_eq!(*calls.lock().unwrap(), ["begin", "resume"]);
            result?;
            assert_eq!(sha256_hex(&fs::read(&install)?), digest);
            let marker: Value = serde_json::from_slice(&fs::read(install_marker_path(&install))?)?;
            assert_eq!(marker["version"], "1.7.0");
            assert_eq!(marker["sha256"], digest);
        }
    }
    Ok(())
}
