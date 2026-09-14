use ctx_history_platform::platform_security::{
    create_private_directory_all, restrict_private_file,
};
use ctx_managed_pair_engine::{
    resume_pending_managed_pair_under_installation_lock,
    stage_managed_pair_under_installation_lock, ManagedPairApplyInput,
    ManagedPairComponentIdentity, ManagedPairStageOutcome, ManagedPairTarget, ManagedPairVerifier,
    VerifiedManagedPairIdentity, MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH,
};
use sha2::{Digest as _, Sha256};

use super::super::{write_state_error_locked, STATE_SCHEMA_VERSION};
use super::*;

struct RecoveryFixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    data_root: PathBuf,
    install_path: PathBuf,
    identity: VerifiedManagedPairIdentity,
}

impl ManagedPairVerifier for RecoveryFixture {
    fn verify_signed_envelope(&self, bytes: &[u8]) -> Result<VerifiedManagedPairIdentity> {
        if bytes != b"signed-recovery-envelope" {
            return Err(anyhow!("fixture envelope rejected"));
        }
        Ok(self.identity.clone())
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

impl RecoveryFixture {
    fn new() -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let root = fs::canonicalize(temp.path())?.join("install");
        let data_root = root.join("origin");
        create_private_directory_all(&root.join("bin"))?;
        create_private_directory_all(&data_root)?;
        let install_path = root.join(if cfg!(windows) {
            "bin/ctx.exe"
        } else {
            "bin/ctx"
        });
        fs::write(&install_path, b"old-core")?;
        restrict_private_file(&install_path)?;
        let target = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("linux", "aarch64") => ManagedPairTarget::LinuxArm64,
            ("linux", "x86_64") => ManagedPairTarget::LinuxX64,
            ("macos", "aarch64") => ManagedPairTarget::MacosArm64,
            ("macos", "x86_64") => ManagedPairTarget::MacosX64,
            ("windows", "x86_64") => ManagedPairTarget::WindowsX64,
            target => return Err(anyhow!("unsupported fixture target {target:?}")),
        };
        Ok(Self {
            _temp: temp,
            root,
            data_root,
            install_path,
            identity: VerifiedManagedPairIdentity::new(
                "recovery",
                target,
                1,
                digest(b"manifest"),
                ManagedPairComponentIdentity::new(digest(b"new-core"), 8)?,
                ManagedPairComponentIdentity::new(digest(b"new-companion"), 13)?,
            )?,
        })
    }

    fn lock(&self) -> Result<UpgradeLock> {
        let installation = InstallationLock::try_acquire_at_root(&self.root)?
            .ok_or_else(|| anyhow!("fixture installation lock held"))?;
        Ok(UpgradeLock::from_installation_for_test(
            self.install_path.clone(),
            installation,
        ))
    }

    fn stage_failed_attempt(&self, lock: &UpgradeLock) -> Result<(UpgradeAttempt, String)> {
        let inputs = self.root.join("inputs");
        create_private_directory_all(&inputs)?;
        for (name, bytes) in [
            ("envelope", b"signed-recovery-envelope".as_slice()),
            ("core", b"new-core".as_slice()),
            ("companion", b"new-companion".as_slice()),
            ("marker", b"managed-marker".as_slice()),
        ] {
            fs::write(inputs.join(name), bytes)?;
            restrict_private_file(&inputs.join(name))?;
        }
        let staged = stage_managed_pair_under_installation_lock(
            &self.root,
            &ManagedPairApplyInput::new(
                inputs.join("envelope"),
                inputs.join("core"),
                inputs.join("companion"),
                inputs.join("marker"),
            ),
            self,
        )?;
        let ManagedPairStageOutcome::Staged {
            attempt_id: pair_attempt,
            ..
        } = staged
        else {
            return Err(anyhow!("fixture candidate was not staged"));
        };
        let attempt = UpgradeAttempt {
            id: "ua_pending_pair_recovery".to_owned(),
        };
        let state = UpgradeState {
            schema_version: STATE_SCHEMA_VERSION,
            status: "applying".to_owned(),
            attempt_id: Some(attempt.id().to_owned()),
            attempt_source: Some("automatic".to_owned()),
            plan: json!({
                "managed_pair_apply": true,
                "managed_pair_data_root": self.data_root,
                "install_path": self.install_path,
                "channel": "stable",
                "managed_pair_interval_seconds": 60,
                "managed_pair_core_sha256": digest(b"new-core"),
                "managed_pair_envelope_sha256": digest(b"signed-recovery-envelope"),
                "managed_pair_restart_trigger": "automatic_upgrade",
                "managed_pair_restart_interval_seconds": 60
            })
            .as_object()
            .unwrap()
            .clone(),
            ..UpgradeState::default()
        };
        write_state_object_locked(lock, state)?;
        assert!(write_state_error_locked(
            &self.data_root,
            lock,
            &attempt,
            "apply",
            "publication interrupted"
        )?);
        Ok((attempt, pair_attempt))
    }
}

