//! Opt-in evidence for a rebuilt qualification executable, never stock 1.4 or
//! byte-identical platform-signed release evidence. No network or key generation.
use super::*;
use crate::upgrade::{install, sha256_hex, TEST_RELEASE_PROCESS};
use ctx_history_platform::platform_security::{
    create_private_directory_all, restrict_private_executable, restrict_private_file,
};
use ctx_managed_pair_engine::{
    stage_managed_pair_under_installation_lock, ManagedPairApplyInput, ManagedPairStageOutcome,
    MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH,
};
use serde_json::{json, Value};
use std::{
    fs,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    process::Command,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::WAIT_OBJECT_0,
    System::Threading::{
        GetExitCodeProcess, OpenProcess, TerminateProcess, WaitForSingleObject,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    },
};

const ACTOR: &str = "upgrade::managed_pair::native_helper_tests::native_helper_parent";
const ATTEMPT: &str = "ua_authored_legacy_qualification";
const OLD: &[u8] = b"authored pre-upgrade image; not a stock executable";

/// Supply a rebuilt Windows candidate, candidate-bound fixture envelope and
/// inline PUBLIC authority. The admission owner captures stdout as the receipt.
#[test]
#[ignore = "requires admitted native Windows and candidate-bound public-key fixtures"]
fn rebuilt_candidate_finishes_legacy_windows_transaction() -> Result<()> {
    let candidate = PathBuf::from(
        std::env::var_os("CTX_RELEASE_QUALIFICATION_CANDIDATE")
            .ok_or_else(|| anyhow!("missing rebuilt candidate path"))?,
    );
    let envelope = PathBuf::from(
        std::env::var_os("CTX_RELEASE_QUALIFICATION_ENVELOPE")
            .ok_or_else(|| anyhow!("missing candidate-bound envelope path"))?,
    );
    let authority = std::env::var("CTX_RELEASE_MANAGED_PAIR_AUTHORITY_JSON")?;
    // Validate before creating any test installation, using the actual verifier.
    let identity = ReleaseManagedPairVerifier::for_channel("stable")?
        .verify_signed_envelope(&bounded_read(&envelope, 2 * 1024 * 1024)?)?;
    let candidate_bytes = bounded_read(&candidate, RELEASE_ARTIFACT_MAX_BYTES)?;
    assert_eq!(identity.core().sha256(), sha256_hex(&candidate_bytes));
    assert_eq!(identity.core(), identity.companion());
    for case in ["complete", "tampered"] {
        let temp = tempfile::tempdir()?;
        create_private_directory_all(temp.path())?;
        let root = fs::canonicalize(temp.path())?;
        let mut command = Command::new(std::env::current_exe()?);
        command.env_clear();
        for key in ["SystemRoot", "WINDIR", "ComSpec"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        if let Some(windows) = std::env::var_os("SystemRoot") {
            command.env("PATH", PathBuf::from(windows).join("System32"));
        }
        command
            .args(["--exact", ACTOR, "--nocapture", "--test-threads=1"])
            .env("CTX_NATIVE_PAIR_ROOT", &root)
            .env("CTX_NATIVE_PAIR_CASE", case)
            .env("CTX_RELEASE_QUALIFICATION_CANDIDATE", &candidate)
            .env("CTX_RELEASE_QUALIFICATION_ENVELOPE", &envelope)
            .env("CTX_RELEASE_MANAGED_PAIR_AUTHORITY_JSON", &authority);
        // Every mutable product, provider and credential root is private to this case.
        for key in [
            "HOME",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "XDG_CACHE_HOME",
            "XDG_RUNTIME_DIR",
            "CODEX_HOME",
            "CLAUDE_CONFIG_DIR",
            "CTX_CONFIG_ROOT",
            "CTX_CACHE_ROOT",
            "CTX_STATE_ROOT",
            "TEMP",
            "TMP",
            "GNUPGHOME",
        ] {
            let path = root.join(key);
            create_private_directory_all(&path)?;
            command.env(key, path);
        }
        command
            .env("CTX_DATA_ROOT", root.join("data"))
            .env_remove("CTX_UPGRADE_TEST_TARGET")
            .env_remove("CTX_DAEMON_UPGRADE_HANDOFF_TOKEN");
        let mut parent = command.spawn()?;
        let deadline = Instant::now() + Duration::from_secs(30);
        let pid = loop {
            if let Ok(text) = fs::read_to_string(root.join("helper.pid")) {
                break text.parse::<u32>()?;
            }
            if let Some(status) = parent.try_wait()? {
                bail!("qualification parent exited before ready: {status}");
            }
            if Instant::now() >= deadline {
                parent.kill()?;
                parent.wait()?;
                bail!("qualification parent timed out before helper ready");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        // Open the exact still-running child while its real parent is alive and
        // the installation lock is held. The PID cannot be recycled in this window.
        let raw = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                0,
                pid,
            )
        };
        if raw.is_null() {
            parent.kill()?;
            parent.wait()?;
            return Err(std::io::Error::last_os_error().into());
        }
        let helper = unsafe { OwnedHandle::from_raw_handle(raw) };
        fs::write(root.join("parent-may-exit"), b"go")?;
        let status = unsafe { WaitForSingleObject(helper.as_raw_handle(), 30_000) };
        if status != WAIT_OBJECT_0 {
            unsafe {
                TerminateProcess(helper.as_raw_handle(), 1);
            }
            let _ = parent.kill();
            let _ = parent.wait();
            bail!("rebuilt qualification helper did not terminate within 30 seconds");
        }
        assert!(parent.wait()?.success());
        let mut exit_code = 0;
        if unsafe { GetExitCodeProcess(helper.as_raw_handle(), &mut exit_code) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let install_root = root.join("install");
        let core = install_root.join("bin/ctx.exe");
        let state: Value = serde_json::from_slice(&fs::read(
            install_root.join("bin/.ctx.exe.upgrade-state.json"),
        )?)?;
        let marker: Value =
            serde_json::from_slice(&fs::read(install::install_marker_path(&core))?)?;
        if case == "complete" {
            assert_eq!(exit_code, 0);
            assert_eq!(sha256_hex(&fs::read(&core)?), identity.core().sha256());
            assert_eq!(state["status"], "applied");
            assert!(marker.get("managed_pair").is_none());
            for path in [
                "libexec/ctx-pro.exe",
                "share/ctx/managed-pair-state.json",
                "share/ctx/managed-pair-envelope.json",
                MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH,
            ] {
                assert!(!install_root.join(path).exists(), "retained {path}");
            }
            let handoff: Value =
                serde_json::from_slice(&fs::read(root.join("data/daemon/upgrade-handoff.json"))?)?;
            assert_eq!(handoff["phase"], "completed");
        } else {
            assert_ne!(exit_code, 0);
            assert_eq!(fs::read(core)?, OLD);
            assert_ne!(state["status"], "applied");
            assert!(install_root
                .join(MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH)
                .exists());
        }
        assert_eq!(
            fs::read(root.join("data/pro/graph"))?,
            b"authored preserved legacy graph"
        );
        assert_eq!(
            fs::read(root.join("data/pro/key"))?,
            b"authored preserved legacy key"
        );
        println!(
            "{}",
            json!({"evidence":"rebuilt_qualification_candidate_helper", "case":case,
            "candidate_sha256":identity.core().sha256(), "helper_exit_code":exit_code,
            "old_layout":"authored", "stock_1_4_discovery":false, "byte_identical_signed_release":false})
        );
    }
    Ok(())
}

#[test]
fn native_helper_parent() -> Result<()> {
    let Some(root) = std::env::var_os("CTX_NATIVE_PAIR_ROOT").map(PathBuf::from) else {
        return Ok(());
    };
    let candidate = bounded_read(
        &PathBuf::from(std::env::var_os("CTX_RELEASE_QUALIFICATION_CANDIDATE").unwrap()),
        RELEASE_ARTIFACT_MAX_BYTES,
    )?;
    let envelope = bounded_read(
        &PathBuf::from(std::env::var_os("CTX_RELEASE_QUALIFICATION_ENVELOPE").unwrap()),
        2 * 1024 * 1024,
    )?;
    let verifier = ReleaseManagedPairVerifier::for_channel("stable")?;
    let identity = verifier.verify_signed_envelope(&envelope)?;
    let install_root = root.join("install");
    let data = root.join("data");
    for path in [
        install_root.join("bin"),
        data.join("daemon"),
        data.join("pro"),
        root.join("inputs"),
    ] {
        create_private_directory_all(&path)?;
    }
    let data = fs::canonicalize(data)?;
    fs::write(data.join("pro/graph"), b"authored preserved legacy graph")?;
    fs::write(data.join("pro/key"), b"authored preserved legacy key")?;
    let core = install_root.join("bin/ctx.exe");
    fs::write(&core, OLD)?;
    restrict_private_executable(&core)?;
    let marker = |version: &str, hash: &str, paired: bool| {
        json!({"schema_version":1,
        "manager":"ctx-hosted-installer", "install_path":core, "platform":"windows-x64",
        "channel":"stable", "version":version, "sha256":hash, "managed_pair":paired})
    };
    write_json(
        &install::install_marker_path(&core),
        &marker("1.4.12", &sha256_hex(OLD), false),
    )?;
    let inputs = root.join("inputs");
    for (name, bytes) in [
        ("core", candidate.as_slice()),
        ("companion", candidate.as_slice()),
        ("envelope", envelope.as_slice()),
    ] {
        fs::write(inputs.join(name), bytes)?;
        restrict_private_file(&inputs.join(name))?;
    }
    write_json(
        &inputs.join("marker"),
        &marker(
            identity.release_name().trim_start_matches('v'),
            identity.core().sha256(),
            true,
        ),
    )?;
    let lock = InstallationLock::try_acquire_at_root(&install_root)?
        .ok_or_else(|| anyhow!("test installation busy"))?;
    let staged = stage_managed_pair_under_installation_lock(
        &install_root,
        &ManagedPairApplyInput::new(
            inputs.join("envelope"),
            inputs.join("core"),
            inputs.join("companion"),
            inputs.join("marker"),
        ),
        &verifier,
    )?;
    let ManagedPairStageOutcome::Staged { retained_core, .. } = staged else {
        bail!("fixture did not stage");
    };
    let helper = core.with_file_name(format!(".ctx.exe.ctx-upgrade-{ATTEMPT}.helper.exe"));
    fs::write(&helper, &candidate)?;
    restrict_private_executable(&helper)?;
    write_json(
        &core.with_file_name(".ctx.exe.upgrade-state.json"),
        &json!({
        "schema_version":1,"status":"scheduled","attempt_id":ATTEMPT,"attempt_source":"manual_apply",
        "managed_pair_apply":true,"managed_pair_data_root":data,"install_path":core,"channel":"stable",
        "managed_pair_interval_seconds":60,"managed_pair_core_sha256":identity.core().sha256(),
        "managed_pair_envelope_sha256":sha256_hex(&envelope),"managed_pair_helper_path":helper,
        "managed_pair_helper_parent_pid":std::process::id()}),
    )?;
    write_json(
        &data.join("daemon/upgrade-handoff.json"),
        &json!({"schema_version":1,
        "handoff_id":ATTEMPT,"phase":"ready","owner_pid":std::process::id(),
        "updated_at_ms":ctx_history_core::utc_now().timestamp_millis()}),
    )?;
    if std::env::var("CTX_NATIVE_PAIR_CASE")? == "tampered" {
        fs::write(retained_core, b"tampered retained executable")?;
    }
    let pid = install::spawn_managed_pair_helper(
        &TEST_RELEASE_PROCESS,
        &helper,
        &data,
        &core,
        ATTEMPT,
        std::process::id(),
    )?;
    fs::write(root.join("helper.pid"), pid.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while !root.join("parent-may-exit").exists() {
        if Instant::now() >= deadline {
            bail!("test controller did not acknowledge helper");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(lock);
    Ok(()) // The real helper waits for this actual parent process to exit.
}

fn bounded_read(path: &Path, bound: u64) -> Result<Vec<u8>> {
    install::read_stable_file(
        path,
        "qualification input",
        bound,
        install::StableFileKind::Data,
    )?
    .ok_or_else(|| anyhow!("qualification input missing: {}", path.display()))
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    fs::write(path, serde_json::to_vec(value)?)?;
    restrict_private_file(path)?;
    Ok(())
}
