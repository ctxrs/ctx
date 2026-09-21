#![cfg(unix)]
use super::legacy_pair::cleanup_legacy_managed_pair_under_installation_lock;
use super::{install_marker_path, InstallationLock};
use crate::upgrade::platform_key;
use crate::upgrade::sha256_hex;
use anyhow::Result;
use ctx_history_platform::platform_security::{
    create_private_directory_all, restrict_private_file,
};
use ctx_managed_pair_engine::{
    MANAGED_PAIR_ENVELOPE_RELATIVE_PATH, MANAGED_PAIR_STATE_RELATIVE_PATH,
};
use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt as _, path::PathBuf};

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    core: PathBuf,
}
impl Fixture {
    fn new(version: &str) -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let root = fs::canonicalize(temp.path())?.join("install");
        for path in ["bin", "libexec", "share/ctx"] {
            create_private_directory_all(&root.join(path))?;
        }
        let core = root.join("bin/ctx");
        fs::write(&core, b"unified")?;
        fs::set_permissions(&core, fs::Permissions::from_mode(0o700))?;
        let marker = json!({"schema_version":1,"manager":"ctx-hosted-installer","version":version,
            "install_path":core,"platform":platform_key()?,"channel":"stable","sha256":sha256_hex(b"unified"),
            "managed_pair":true,"man_pages":{"keep":"untouched"},"install_attempt_id":"ia_preserved_receipt"});
        fs::write(install_marker_path(&core), serde_json::to_vec(&marker)?)?;
        restrict_private_file(&install_marker_path(&core))?;
        for path in [
            "libexec/ctx-pro",
            MANAGED_PAIR_STATE_RELATIVE_PATH,
            MANAGED_PAIR_ENVELOPE_RELATIVE_PATH,
        ] {
            fs::write(root.join(path), b"retired installation file")?;
            restrict_private_file(&root.join(path))?;
        }
        Ok(Self {
            _temp: temp,
            root,
            core,
        })
    }
    fn cleanup(&self) -> Result<()> {
        let _lock = InstallationLock::try_acquire_at_root(&self.root)?.unwrap();
        cleanup_legacy_managed_pair_under_installation_lock(&self.core)
    }
    fn marker(&self) -> Value {
        serde_json::from_slice(&fs::read(install_marker_path(&self.core)).unwrap()).unwrap()
    }
}

#[test]
fn completed_unified_install_loses_only_legacy_classification_and_slots() -> Result<()> {
    let f = Fixture::new("1.5.0")?;
    f.cleanup()?;
    f.cleanup()?;
    assert!(f.marker().get("managed_pair").is_none());
    assert_eq!(f.marker()["man_pages"]["keep"], "untouched");
    assert_eq!(f.marker()["install_attempt_id"], "ia_preserved_receipt");
    assert_eq!(fs::read(&f.core)?, b"unified");
    assert!(!f.root.join("libexec/ctx-pro").exists());
    Ok(())
}

#[test]
fn older_candidate_and_unmanaged_install_keep_their_files() -> Result<()> {
    let f = Fixture::new("1.4.12")?;
    f.cleanup()?;
    assert_eq!(f.marker()["managed_pair"], true);
    fs::remove_file(install_marker_path(&f.core))?;
    f.cleanup()?;
    assert!(f.root.join("libexec/ctx-pro").exists());
    Ok(())
}

