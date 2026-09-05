#[cfg(test)]
use std::{fs, path::PathBuf};
use std::{path::Path, process};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use windows_sys::Win32::{Foundation::HANDLE, System::Threading::TerminateProcess};

use super::super::DAEMON_UPGRADE_RESTART_TIMEOUT;
use crate::{
    daemon_lock_path, observe_pid_advisory_lock, pid_from_lock_json, read_pid_lock_json,
    PidAdvisoryLockObservation, PID_LOCK_PROTOCOL,
};

mod image_identity;
mod process_handle;
use image_identity::{verify_lock_paths, verify_recorded_digest_identity};
use process_handle::{filetime_unix_ms, WindowsProcess, WindowsProcessAccess};

const PROCESS_START_MAX_DELAY_MS: i64 = 5 * 60 * 1_000;
const PROCESS_START_CLOCK_SKEW_MS: i64 = 5_000;

pub fn terminate_identity_verified_residual_daemon(
    data_root: &Path,
    expected_executable: &Path,
) -> Result<()> {
    terminate_identity_verified_residual_daemon_owner(data_root, expected_executable, None)
}

pub fn terminate_identity_verified_residual_daemon_owner(
    data_root: &Path,
    expected_executable: &Path,
    expected_owner_id: Option<&str>,
) -> Result<()> {
    let lock_path = daemon_lock_path(data_root);
    let value = read_pid_lock_json(&lock_path)
        .ok_or_else(|| anyhow!("active ctx daemon lock has no readable identity"))?;
    let pid = pid_from_lock_json(&value)
        .ok_or_else(|| anyhow!("active ctx daemon lock has no process identity"))?;
    if pid == process::id() {
        return Err(anyhow!("refusing to terminate the current ctx process"));
    }
    let observed_owner_id = value.get("owner_id").and_then(Value::as_str);
    if expected_owner_id.is_some() && observed_owner_id != expected_owner_id {
        return Err(anyhow!(
            "ctx daemon ownership changed after health verification; refusing to terminate"
        ));
    }
    verify_lock_paths(&value, data_root, expected_executable)?;

    let owner_released = value.get("released").and_then(Value::as_bool) == Some(true);

    let access = if owner_released {
        WindowsProcessAccess::Observe
    } else {
        WindowsProcessAccess::ModernTerminate
    };
    let Some(target) = WindowsProcess::open(pid, access)? else {
        if advisory_lock_is_held(data_root) {
            return Err(anyhow!(
                "ctx daemon owner lock is held but its recorded process is not running"
            ));
        }
        return Ok(());
    };
    if owner_released {
        return wait_for_released_process(target, &value);
    }
    match observe_pid_advisory_lock(&lock_path) {
        Some(PidAdvisoryLockObservation { held: false, .. }) => {
            return wait_for_released_process(target, &value);
        }
        Some(PidAdvisoryLockObservation {
            held: true,
            released: false,
        }) => {}
        None => {
            return Err(anyhow!(
                "ctx daemon owner lock state is unreadable; refusing residual termination"
            ));
        }
        Some(PidAdvisoryLockObservation {
            held: true,
            released: true,
        }) => {
            if let Some(current) = read_pid_lock_json(&lock_path) {
                if is_same_owner_release_transition(&value, &current) {
                    return wait_for_released_process(target, &current);
                }
            }
            return Err(anyhow!(
                "ctx daemon owner lock state is inconsistent; refusing residual termination"
            ));
        }
    }

    verify_recorded_digest_identity(pid, &value)?;

    let owner_id = expected_owner_id.or(observed_owner_id);
    if let OwnerMetadataStatus::Released(current) =
        recheck_owner_metadata(&lock_path, &value, pid, owner_id)?
    {
        return wait_for_released_process(target, &current);
    }
    if !advisory_lock_is_held(data_root) {
        return wait_for_released_process(target, &value);
    }
    if let OwnerMetadataStatus::Released(current) =
        recheck_owner_metadata(&lock_path, &value, pid, owner_id)?
    {
        return wait_for_released_process(target, &current);
    }
    if !target.is_running()? {
        return Ok(());
    }
    terminate_process_and_wait(&target)
}

