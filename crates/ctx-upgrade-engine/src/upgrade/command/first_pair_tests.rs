//! Opaque pair fixtures exercise the ordinary owner after plan authentication.
//! Signature negatives below use the real production verifiers, without the
//! fixture verifier. These are orchestration tests, not signed release proof.
use super::*;
use crate::upgrade::{
    install::{install_marker_path, InstallationLock},
    managed_pair::{tests::with_fixture_verifier, ReleaseManagedPairVerifier},
    sha256_hex, ProductBuildIdentity, ReleaseTransport, TEST_RELEASE_PROCESS, TEST_SEMANTIC_LAYOUT,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ctx_companion_bridge::ReleaseChannel;
use ctx_history_platform::platform_security::{
    create_private_directory_all, restrict_private_file,
};
use ctx_managed_pair_engine::{
    ManagedPairComponentIdentity, ManagedPairTarget, ManagedPairVerifier,
    VerifiedManagedPairIdentity,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::Write as _,
    os::unix::fs::PermissionsExt as _,
    sync::{Arc, Mutex},
};

const CORE: &[u8] = b"installed-core";
const NEXT_CORE: &[u8] = b"paired-core";
const COMPANION: &[u8] = b"paired-companion";
const ENVELOPE: &[u8] = b"opaque-fixture-envelope";

pub(super) struct Fixture {
    _temp: tempfile::TempDir,
    pub(super) root: PathBuf,
    pub(super) data: PathBuf,
    pub(super) plan: UpgradePlan,
    pub(super) metadata: Vec<u8>,
    pub(super) transport: Transport,
    pub(super) trace: Arc<Trace>,
    pub(super) identity: VerifiedManagedPairIdentity,
}

pub(super) struct Transport {
    pub(super) bytes: BTreeMap<String, Vec<u8>>,
    log: Arc<Mutex<Vec<String>>>,
}
impl ReleaseTransport for Transport {
    fn get_bytes_limited(&self, endpoint: &str, max: usize) -> Result<Vec<u8>> {
        self.log.lock().unwrap().push(endpoint.to_owned());
        let bytes = self
            .bytes
            .get(endpoint)
            .ok_or_else(|| anyhow!("unexpected request {endpoint}"))?;
        assert!(bytes.len() <= max);
        Ok(bytes.clone())
    }
    fn download_artifact(
        &self,
        endpoint: &str,
        destination: &mut fs::File,
        max: u64,
        _: Duration,
    ) -> Result<u64> {
        let bytes = self.get_bytes_limited(endpoint, max as usize)?;
        destination.write_all(&bytes)?;
        Ok(bytes.len() as u64)
    }
}

pub(super) struct Trace {
    root: PathBuf,
    core: PathBuf,
    data: PathBuf,
    case: String,
    expected_core: Vec<u8>,
    pub(super) calls: Mutex<Vec<&'static str>>,
    downloads: Arc<Mutex<Vec<String>>>,
}
impl Trace {
    fn unchanged(&self) -> Result<()> {
        assert_eq!(fs::read(&self.core)?, b"installed-core");
        assert!(!self.root.join("libexec/ctx-pro").exists());
        assert!(!self.root.join("share/ctx/managed-pair-state.json").exists());
        Ok(())
    }
}
impl DaemonUpgradeLease for Arc<Trace> {
    fn wait_for_installation_quiescence(&self) -> Result<()> {
        Ok(())
    }
    fn replacement_restart(&self) -> Option<crate::DaemonRestart<'_>> {
        Some(crate::DaemonRestart {
            trigger: "automatic_upgrade",
            loop_interval_seconds: Some(60),
        })
    }
    fn resume_with(self, executable: &Path) -> Result<()> {
        assert_eq!(executable, self.core);
        if self.case == "partial_pair" {
            assert_eq!(fs::read(&self.core)?, b"installed-core");
        } else if self.case == "stale_at_handoff" || self.case == "automatic_disabled" {
            self.unchanged()?;
        } else {
            assert_eq!(fs::read(&self.core)?, self.expected_core);
            assert_eq!(fs::read(self.root.join("libexec/ctx-pro"))?, COMPANION);
            assert_eq!(
                fs::read(self.root.join("share/ctx/managed-pair-envelope.json"))?,
                ENVELOPE
            );
            let marker: Value =
                serde_json::from_slice(&fs::read(install_marker_path(&self.core))?)?;
            assert_eq!(marker["managed_pair"], true);
            assert_eq!(
                crate::upgrade::read_state_json().unwrap()["status"],
                "applied"
            );
            assert!(!self
                .root
                .join(ctx_managed_pair_engine::MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH)
                .exists());
        }
        self.calls.lock().unwrap().push("resume");
        if self.case == "restart_failure" {
            return Err(anyhow!("injected restart failure"));
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
        if self.case == "partial_pair" {
            assert_eq!(fs::read(&self.core)?, b"installed-core");
        } else {
            self.unchanged()?;
        }
        assert!(InstallationLock::try_acquire_at_root(&self.root)?.is_none());
        let state = crate::upgrade::read_state_json().unwrap();
        assert_eq!(
            state["status"],
            if self.case.starts_with("automatic") {
                "staged"
            } else {
                "quiescing"
            }
        );
        assert_eq!(
            self.downloads.lock().unwrap().len(),
            3,
            "all signed pair inputs must be staged before handoff"
        );
        assert!(!self
            .root
            .join(ctx_managed_pair_engine::MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH)
            .exists());
        self.calls.lock().unwrap().push("begin");
        if self.case == "handoff_failure" {
            return Err(anyhow!("injected all-root handoff failure"));
        }
        if self.case == "stale_at_handoff" {
            let path = install_marker_path(&self.core);
            let mut bytes = fs::read(&path)?;
            bytes.push(b' ');
            fs::write(path, bytes)?;
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
        let bin = root.join(if case == "custom_geometry" {
            "custom"
        } else {
            "bin"
        });
        create_private_directory_all(&bin)?;
        create_private_directory_all(&data)?;
        // Only an isolated --exact child enters this fixture.
        for key in [
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "XDG_RUNTIME_DIR",
            "CTX_DATA_ROOT",
        ] {
            let path = temp.path().join(key);
            create_private_directory_all(&path)?;
            std::env::set_var(key, path);
        }
        let core = bin.join("ctx");
        fs::write(&core, b"installed-core")?;
        fs::set_permissions(&core, fs::Permissions::from_mode(0o700))?;
        std::env::set_var("CTX_UPGRADE_TEST_TARGET", &core);
        let marker_path = install_marker_path(&core);
        let platform = platform_key()?;
        let marker = json!({"schema_version":1,"manager":"ctx-hosted-installer","install_path":core,
            "platform":platform,"channel":"stable","version":"1.3.2","sha256":sha256_hex(b"installed-core")});
        fs::write(&marker_path, serde_json::to_vec(&marker)?)?;
        restrict_private_file(&marker_path)?;
        let latest = if matches!(case, "newer" | "automatic_newer") {
            "1.3.3"
        } else {
            "1.3.2"
        };
        let candidate_core = if latest == "1.3.3" { NEXT_CORE } else { CORE };
        let core_sha = sha256_hex(candidate_core);
        let companion_sha = sha256_hex(COMPANION);
        let base =
            format!("https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/{latest}");
        let p = platform.replace('-', "_");
        let metadata = format!("CTX_RELEASE_SCHEMA_VERSION=1\nCTX_RELEASE_CHANNEL=stable\nCTX_RELEASE_SELF_UPGRADE_ALLOWED=true\nCTX_RELEASE_AUTO_UPGRADE_ALLOWED=true\nCTX_RELEASE_VERSION={latest}\nCTX_RELEASE_BASE_URL={base}\nCTX_RELEASE_ARTIFACT_{p}=ctx\nCTX_RELEASE_SHA256_{p}={core_sha}\nCTX_RELEASE_MANAGED_PAIR_ENVELOPE_{p}=pair.json\nCTX_RELEASE_MANAGED_PAIR_CORE_OBJECT_{p}=sha256/{core_sha}/ctx\nCTX_RELEASE_MANAGED_PAIR_CORE_SHA256_{p}={core_sha}\nCTX_RELEASE_MANAGED_PAIR_COMPANION_OBJECT_{p}=sha256/{companion_sha}/ctx-pro\nCTX_RELEASE_MANAGED_PAIR_COMPANION_SHA256_{p}={companion_sha}\n").into_bytes();
        let parsed =
            parse_release_metadata(&metadata, platform, "stable", false, &TEST_SEMANTIC_LAYOUT)?;
        let pair =
            project_managed_pair_release(&parsed.base_url, parsed.managed_pair.as_ref())?.unwrap();
        let mut warnings = vec![];
        let snapshot = capture_install_snapshot(true, platform, "stable", "1.3.2", &mut warnings)?;
        let plan = UpgradePlan {
            current_version: snapshot.marker.version,
            latest_version: latest.to_owned(),
            channel: "stable".to_owned(),
            platform: platform.to_owned(),
            metadata_url: metadata_url("stable"),
            artifact_url: format!("{base}/ctx"),
            artifact_sha256: core_sha.clone(),
            install_path: core.clone(),
            install_fingerprint: snapshot.fingerprint,
            update_available: version_gt(latest, "1.3.2"),
            managed: warnings.is_empty(),
            warnings,
            managed_pair_release: Some(pair.clone()),
            metadata: parsed,
            semantic_provisioning: None,
        };
        let target = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("linux", "x86_64") => ManagedPairTarget::LinuxX64,
            ("linux", "aarch64") => ManagedPairTarget::LinuxArm64,
            ("macos", "x86_64") => ManagedPairTarget::MacosX64,
            ("macos", "aarch64") => ManagedPairTarget::MacosArm64,
            other => return Err(anyhow!("unsupported fixture platform {other:?}")),
        };
        let identity = VerifiedManagedPairIdentity::new(
            format!("ctx-{latest}"),
            target,
            1,
            sha256_hex(b"manifest"),
            ManagedPairComponentIdentity::new(core_sha, candidate_core.len() as u64)?,
            ManagedPairComponentIdentity::new(companion_sha, COMPANION.len() as u64)?,
        )?;
        let log = Arc::new(Mutex::new(vec![]));
        let transport = Transport {
            bytes: BTreeMap::from([
                (pair.envelope_url, ENVELOPE.to_vec()),
                (pair.core_object_url, candidate_core.to_vec()),
                (pair.companion_object_url, COMPANION.to_vec()),
            ]),
            log: log.clone(),
        };
        let trace = Arc::new(Trace {
            root: root.clone(),
            core,
            data: data.clone(),
            case: case.to_owned(),
            expected_core: candidate_core.to_vec(),
            calls: Mutex::new(vec![]),
            downloads: log,
        });
        Ok(Self {
            _temp: temp,
            root,
            data,
            plan,
            metadata,
            transport,
            trace,
            identity,
        })
    }
    pub(super) fn requests(&self) -> Vec<String> {
        self.transport.log.lock().unwrap().clone()
    }
    pub(super) fn engine(&self) -> UpgradeEngine<'_, Arc<Trace>> {
        UpgradeEngine::new(
            ProductBuildIdentity::new("1.3.2"),
            &self.transport,
            &TEST_RELEASE_PROCESS,
            &TEST_SEMANTIC_LAYOUT,
            &self.trace,
        )
    }
    pub(super) fn verified<T>(&self, operation: impl FnOnce() -> T) -> T {
        with_fixture_verifier(
            ReleaseChannel::Stable,
            ENVELOPE.to_vec(),
            self.identity.clone(),
            operation,
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
        .env("CTX_FIRST_PAIR_CASE", case)
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
fn first_pair_foreground_owner_and_admission() -> Result<()> {
    for case in [
        "same",
        "newer",
        "dry_run",
        "source",
        "custom_geometry",
        "no_pair_metadata",
        "older",
        "build_identity",
        "invalid_version",
        "disallowed",
        "marker_changed",
        "binary_changed",
        "wrong_target",
        "invalid_manager",
        "wrong_platform",
        "unsafe_marker",
        "nonexecutable",
        "stale_at_handoff",
        "handoff_failure",
        "restart_failure",
        "malformed_envelope",
        "bad_signature",
        "metadata_signature",
        "partial_metadata",
        "partial_pair",
    ] {
        child(case, "upgrade::command::first_pair_tests::first_pair_probe")?;
    }
    Ok(())
}

#[test]
fn first_pair_probe() -> Result<()> {
    let Ok(case) = std::env::var("CTX_FIRST_PAIR_CASE") else {
        return Ok(());
    };
    let mut f = Fixture::new(&case)?;
    let marker = install_marker_path(&f.plan.install_path);
    match case.as_str() {
        "source" => {
            fs::remove_file(&marker)?;
            let mut warnings = vec![];
            capture_install_snapshot(false, &f.plan.platform, "stable", "1.3.2", &mut warnings)?;
            assert!(!warnings.is_empty());
            f.plan.managed = warnings.is_empty();
            let lock = InstallationLock::try_acquire_at_root(&f.root)?.unwrap();
            assert_eq!(
                inspect_plan_under_installation_lock(&f.plan, &lock)?,
                ManagedPairMode::CoreOnly
            );
            drop(lock);
            assert!(format!(
                "{:#}",
                f.engine()
                    .apply(&f.data, policy(), None, false)
                    .unwrap_err()
            )
            .contains("not installed by the hosted installer"));
        }
        "custom_geometry" | "no_pair_metadata" => {
            if case == "no_pair_metadata" {
                f.plan.managed_pair_release = None;
            }
            assert_eq!(f.verified(|| f.apply(false))?.status(), "up_to_date");
        }
        "metadata_signature" => {
            f.transport
                .bytes
                .insert(f.plan.metadata_url.clone(), f.metadata.clone());
            f.transport.bytes.insert(
                metadata_signature_url(&f.plan.metadata_url),
                BASE64.encode([0u8; 384]).into_bytes(),
            );
            assert!(format!(
                "{:#}",
                f.engine()
                    .apply(&f.data, policy(), None, false)
                    .unwrap_err()
            )
            .contains("metadata signature verification failed"));
        }
        "partial_metadata" => {
            let text = String::from_utf8(f.metadata.clone())?;
            let partial = text
                .lines()
                .filter(|line| !line.starts_with("CTX_RELEASE_MANAGED_PAIR_COMPANION_SHA256_"))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(format!(
                "{:#}",
                parse_release_metadata(
                    partial.as_bytes(),
                    &f.plan.platform,
                    "stable",
                    false,
                    &TEST_SEMANTIC_LAYOUT
                )
                .unwrap_err()
            )
            .contains("is partial"));
        }
        "malformed_envelope" | "bad_signature" => {
            let envelope = if case == "bad_signature" {
                invalid_signature_envelope()?
            } else {
                b"not-json".to_vec()
            };
            let expected = if case == "bad_signature" {
                "detached manifest signature is invalid"
            } else {
                "detached envelope"
            };
            assert!(format!(
                "{:#}",
                ReleaseManagedPairVerifier::for_channel("stable")?
                    .verify_signed_envelope(&envelope)
                    .unwrap_err()
            )
            .contains(expected));
            f.transport.bytes.insert(
                f.plan
                    .managed_pair_release
                    .as_ref()
                    .unwrap()
                    .envelope_url
                    .clone(),
                envelope,
            );
            assert!(format!("{:#}", f.apply(false).unwrap_err()).contains(expected));
        }
        "older" | "build_identity" | "invalid_version" | "disallowed" | "marker_changed"
        | "binary_changed" | "wrong_target" | "invalid_manager" | "wrong_platform"
        | "unsafe_marker" | "nonexecutable" => {
            match case.as_str() {
                "older" => f.plan.latest_version = "1.3.1".to_owned(),
                "build_identity" => f.plan.latest_version = "1.3.2+different".to_owned(),
                "invalid_version" => f.plan.latest_version = "invalid".to_owned(),
                "disallowed" => f.plan.metadata.self_upgrade_allowed = false,
                "marker_changed" => {
                    let mut bytes = fs::read(&marker)?;
                    bytes.push(b' ');
                    fs::write(&marker, bytes)?;
                }
                "binary_changed" => fs::write(&f.plan.install_path, b"different-core")?,
                "wrong_target" => {
                    std::env::set_var("CTX_UPGRADE_TEST_TARGET", std::env::current_exe()?)
                }
                "invalid_manager" | "wrong_platform" => {
                    let mut value: Value = serde_json::from_slice(&fs::read(&marker)?)?;
                    value[if case == "invalid_manager" {
                        "manager"
                    } else {
                        "platform"
                    }] = json!("other");
                    fs::write(&marker, serde_json::to_vec(&value)?)?;
                }
                "unsafe_marker" => fs::set_permissions(&marker, fs::Permissions::from_mode(0o666))?,
                "nonexecutable" => {
                    fs::set_permissions(&f.plan.install_path, fs::Permissions::from_mode(0o600))?
                }
                _ => unreachable!(),
            }
            assert!(f.verified(|| f.apply(false)).is_err(), "{case}");
            assert!(f.transport.log.lock().unwrap().is_empty());
        }
        "partial_pair" => {
            create_private_directory_all(&f.root.join("libexec"))?;
            let companion = f.root.join("libexec/ctx-pro");
            fs::write(&companion, b"orphan-companion")?;
            fs::set_permissions(&companion, fs::Permissions::from_mode(0o700))?;
            let result = f.verified(|| f.apply(false));
            assert!(format!("{:#}", result.unwrap_err())
                .contains("no valid rollback-generation witness"));
            assert_eq!(fs::read(companion)?, b"orphan-companion");
            return Ok(());
        }
        _ => {
            let result = f.verified(|| f.apply(case == "dry_run"));
            match case.as_str() {
                "handoff_failure" => {
                    assert!(
                        format!("{:#}", result.unwrap_err()).contains("all-root handoff failure")
                    );
                    assert_eq!(*f.trace.calls.lock().unwrap(), ["begin"]);
                }
                "stale_at_handoff" => {
                    assert!(format!("{:#}", result.unwrap_err())
                        .contains("changed after this upgrade plan"));
                    assert_eq!(*f.trace.calls.lock().unwrap(), ["begin", "resume"]);
                }
                "dry_run" => {
                    assert_eq!(result?.status(), "dry_run");
                    assert!(f.transport.log.lock().unwrap().is_empty());
                }
                _ => {
                    let outcome = result?;
                    assert!(outcome.applied());
                    assert_eq!(
                        outcome.plan().unwrap().latest_version(),
                        f.plan.latest_version
                    );
                    assert_eq!(*f.trace.calls.lock().unwrap(), ["begin", "resume"]);
                    if case == "restart_failure" {
                        assert!(outcome
                            .warnings()
                            .iter()
                            .any(|w| w.contains("restart is pending")));
                    }
                    return Ok(());
                }
            }
        }
    }
    if case != "binary_changed" {
        f.trace.unchanged()?;
    }
    if !matches!(case.as_str(), "handoff_failure" | "stale_at_handoff") {
        assert!(f.trace.calls.lock().unwrap().is_empty());
    }
    Ok(())
}

fn invalid_signature_envelope() -> Result<Vec<u8>> {
    let build = json!({"component":"core","rust_target":"target","source_revision":"revision","build_fingerprint":"fingerprint"});
    let component = json!({"artifact_name":"ctx","object_key":"object","sha256":sha256_hex(CORE),"size_bytes":CORE.len(),"install_slot":"bin/ctx","build_identity":build});
    let manifest = json!({"contract":"ctx-managed-pair-manifest","schema_version":1,"channel":"stable",
        "release_authority_key_id":"ctx-pro-release-stable-2026-07-27","release_name":"ctx-1.3.2",
        "target":{"id":"linux-x64","os":"linux","arch":"x64","core_rust_target":"target","companion_rust_target":"target"},
        "install_geometry":{"install_root":"root","managed_bin_dir":"bin","core_slot":"bin/ctx","companion_slot":"libexec/ctx-pro"},
        "target_matrix_sha256":sha256_hex(b"matrix"),"rollback_generation":1,"snapshot":{"contract":"snapshot","fingerprint":"fingerprint"},
        "compatibility":{"invocation_fingerprint":"fingerprint","core_capability_fingerprint":"fingerprint"},
        "components":{"core":component,"companion":component}});
    Ok(serde_json::to_vec(
        &json!({"schema_version":1,"manifest_base64":BASE64.encode(serde_json::to_vec(&manifest)?),"signature_base64":BASE64.encode([0u8;384])}),
    )?)
}