#[test]
fn active_or_corrupt_scheduler_blocks_retirement_then_terminal_retry_succeeds() -> Result<()> {
    for state in [
        json!({"schema_version":1,"status":"scheduled"}),
        json!({"schema_version":999,"status":"applied"}),
        json!("corrupt"),
    ] {
        let f = Fixture::new("1.5.0")?;
        let path = f.root.join("bin/.ctx.upgrade-state.json");
        fs::write(&path, serde_json::to_vec(&state)?)?;
        restrict_private_file(&path)?;
        assert!(f.cleanup().is_err());
        assert!(f.root.join("libexec/ctx-pro").exists());
        assert_eq!(f.marker()["managed_pair"], true);
        fs::write(path, br#"{"schema_version":1,"status":"applied"}"#)?;
        f.cleanup()?;
        assert!(f.marker().get("managed_pair").is_none());
    }
    Ok(())
}

#[test]
fn pending_kernel_transaction_or_binary_tampering_never_loses_pair_material() -> Result<()> {
    let f = Fixture::new("1.5.0")?;
    let pending = f
        .root
        .join(ctx_managed_pair_engine::MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH);
    fs::write(&pending, b"pending old transaction")?;
    restrict_private_file(&pending)?;
    assert!(f.cleanup().is_err());
    assert_eq!(f.marker()["managed_pair"], true);
    fs::remove_file(pending)?;
    fs::write(&f.core, b"tampered")?;
    assert!(f.cleanup().is_err());
    assert!(f.root.join("libexec/ctx-pro").exists());
    Ok(())
}

#[test]
fn ordinary_marker_does_not_authorize_deleting_a_coincidentally_named_executable() -> Result<()> {
    let f = Fixture::new("1.5.0")?;
    let mut marker = f.marker();
    marker.as_object_mut().unwrap().remove("managed_pair");
    fs::write(install_marker_path(&f.core), serde_json::to_vec(&marker)?)?;
    fs::remove_file(f.root.join(MANAGED_PAIR_STATE_RELATIVE_PATH))?;
    fs::remove_file(f.root.join(MANAGED_PAIR_ENVELOPE_RELATIVE_PATH))?;
    f.cleanup()?;
    assert!(f.root.join("libexec/ctx-pro").exists());
    Ok(())
}

fn record_terminal_scheduler(root: &std::path::Path, data: &std::path::Path) -> Result<()> {
    let scheduler = root.join("bin/.ctx.upgrade-state.json");
    fs::write(
        &scheduler,
        serde_json::to_vec(&json!({"schema_version":1,"status":"applied",
        "attempt_id":"legacy-publication", "managed_pair_apply":true,"managed_pair_data_root":data}))?,
    )?;
    restrict_private_file(&scheduler)?;
    Ok(())
}

#[test]
fn abandoned_restart_owner_does_not_block_explicit_cleanup_retry() -> Result<()> {
    for phase in ["ready", "finalizing"] {
        let f = Fixture::new("1.5.0")?;
        let data = f._temp.path().join("data");
        create_private_directory_all(&data.join("daemon"))?;
        let output = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "upgrade::install::legacy_pair_tests::abandoned_owner_probe",
                "--nocapture",
            ])
            .env("CTX_RETIRED_PAIR_TEST_ROOT", &f.root)
            .env("CTX_RETIRED_PAIR_TEST_DATA", &data)
            .env("CTX_RETIRED_PAIR_TEST_PHASE", phase)
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        // The actual child wrote terminal state while owning the installation
        // lock, then exited without completing this handoff. No phase rewrite.
        let handoff = data.join("daemon/upgrade-handoff.json");
        let before = fs::read(&handoff)?;
        f.cleanup()?;
        f.cleanup()?;
        assert_eq!(fs::read(handoff)?, before);
        assert!(!f.root.join("libexec/ctx-pro").exists());
        assert!(f.marker().get("managed_pair").is_none());
    }
    Ok(())
}

#[test]
fn abandoned_owner_probe() -> Result<()> {
    let Some(root) = std::env::var_os("CTX_RETIRED_PAIR_TEST_ROOT").map(PathBuf::from) else {
        return Ok(());
    };
    let data = PathBuf::from(std::env::var_os("CTX_RETIRED_PAIR_TEST_DATA").unwrap());
    let _owner = InstallationLock::try_acquire_at_root(&root)?.unwrap();
    record_terminal_scheduler(&root, &data)?;
    let handoff = data.join("daemon/upgrade-handoff.json");
    fs::write(
        &handoff,
        serde_json::to_vec(
            &json!({"schema_version":1,"handoff_id":"legacy-publication",
        "phase":std::env::var("CTX_RETIRED_PAIR_TEST_PHASE")?,"owner_pid":std::process::id(),
        "helper_pid":std::process::id(),"updated_at_ms":ctx_history_core::utc_now().timestamp_millis()}),
        )?,
    )?;
    restrict_private_file(&handoff)?;
    Ok(())
}

#[test]
fn terminal_cleanup_leaves_lifecycle_records_to_their_owner() -> Result<()> {
    for phase in [
        None,
        Some("completed"),
        Some("aborted"),
        Some("ready"),
        Some("finalizing"),
    ] {
        let f = Fixture::new("1.5.0")?;
        let data = f._temp.path().join("data");
        create_private_directory_all(&data.join("daemon"))?;
        record_terminal_scheduler(&f.root, &data)?;
        let handoff = data.join("daemon/upgrade-handoff.json");
        if let Some(phase) = phase {
            fs::write(
                &handoff,
                serde_json::to_vec(&json!({"schema_version":1,
                "handoff_id":"unrelated-live-lifecycle", "phase":phase,
                "owner_pid":std::process::id(),"helper_pid":std::process::id()}))?,
            )?;
            restrict_private_file(&handoff)?;
        }
        let before = fs::read(&handoff).ok();
        // A live installation owner retains exclusive authority, irrespective
        // of lifecycle phase. After publication it releases this existing lock.
        let owner = InstallationLock::try_acquire_at_root(&f.root)?.unwrap();
        assert!(InstallationLock::try_acquire_at_root(&f.root)?.is_none());
        assert!(f.root.join("libexec/ctx-pro").exists());
        drop(owner);
        f.cleanup()?;
        assert_eq!(fs::read(&handoff).ok(), before);
        assert!(!f.root.join("libexec/ctx-pro").exists());
    }
    Ok(())
}