pub fn wait_for_released_residual_daemon(
    data_root: &Path,
    expected_executable: &Path,
) -> Result<()> {
    let lock_path = daemon_lock_path(data_root);
    let Some(value) = read_pid_lock_json(&lock_path) else {
        return Ok(());
    };
    if value.get("lock_protocol").and_then(Value::as_str) != Some(PID_LOCK_PROTOCOL) {
        return Ok(());
    }
    let Some(observation) = observe_pid_advisory_lock(&lock_path) else {
        return Err(anyhow!(
            "ctx daemon owner lock state is unreadable after cooperative shutdown"
        ));
    };
    if observation.held && !observation.released {
        return Ok(());
    }
    let pid = pid_from_lock_json(&value)
        .ok_or_else(|| anyhow!("released ctx daemon lock has no process identity"))?;
    if pid == process::id() {
        return Err(anyhow!(
            "released ctx daemon lock names the current process"
        ));
    }
    let Some(target) = WindowsProcess::open(pid, WindowsProcessAccess::Observe)? else {
        return Ok(());
    };
    match verify_process_start_for_released_lock(&value, &target)? {
        ReleasedProcessIdentity::OriginalOwner => {}
        ReleasedProcessIdentity::ReusedPid => return Ok(()),
    }
    verify_lock_paths(&value, data_root, expected_executable)?;
    wait_for_released_process(target, &value)
}

fn is_same_owner_release_transition(before: &Value, after: &Value) -> bool {
    if before.get("released").and_then(Value::as_bool) != Some(false)
        || after.get("released").and_then(Value::as_bool) != Some(true)
    {
        return false;
    }
    let mut expected = before.clone();
    let Some(object) = expected.as_object_mut() else {
        return false;
    };
    object.insert("released".to_owned(), Value::Bool(true));
    expected == *after
}

enum OwnerMetadataStatus {
    Unchanged,
    Released(Value),
}

fn recheck_owner_metadata(
    lock_path: &Path,
    original: &Value,
    pid: u32,
    owner_id: Option<&str>,
) -> Result<OwnerMetadataStatus> {
    let current = read_pid_lock_json(lock_path)
        .ok_or_else(|| anyhow!("ctx daemon ownership disappeared before termination"))?;
    if pid_from_lock_json(&current) != Some(pid)
        || owner_id.is_some_and(|expected| {
            current.get("owner_id").and_then(Value::as_str) != Some(expected)
        })
    {
        return Err(anyhow!(
            "ctx daemon ownership changed before termination; refusing to terminate"
        ));
    }
    if current == *original {
        return Ok(OwnerMetadataStatus::Unchanged);
    }
    if is_same_owner_release_transition(original, &current) {
        return Ok(OwnerMetadataStatus::Released(current));
    }
    Err(anyhow!(
        "ctx daemon ownership metadata changed before termination; refusing to terminate"
    ))
}

fn verify_released_process_start(
    value: &Value,
    target: &WindowsProcess,
) -> Result<ReleasedProcessIdentity> {
    let started_at_ms = value
        .get("started_at_ms")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("ctx daemon lock has no process-start identity"))?;
    let creation_ms = filetime_unix_ms(target.creation_time).ok_or_else(|| {
        anyhow!(
            "ctx daemon process {} has an invalid creation identity",
            target.pid
        )
    })?;
    if creation_ms > started_at_ms.saturating_add(PROCESS_START_CLOCK_SKEW_MS) {
        return Ok(ReleasedProcessIdentity::ReusedPid);
    }
    if started_at_ms.saturating_sub(creation_ms) > PROCESS_START_MAX_DELAY_MS {
        return Err(anyhow!(
            "ctx daemon lock timestamp does not bind its recorded process; refusing to wait"
        ));
    }
    Ok(ReleasedProcessIdentity::OriginalOwner)
}

fn verify_process_start_for_released_lock(
    value: &Value,
    target: &WindowsProcess,
) -> Result<ReleasedProcessIdentity> {
    if value.get("binary_sha256").and_then(Value::as_str).is_none() {
        return Err(anyhow!(
            "released ctx daemon lock has an invalid executable digest identity"
        ));
    }
    verify_released_process_start(value, target)
}

fn wait_for_released_process(target: WindowsProcess, value: &Value) -> Result<()> {
    match verify_process_start_for_released_lock(value, &target)? {
        ReleasedProcessIdentity::OriginalOwner => {}
        ReleasedProcessIdentity::ReusedPid => return Ok(()),
    }
    target
        .wait_for_exit(DAEMON_UPGRADE_RESTART_TIMEOUT)
        .context("wait for released ctx daemon owner process to exit")
}

