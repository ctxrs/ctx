use std::{collections::BTreeMap, fs, io::Write as _, sync::Mutex, time::Duration};

use ctx_history_platform::platform_security::{restrict_private_directory, restrict_private_file};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use tempfile::tempdir;

use super::*;
use crate::upgrade::{
    install::{install_marker_path, InstallFingerprint},
    metadata::{ManagedPairReleaseMetadata, ReleaseMetadata},
};

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

struct FixtureVerifier(BTreeMap<Vec<u8>, VerifiedManagedPairIdentity>);

impl ManagedPairVerifier for FixtureVerifier {
    fn verify_signed_envelope(
        &self,
        signed_envelope: &[u8],
    ) -> Result<VerifiedManagedPairIdentity> {
        self.0
            .get(signed_envelope)
            .cloned()
            .ok_or_else(|| anyhow!("unexpected envelope"))
    }
}

struct FixtureTransport {
    bytes: BTreeMap<String, Vec<u8>>,
    downloads: Mutex<Vec<String>>,
}

impl ReleaseTransport for FixtureTransport {
    fn get_bytes_limited(&self, endpoint: &str, max_bytes: usize) -> Result<Vec<u8>> {
        let bytes = self
            .bytes
            .get(endpoint)
            .ok_or_else(|| anyhow!("unexpected endpoint {endpoint}"))?
            .clone();
        if bytes.len() > max_bytes {
            bail!("fixture response exceeds bound")
        }
        Ok(bytes)
    }

    fn download_artifact(
        &self,
        endpoint: &str,
        destination: &mut fs::File,
        max_bytes: u64,
        _timeout: Duration,
    ) -> Result<u64> {
        let bytes = self
            .bytes
            .get(endpoint)
            .ok_or_else(|| anyhow!("unexpected endpoint {endpoint}"))?;
        if bytes.len() as u64 > max_bytes {
            bail!("fixture artifact exceeds bound")
        }
        self.downloads.lock().unwrap().push(endpoint.to_owned());
        destination.write_all(bytes)?;
        Ok(bytes.len() as u64)
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn current_target() -> ManagedPairTarget {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "aarch64") => ManagedPairTarget::LinuxArm64,
        ("linux", "x86_64") => ManagedPairTarget::LinuxX64,
        ("macos", "aarch64") => ManagedPairTarget::MacosArm64,
        ("macos", "x86_64") => ManagedPairTarget::MacosX64,
        ("windows", "x86_64") => ManagedPairTarget::WindowsX64,
        pair => panic!("unsupported test target {pair:?}"),
    }
}