// The child only reads the production hint. A separate process isolates the
// existing test target override from concurrently running unit tests.
#[test]
fn recovery_hint_probe() {
    let Ok(expected) = std::env::var("CTX_PAIR_RECOVERY_EXPECTED_HINT") else {
        return;
    };
    if expected == "error" {
        assert!(recovery_hint().is_err());
        return;
    }
    let actual = recovery_hint().unwrap();
    assert_eq!(
        actual.as_deref(),
        (expected != "none").then_some(expected.as_str())
    );
}

fn assert_hint(install_path: &Path, expected: Option<&str>) -> Result<()> {
    let output = std::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "upgrade::state::managed_pair::tests::recovery_hint_probe",
            "--nocapture",
        ])
        .env("CTX_UPGRADE_TEST_TARGET", install_path)
        .env(
            "CTX_PAIR_RECOVERY_EXPECTED_HINT",
            expected.unwrap_or("none"),
        )
        .output()?;
    assert!(
        output.status.success(),
        "hint probe failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "probe must actually execute"
    );
    Ok(())
}

#[test]
fn locked_recovery_rejects_scheduler_replacement_and_removed_or_generic_pending() -> Result<()> {
    for mutation in [
        "attempt",
        "install",
        "origin",
        "core_hash",
        "envelope_hash",
        "removed",
        "generic",
    ] {
        let fixture = RecoveryFixture::new()?;
        let lock = fixture.lock()?;
        let (attempt, _) = fixture.stage_failed_attempt(&lock)?;
        drop(lock);
        assert_hint(&fixture.install_path, Some(attempt.id()))?;
        let lock = fixture.lock()?;
        let mut state = read_state_object(&fixture.install_path);
        let pending = fixture
            .root
            .join(MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH);
        match mutation {
            "attempt" => state.attempt_id = Some("ua_replacement".to_owned()),
            "install" => {
                state.plan.insert(
                    "install_path".to_owned(),
                    json!(fixture.root.join("other/ctx")),
                );
            }
            "origin" => {
                state.plan.insert(
                    DATA_ROOT_KEY.to_owned(),
                    json!(fixture.data_root.join("..")),
                );
            }
            "core_hash" => {
                state
                    .plan
                    .insert(CORE_SHA256_KEY.to_owned(), json!("invalid"));
            }
            "envelope_hash" => {
                state
                    .plan
                    .insert(ENVELOPE_SHA256_KEY.to_owned(), json!("invalid"));
            }
            "removed" => fs::remove_file(&pending)?,
            "generic" => fs::write(
                &pending,
                br#"{"schema_version":2,"kind":"generic-upgrade"}"#,
            )?,
            _ => unreachable!(),
        }
        write_state_object_locked(&lock, state)?;
        assert!(recovery_locked(&lock, attempt.id()).is_err(), "{mutation}");
        assert_eq!(fs::read(&fixture.install_path)?, b"old-core");
        if matches!(mutation, "removed" | "generic") {
            assert_hint(&fixture.install_path, None)?;
        }
    }
    Ok(())
}

#[test]
fn recovery_rejects_corrupt_retained_components_after_scheduler_error() -> Result<()> {
    let fixture = RecoveryFixture::new()?;
    let lock = fixture.lock()?;
    let (attempt, _) = fixture.stage_failed_attempt(&lock)?;
    let recovery = recovery_locked(&lock, attempt.id())?;
    assert_eq!(recovery.core_sha256, digest(b"new-core"));
    let retained = fixture
        .root
        .join("share/ctx/.managed-pair-apply-v1")
        .join(if cfg!(windows) {
            "bin/ctx.exe"
        } else {
            "bin/ctx"
        });
    fs::write(retained, b"corrupt-core")?;
    assert!(resume_pending_managed_pair_under_installation_lock(&fixture.root, &fixture).is_err());
    assert_eq!(fs::read(&fixture.install_path)?, b"old-core");
    assert!(fixture
        .root
        .join(MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH)
        .is_file());
    Ok(())
}

#[cfg(unix)]
mod dispatch;

#[test]
fn malformed_and_bounded_pair_hints_preserve_the_pending_record() -> Result<()> {
    for bytes in [
        br#"{"schema":"ctx-managed-pair-apply-v1"}"#.to_vec(),
        vec![b'x'; 16 * 1024 + 1],
    ] {
        let fixture = RecoveryFixture::new()?;
        let lock = fixture.lock()?;
        fixture.stage_failed_attempt(&lock)?;
        let pending = fixture
            .root
            .join(MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH);
        fs::write(&pending, &bytes)?;
        assert_hint(&fixture.install_path, Some("error"))?;
        assert_eq!(fs::read(pending)?, bytes);
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn unsafe_pair_hint_preserves_the_link_and_its_target() -> Result<()> {
    let fixture = RecoveryFixture::new()?;
    let lock = fixture.lock()?;
    fixture.stage_failed_attempt(&lock)?;
    let pending = fixture
        .root
        .join(MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH);
    let target = pending.with_extension("saved");
    fs::rename(&pending, &target)?;
    let original = fs::read(&target)?;
    std::os::unix::fs::symlink(&target, &pending)?;
    assert_hint(&fixture.install_path, Some("error"))?;
    assert_eq!(fs::read_link(pending)?, target);
    assert_eq!(fs::read(target)?, original);
    Ok(())
}
