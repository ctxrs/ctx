use super::*;
use std::{
    io::{self, Write as _},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use crate::{
    daemon_lock_is_active, observe_pid_advisory_lock, pid_from_lock_json, DaemonLock,
    PidAdvisoryLockObservation,
};

const CHILD_ROOT: &str = "CTX_PROCESS_INSPECTION_TEST_ROOT";
const CHILD_TEST: &str =
    "process::linux_inspection_tests::live_owner_inspection_denial_preserves_pid_and_io_error";

struct ReapedChild(Child);

impl Drop for ReapedChild {
    fn drop(&mut self) {
        // Assertion failures must not leave a non-dumpable lock owner behind.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn inspection_child(root: &Path) -> Result<()> {
    // Only the re-executed child reaches this branch. Never change dumpability
    // or fork directly in the multithreaded parent test runner.
    unsafe { libc::alarm(30) };
    let _owner = DaemonLock::acquire(&root.join("data"))?
        .context("inspection child could not acquire its isolated daemon lock")?;
    let mut input = io::stdin().lock();
    for (phase, dumpable) in [("readable", 1), ("denied", 0), ("restored", 1)] {
        if unsafe {
            libc::prctl(
                libc::PR_SET_DUMPABLE,
                dumpable as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
            )
        } != 0
        {
            return Err(io::Error::last_os_error()).context("set child dumpability");
        }
        fs::write(root.join(phase), b"ready")?;
        let mut acknowledgement = [0];
        input.read_exact(&mut acknowledgement)?;
        anyhow::ensure!(acknowledgement == *b"+", "invalid phase acknowledgement");
    }
    Ok(())
}

fn wait_for_phase(child: &mut Child, root: &Path, phase: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !root.join(phase).exists() {
        anyhow::ensure!(
            child.try_wait()?.is_none(),
            "inspection child exited before phase {phase}"
        );
        anyhow::ensure!(
            Instant::now() < deadline,
            "inspection child did not reach phase {phase}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

#[test]
fn live_owner_inspection_denial_preserves_pid_and_io_error() -> Result<()> {
    if let Some(root) = env::var_os(CHILD_ROOT) {
        return inspection_child(&PathBuf::from(root));
    }

    let root = tempfile::tempdir()?;
    let data_root = root.path().join("data");
    let executable = env::current_exe()?;
    let mut command = Command::new(&executable);
    command
        .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
        .env(CHILD_ROOT, root.path())
        .env("CTX_DATA_ROOT", &data_root)
        .env("CTX_ANALYTICS_ENABLED", "false")
        .env("CTX_LOCAL_USAGE_ENABLED", "false")
        .env("CTX_DAEMON_AUTOSTART_OFF", "1")
        .env("CTX_UPGRADE_AUTO", "off")
        .current_dir(root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    for variable in [
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        "XDG_RUNTIME_DIR",
        "TMPDIR",
    ] {
        let path = root.path().join(variable);
        fs::create_dir(&path)?;
        command.env(variable, path);
    }
    let mut child = ReapedChild(command.spawn()?);
    let pid = child.0.id();
    let mut input = child.0.stdin.take().context("inspection child stdin")?;
    wait_for_phase(&mut child.0, root.path(), "readable")?;
    let lock_path = daemon_lock_path(&data_root);
    let owner = read_pid_lock_json(&lock_path).context("read child owner identity")?;
    assert_eq!(pid_from_lock_json(&owner), Some(pid));

    for phase in ["readable", "denied", "restored"] {
        wait_for_phase(&mut child.0, root.path(), phase)?;
        assert!(child.0.try_wait()?.is_none());
        assert_eq!(process_state(pid), ProcessState::Running);
        assert!(daemon_lock_is_active(&data_root));
        assert_eq!(
            observe_pid_advisory_lock(&lock_path),
            Some(PidAdvisoryLockObservation {
                held: true,
                released: false,
            })
        );
        assert_eq!(read_pid_lock_json(&lock_path).as_ref(), Some(&owner));

        if phase == "denied" {
            // A privileged observer with CAP_SYS_PTRACE can bypass this
            // restriction. Require real denial; never silently pass without
            // exercising the regression on the ordinary unprivileged runner.
            let denied_io = fs::File::open(format!("/proc/{pid}/exe"))
                .expect_err("non-dumpable child's image must be inaccessible to this observer");
            assert_eq!(denied_io.kind(), io::ErrorKind::PermissionDenied);
            let error = daemon_owner_binary_identity_matches(&owner, &executable)
                .expect_err("denied inspection must not become an image mismatch");
            let denied = error
                .downcast_ref::<ProcessExecutableInspectionDenied>()
                .context("missing typed executable-inspection denial")?;
            assert_eq!(denied.pid, pid);
            let source = error
                .chain()
                .find_map(|cause| cause.downcast_ref::<io::Error>())
                .context("executable-inspection denial lost its I/O cause")?;
            assert_eq!(source.kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(source.raw_os_error(), denied_io.raw_os_error());
            // Termination callers still receive no usable image evidence.
            assert_eq!(process_executable_sha256(pid), None);
        } else {
            assert!(daemon_owner_binary_identity_matches(&owner, &executable)?);
            assert_eq!(
                process_executable_sha256(pid).as_deref(),
                owner.get("binary_sha256").and_then(Value::as_str)
            );
        }
        input.write_all(b"+")?;
    }

    drop(input);
    assert!(child.0.wait()?.success(), "inspection child failed");
    assert_eq!(process_state(pid), ProcessState::NotRunning);
    assert!(!daemon_lock_is_active(&data_root));
    assert_eq!(process_executable_sha256(pid), None);
    match daemon_owner_binary_identity_matches(&owner, &executable) {
        Ok(matches) => assert!(!matches, "reaped owner cannot match a live image"),
        Err(error) => {
            assert!(error
                .downcast_ref::<ProcessExecutableInspectionDenied>()
                .is_none());
            assert_eq!(
                error.downcast_ref::<io::Error>().map(io::Error::kind),
                Some(io::ErrorKind::NotFound)
            );
        }
    }
    Ok(())
}