#[test]
fn managed_pair_apply_binds_resumed_candidate_to_requested_identity() -> Result<()> {
    let release_verifier = ReleaseManagedPairVerifier::for_channel("stable")?;
    assert!(release_verifier
        .verify_signed_envelope(b"not-a-signed-envelope")
        .is_err());

    let fixture = tempdir()?;
    restrict_private_directory(fixture.path())?;
    let bin = fixture.path().join("bin");
    fs::create_dir(&bin)?;
    restrict_private_directory(&bin)?;
    let core_path = bin.join(if cfg!(windows) { "ctx.exe" } else { "ctx" });
    fs::write(&core_path, b"old-core")?;
    restrict_private_file(&core_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&core_path, fs::Permissions::from_mode(0o700))?;
    }
    validate_managed_pair_helper_file(&core_path, &digest(b"old-core"))?;
    assert!(validate_managed_pair_helper_file(&core_path, &"0".repeat(64)).is_err());
    let marker_path = install_marker_path(&core_path);
    fs::write(
        &marker_path,
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "manager": "ctx-hosted-installer",
            "install_path": core_path,
            "platform": super::super::platform_key()?,
            "channel": "stable",
            "version": "1.0.0",
            "sha256": digest(b"old-core"),
            "installed_at": "2026-09-02T00:00:00Z",
            "man_pages": {"status": "installed"},
        }))?,
    )?;
    restrict_private_file(&marker_path)?;

    let next_core = b"next-core";
    let companion = b"next-companion";
    let core_sha = digest(next_core);
    let companion_sha = digest(companion);
    let requested_core = b"requested-core";
    let requested_companion = b"requested-companion";
    let requested_core_sha = digest(requested_core);
    let requested_companion_sha = digest(requested_companion);
    let plan = UpgradePlan {
        current_version: "1.0.0".to_owned(),
        latest_version: "1.1.0".to_owned(),
        channel: "stable".to_owned(),
        platform: super::super::platform_key()?.to_owned(),
        metadata_url: "metadata".to_owned(),
        artifact_url: "legacy-core".to_owned(),
        artifact_sha256: core_sha.clone(),
        install_path: core_path.clone(),
        install_fingerprint: InstallFingerprint {
            binary_sha256: digest(b"old-core"),
            marker_sha256: digest(&fs::read(&marker_path)?),
        },
        update_available: true,
        managed: true,
        warnings: Vec::new(),
        managed_pair_release: Some(ManagedPairReleaseMetadata {
            envelope_url: "pair-envelope".to_owned(),
            core_object_url: "pair-core".to_owned(),
            core_sha256: core_sha.clone(),
            companion_object_url: "pair-companion".to_owned(),
            companion_sha256: companion_sha.clone(),
        }),
        metadata: ReleaseMetadata {
            version: "1.1.0".to_owned(),
            base_url: "https://cli.ctx.rs/releases/1.1.0".to_owned(),
            artifact: "ctx".to_owned(),
            sha256: core_sha.clone(),
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
    let applied_identity = VerifiedManagedPairIdentity::new(
        "ctx-1.1.0",
        current_target(),
        2,
        "3".repeat(64),
        ManagedPairComponentIdentity::new(&core_sha, next_core.len() as u64)?,
        ManagedPairComponentIdentity::new(&companion_sha, companion.len() as u64)?,
    )?;
    let requested_identity = VerifiedManagedPairIdentity::new(
        "ctx-1.2.0",
        current_target(),
        3,
        "4".repeat(64),
        ManagedPairComponentIdentity::new(&requested_core_sha, requested_core.len() as u64)?,
        ManagedPairComponentIdentity::new(
            &requested_companion_sha,
            requested_companion.len() as u64,
        )?,
    )?;
    let verifier = FixtureVerifier(BTreeMap::from([
        (b"signed-envelope".to_vec(), applied_identity),
        (b"requested-envelope".to_vec(), requested_identity),
    ]));
    let mut staging_plan = plan.clone();
    staging_plan.channel = "staging".to_owned();
    let staging_marker: serde_json::Value = serde_json::from_slice(
        &install::install_marker_bytes(&marker_path, &staging_plan, None)?,
    )?;
    assert_eq!(staging_marker["staging_dogfood"], true);
    assert_eq!(staging_marker["managed_pair"], true);
    let transport = FixtureTransport {
        bytes: BTreeMap::from([
            ("pair-envelope".to_owned(), b"signed-envelope".to_vec()),
            ("pair-core".to_owned(), next_core.to_vec()),
            ("pair-companion".to_owned(), companion.to_vec()),
            (
                "requested-envelope".to_owned(),
                b"requested-envelope".to_vec(),
            ),
            ("requested-core".to_owned(), requested_core.to_vec()),
            (
                "requested-companion".to_owned(),
                requested_companion.to_vec(),
            ),
            ("legacy-core".to_owned(), b"must-not-download".to_vec()),
        ]),
        downloads: Mutex::new(Vec::new()),
    };
    let paired_update = ManagedPairMode::Paired {
        install_root: fixture.path().to_path_buf(),
        repair_required: false,
    };
    assert_eq!(
        core_download_route(&plan, &paired_update),
        CoreDownloadRoute::ManagedPair
    );
    assert_eq!(
        core_download_route(&plan, &ManagedPairMode::CoreOnly),
        CoreDownloadRoute::Legacy
    );

    let mut downloads =
        ManagedPairDownloads::download(&transport, fixture.path(), &plan, &verifier)?;
    assert_eq!(
        transport.downloads.lock().unwrap().as_slice(),
        ["pair-core", "pair-companion"]
    );
    assert!(!transport
        .downloads
        .lock()
        .unwrap()
        .iter()
        .any(|endpoint| endpoint == "legacy-core"));
    let lock = InstallationLock::try_acquire_at_root(fixture.path())?.expect("pair lock");
    let outcome = downloads.apply_under_installation_lock(&lock, fixture.path(), &verifier)?;
    assert!(matches!(outcome, ManagedPairApplyOutcome::Applied { .. }));
    assert_eq!(fs::read(&core_path)?, next_core);
    assert!(resume_or_confirm_pending_with_verifier(
        &core_path,
        &core_sha,
        &digest(b"signed-envelope"),
        &lock,
        &verifier,
    )?);
    assert!(!resume_or_confirm_pending_with_verifier(
        &core_path,
        &core_sha,
        &digest(b"signed-envelope-with-new-companion"),
        &lock,
        &verifier,
    )?);
    let companion_name = if cfg!(windows) {
        "ctx-pro.exe"
    } else {
        "ctx-pro"
    };
    assert_eq!(
        fs::read(fixture.path().join("libexec").join(companion_name))?,
        companion
    );
    let marker: serde_json::Value = serde_json::from_slice(&fs::read(&marker_path)?)?;
    assert_eq!(marker["version"], "1.1.0");
    assert_eq!(marker["man_pages"]["status"], "installed");
    assert!(fixture
        .path()
        .join("share/ctx/managed-pair-state.json")
        .is_file());
    drop(lock);

    fs::write(
        fixture.path().join("libexec").join(companion_name),
        b"damaged",
    )?;
    let mut repair_plan = plan.clone();
    repair_plan.current_version = "1.1.0".to_owned();
    repair_plan.latest_version = "1.1.0".to_owned();
    repair_plan.update_available = false;
    repair_plan.install_fingerprint = InstallFingerprint {
        binary_sha256: digest(next_core),
        marker_sha256: digest(&fs::read(&marker_path)?),
    };
    let lock = InstallationLock::try_acquire_at_root(fixture.path())?.expect("repair lock");
    let repair_mode = inspect_plan_under_installation_lock(&repair_plan, &lock)?;
    assert!(repair_mode.pair_apply_required(&repair_plan));
    assert_eq!(
        core_download_route(&repair_plan, &repair_mode),
        CoreDownloadRoute::ManagedPair
    );
    let mut repair =
        ManagedPairDownloads::download(&transport, fixture.path(), &repair_plan, &verifier)?;
    let staged =
        stage_managed_pair_under_installation_lock(fixture.path(), &repair.input()?, &verifier)?;
    assert!(matches!(staged, ManagedPairStageOutcome::Staged { .. }));
    let mut requested_plan = repair_plan.clone();
    requested_plan.latest_version = "1.2.0".to_owned();
    requested_plan.artifact_sha256 = requested_core_sha.clone();
    requested_plan.update_available = true;
    requested_plan.managed_pair_release = Some(ManagedPairReleaseMetadata {
        envelope_url: "requested-envelope".to_owned(),
        core_object_url: "requested-core".to_owned(),
        core_sha256: requested_core_sha,
        companion_object_url: "requested-companion".to_owned(),
        companion_sha256: requested_companion_sha,
    });
    requested_plan.metadata.version = "1.2.0".to_owned();
    requested_plan.metadata.sha256 = requested_plan.artifact_sha256.clone();
    let mut requested =
        ManagedPairDownloads::download(&transport, fixture.path(), &requested_plan, &verifier)?;
    let error = requested
        .apply_under_installation_lock(&lock, fixture.path(), &verifier)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("did not apply the requested signed candidate"),
        "{error:#}"
    );
    assert_eq!(fs::read(&core_path)?, next_core);
    assert!(!resume_or_confirm_pending_with_verifier(
        &core_path,
        &core_sha,
        &digest(b"signed-envelope-with-new-companion"),
        &lock,
        &verifier,
    )?);
    assert_eq!(
        fs::read(fixture.path().join("libexec").join(companion_name))?,
        companion
    );
    Ok(())
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
    let lock = UpgradeLock::from_installation_for_test(install_path.clone(), installation);
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