fn terminate_process_and_wait(target: &WindowsProcess) -> Result<()> {
    terminate_process_and_wait_with(target, |handle| {
        if unsafe { TerminateProcess(handle, 0) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    })
}

fn terminate_process_and_wait_with(
    target: &WindowsProcess,
    terminate: impl FnOnce(HANDLE) -> std::io::Result<()>,
) -> Result<()> {
    if let Err(error) = terminate(target.handle) {
        if target.is_running().is_ok_and(|running| !running) {
            return Ok(());
        }
        return Err(error).context("terminate identity-verified residual ctx daemon");
    }
    target
        .wait_for_exit(DAEMON_UPGRADE_RESTART_TIMEOUT)
        .context("wait for terminated residual ctx daemon process to exit")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReleasedProcessIdentity {
    OriginalOwner,
    ReusedPid,
}

fn advisory_lock_is_held(data_root: &Path) -> bool {
    observe_pid_advisory_lock(&daemon_lock_path(data_root)).is_some_and(|state| state.held)
}

#[cfg(test)]
mod tests {
    use std::{
        env,
        process::{Child, Command, Stdio},
        sync::{Mutex, MutexGuard},
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use fs2::FileExt;
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::{
        open_or_create_pid_lock_file, publish_pid_lock_metadata, secure_private_file_permissions,
        try_lock_pid_file,
    };

    pub(super) const CHILD_TEST: &str = "handoff::termination::windows::tests::daemon_owner_child";
    pub(super) const CHILD_MODE_ENV: &str = "CTX_TEST_DAEMON_OWNER_CHILD_MODE";
    pub(super) const CHILD_ROOT_ENV: &str = "CTX_TEST_DAEMON_OWNER_ROOT";
    static FIXTURE_TEST_LOCK: Mutex<()> = Mutex::new(());

    pub(super) struct DaemonFixture {
        _temp: TempDir,
        pub(super) active: PathBuf,
        pub(super) root: PathBuf,
        pub(super) owner: Child,
    }

    impl DaemonFixture {
        pub(super) fn start() -> Self {
            let temp = tempfile::tempdir().expect("temporary daemon fixture");
            let active = temp.path().join("ctx.exe");
            let root = temp.path().join("data");
            fs::create_dir_all(&root).expect("create daemon data root");
            fs::copy(
                env::current_exe().expect("current test executable"),
                &active,
            )
            .expect("copy daemon fixture executable");
            let owner = spawn_fixture_child(&active, &root, "owner");
            wait_for_path(&root.join("owner-ready"));
            assert_eq!(
                observe_pid_advisory_lock(&daemon_lock_path(&root)),
                Some(PidAdvisoryLockObservation {
                    held: true,
                    released: false,
                }),
                "daemon fixture readiness did not publish a held advisory owner"
            );
            Self {
                _temp: temp,
                active,
                root,
                owner,
            }
        }
    }

    impl Drop for DaemonFixture {
        fn drop(&mut self) {
            if self.owner.try_wait().ok().flatten().is_none() {
                let _ = self.owner.kill();
            }
            let _ = self.owner.wait();
        }
    }

    #[test]
    fn missing_or_null_digest_never_terminates_a_live_owner() {
        let _serial = fixture_test_guard();
        let mut fixture = DaemonFixture::start();
        let lock_path = daemon_lock_path(&fixture.root);
        let original = read_pid_lock_json(&lock_path).expect("owner metadata");
        for digest in [None, Some(Value::Null)] {
            let mut value = original.clone();
            value.as_object_mut().unwrap().remove("binary_sha256");
            if let Some(digest) = digest {
                value["binary_sha256"] = digest;
            }
            fs::write(&lock_path, serde_json::to_vec(&value).unwrap()).unwrap();
            let error = terminate_identity_verified_residual_daemon(&fixture.root, &fixture.active)
                .expect_err("digest-free identity must be rejected");
            assert!(
                error.to_string().contains("no executable digest identity"),
                "{error:#}"
            );
            assert!(fixture.owner.try_wait().unwrap().is_none());

            value["released"] = Value::Bool(true);
            fs::write(&lock_path, serde_json::to_vec(&value).unwrap()).unwrap();
            let error = wait_for_released_residual_daemon(&fixture.root, &fixture.active)
                .expect_err("released digest-free identity must be rejected");
            assert!(
                error
                    .to_string()
                    .contains("invalid executable digest identity"),
                "{error:#}"
            );
            assert!(fixture.owner.try_wait().unwrap().is_none());
        }
        fs::write(&lock_path, serde_json::to_vec(&original).unwrap()).unwrap();
    }

    #[test]
    fn digest_mismatch_does_not_terminate_the_owner() {
        let _serial = fixture_test_guard();
        let mut fixture = DaemonFixture::start();
        let mut value =
            read_pid_lock_json(&daemon_lock_path(&fixture.root)).expect("daemon fixture metadata");
        value["binary_sha256"] = Value::String("0".repeat(64));
        fs::write(
            daemon_lock_path(&fixture.root),
            serde_json::to_vec(&value).expect("encode digest-bearing lock"),
        )
        .expect("publish digest-bearing lock");

        let error = terminate_identity_verified_residual_daemon(&fixture.root, &fixture.active)
            .expect_err("digest mismatch must reject termination");
        assert!(
            error
                .to_string()
                .contains("owner image does not match its held ctx daemon lock"),
            "{error:#}"
        );
        assert!(fixture.owner.try_wait().expect("inspect owner").is_none());
    }

    #[test]
    fn digest_bearing_owner_terminates_with_modern_process_rights() {
        let _serial = fixture_test_guard();
        let mut fixture = DaemonFixture::start();
        let target = WindowsProcess::open(fixture.owner.id(), WindowsProcessAccess::Observe)
            .expect("open modern owner signal handle")
            .expect("live modern owner");
        terminate_identity_verified_residual_daemon(&fixture.root, &fixture.active)
            .expect("terminate digest-bound modern owner");
        assert!(
            !target.is_running().expect("inspect modern owner signal"),
            "modern residual termination returned before process exit"
        );
        assert!(fixture
            .owner
            .try_wait()
            .expect("inspect modern owner")
            .is_some());
    }

    #[test]
    fn released_metadata_while_guard_is_held_waits_for_clean_exit() {
        let _serial = fixture_test_guard();
        let mut fixture = DaemonFixture::start();
        let target = WindowsProcess::open(fixture.owner.id(), WindowsProcessAccess::Observe)
            .expect("open releasing daemon owner signal handle")
            .expect("live releasing daemon owner");
        fs::write(fixture.root.join("release-trigger"), b"release")
            .expect("trigger daemon release publication");
        wait_for_path(&fixture.root.join("release-published"));
        assert_eq!(
            observe_pid_advisory_lock(&daemon_lock_path(&fixture.root)),
            Some(PidAdvisoryLockObservation {
                held: true,
                released: true,
            }),
            "fixture did not publish released metadata while retaining its guard"
        );

        terminate_identity_verified_residual_daemon(&fixture.root, &fixture.active)
            .expect("wait for releasing daemon owner");
        assert!(!target.is_running().expect("inspect releasing owner"));
        let status = fixture
            .owner
            .try_wait()
            .expect("inspect clean releasing owner")
            .expect("releasing owner did not exit before return");
        assert!(status.success(), "{status}");
        assert!(fixture.root.join("clean-exit").exists());
    }

    #[test]
    fn released_guard_with_true_metadata_waits_for_clean_exit_and_retry_is_idempotent() {
        let _serial = fixture_test_guard();
        let mut fixture = DaemonFixture::start();
        let target = WindowsProcess::open(fixture.owner.id(), WindowsProcessAccess::Observe)
            .expect("open released daemon owner signal handle")
            .expect("live released daemon owner");
        fs::write(fixture.root.join("release-trigger"), b"release")
            .expect("trigger daemon guard release");
        wait_for_path(&fixture.root.join("guard-released"));
        let released = read_pid_lock_json(&daemon_lock_path(&fixture.root))
            .expect("released daemon fixture metadata");
        assert_eq!(released["released"], true, "{released:#}");

        terminate_identity_verified_residual_daemon(&fixture.root, &fixture.active)
            .expect("wait for released daemon owner");
        assert!(
            !target.is_running().expect("inspect released owner signal"),
            "released-owner wait returned before its process handle was signaled"
        );
        let status = fixture
            .owner
            .try_wait()
            .expect("inspect clean daemon owner")
            .expect("released-owner wait returned before the child exited");
        assert!(status.success(), "{status}");
        assert!(fixture.root.join("clean-exit").exists());

        wait_for_released_residual_daemon(&fixture.root, &fixture.active)
            .expect("released-owner retry");
    }

    #[test]
    fn natural_exit_after_running_check_preserves_success_on_the_same_handle() {
        let _serial = fixture_test_guard();
        let mut fixture = DaemonFixture::start();
        let target =
            WindowsProcess::open(fixture.owner.id(), WindowsProcessAccess::ModernTerminate)
                .expect("open natural-exit fixture handle")
                .expect("live natural-exit fixture");
        assert!(target.is_running().expect("initial running check"));

        let error =
            terminate_process_and_wait_with(&target, |_| Err(std::io::Error::from_raw_os_error(5)))
                .expect_err("a failed termination of a live process must remain an error");
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .and_then(std::io::Error::raw_os_error),
            Some(5),
            "termination failure did not preserve its original OS error: {error:#}"
        );
        assert!(target
            .is_running()
            .expect("running after failed termination"));

        terminate_process_and_wait_with(&target, |_| {
            fixture.owner.kill()?;
            fixture.owner.wait()?;
            Err(std::io::Error::from_raw_os_error(5))
        })
        .expect("natural exit after the running check");
        assert!(
            !target.is_running().expect("inspect natural-exit handle"),
            "natural-exit recovery accepted an unsignaled process handle"
        );
    }

    #[test]
    fn daemon_owner_child() {
        let Some(mode) = env::var_os(CHILD_MODE_ENV) else {
            return;
        };
        let root = PathBuf::from(env::var_os(CHILD_ROOT_ENV).expect("daemon child root"));
        if mode == "takeover" {
            let expected = env::current_exe().expect("takeover child executable");
            terminate_identity_verified_residual_daemon(&root, &expected)
                .expect("daemon takeover child");
            return;
        }
        let daemon_root = root.join("daemon");
        fs::create_dir_all(&daemon_root).expect("create daemon root");
        let guard_path = daemon_root.join("daemon.guard");
        let (guard, _) = open_or_create_pid_lock_file(&guard_path).expect("open daemon guard");
        secure_private_file_permissions(&guard_path).expect("secure daemon guard");
        assert!(
            try_lock_pid_file(&guard).expect("hold daemon advisory guard"),
            "daemon fixture guard unexpectedly contended"
        );
        let lock_path = daemon_root.join("daemon.lock");
        let value = daemon_lock_value(
            &root,
            &env::current_exe().expect("daemon child executable"),
            std::process::id(),
            unix_now_ms(),
            false,
        );
        assert!(
            publish_pid_lock_metadata(&lock_path, &value).expect("publish daemon child lock"),
            "daemon child lock publication was rejected"
        );
        fs::write(root.join("owner-ready"), b"ready").expect("publish owner readiness");

        let deadline = Instant::now() + Duration::from_secs(30);
        while !root.join("release-trigger").exists() {
            assert!(
                Instant::now() < deadline,
                "daemon child exceeded its test lease"
            );
            thread::sleep(Duration::from_millis(20));
        }
        let mut released = read_pid_lock_json(&lock_path).expect("read owner metadata to release");
        released["released"] = Value::Bool(true);
        assert!(
            publish_pid_lock_metadata(&lock_path, &released)
                .expect("publish released owner metadata"),
            "released owner metadata publication was rejected"
        );
        fs::write(root.join("release-published"), b"released")
            .expect("publish released-metadata readiness");
        thread::sleep(Duration::from_secs(1));
        FileExt::unlock(&guard).expect("release daemon advisory guard");
        drop(guard);
        fs::write(root.join("guard-released"), b"released").expect("publish guard release");
        thread::sleep(Duration::from_millis(250));
        fs::write(root.join("clean-exit"), b"clean").expect("publish clean exit");
    }

    pub(super) fn spawn_fixture_child(binary: &Path, root: &Path, mode: &str) -> Child {
        Command::new(binary)
            .args(["--exact", CHILD_TEST, "--nocapture"])
            .env(CHILD_MODE_ENV, mode)
            .env(CHILD_ROOT_ENV, root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn daemon fixture child")
    }

    pub(super) fn fixture_test_guard() -> MutexGuard<'static, ()> {
        FIXTURE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(super) fn daemon_lock_value(
        root: &Path,
        binary: &Path,
        pid: u32,
        started_at_ms: i64,
        released: bool,
    ) -> Value {
        json!({
            "lock_protocol": "advisory-v1",
            "owner_id": format!("daemon-fixture-{pid}"),
            "pid": pid,
            "released": released,
            "started_at_ms": started_at_ms,
            "binary": binary,
            "binary_sha256": crate::executable_sha256(binary).expect("fixture executable digest"),
            "data_root": root,
        })
    }

    pub(super) fn unix_now_ms() -> i64 {
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_millis(),
        )
        .expect("current time fits i64 milliseconds")
    }

    pub(super) fn wait_for_path(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {}",
                path.display()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}
