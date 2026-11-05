use super::*;
use std::{
    io::{Read, Write},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::Context;
use ctx_daemon_runtime::{observe_pid_advisory_guard, publish_pid_lock_metadata};

const CHILD_ROOT: &str = "CTX_PID_PUBLICATION_TEST_ROOT";
const CHILD_TEST: &str =
    "status::tests::publication::concurrent_lock_publication_keeps_legacy_reads_and_status_consistent";

struct ReapedChild(Child);

impl Drop for ReapedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn writer(root: &Path) -> anyhow::Result<()> {
    let lock = DaemonLock::acquire_for_finite_worker(root)?.context("child lock")?;
    let path = daemon_lock_path(root);
    let mut value = read_pid_lock_json(&path).context("child identity")?;
    let mut input = std::io::stdin().lock();
    let mut acknowledgement = [0];
    fs::write(root.join("ready"), b"ready")?;
    input.read_exact(&mut acknowledgement)?;
    for sequence in 0..256 {
        // Vary lengths and contents to expose both truncated and torn reads.
        value["publication_test"] = json!(sequence.to_string().repeat(2048));
        assert!(publish_pid_lock_metadata(&path, &value)?);
    }
    fs::write(root.join("updated"), b"updated")?;
    input.read_exact(&mut acknowledgement)?;
    drop(lock);
    Ok(())
}

fn assert_child_running(child: &mut Child, deadline: Instant) -> anyhow::Result<()> {
    anyhow::ensure!(child.try_wait()?.is_none(), "writer exited early");
    anyhow::ensure!(Instant::now() < deadline, "writer timed out");
    Ok(())
}

#[test]
fn concurrent_lock_publication_keeps_legacy_reads_and_status_consistent() -> anyhow::Result<()> {
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        return writer(Path::new(&root));
    }
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("data");
    let path = daemon_lock_path(&root);
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
        .env(CHILD_ROOT, &root)
        .env("CTX_DATA_ROOT", &root)
        .env("CTX_ANALYTICS_ENABLED", "false")
        .env("CTX_LOCAL_USAGE_ENABLED", "false")
        .env("CTX_DAEMON_AUTOSTART_OFF", "1")
        .env("CTX_UPGRADE_AUTO", "off")
        .current_dir(temp.path())
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
    ] {
        let isolated = temp.path().join(variable);
        fs::create_dir(&isolated)?;
        command.env(variable, isolated);
    }
    let mut child = ReapedChild(command.spawn()?);
    let deadline = Instant::now() + Duration::from_secs(30);
    // Initial publication must expose either no file or a complete old-format
    // record. These one-shot reads deliberately do not use the new retry path.
    while !root.join("ready").exists() {
        assert_child_running(&mut child.0, deadline)?;
        let status = report(&root, false, None);
        assert_ne!(status["status"], "stale_lock", "{status}");
        assert_eq!(status["recoverable"], false, "{status}");
        match fs::read(&path) {
            Ok(bytes) => {
                let _: Value = serde_json::from_slice(&bytes)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        std::thread::yield_now();
    }
    let identity: Value = serde_json::from_slice(&fs::read(&path)?)?;
    write_lifecycle_status(&root, "running", None)?;
    let mut input = child.0.stdin.take().context("writer stdin")?;
    input.write_all(b"+")?;
    loop {
        assert_child_running(&mut child.0, deadline)?;
        let value: Value = serde_json::from_slice(&fs::read(&path)?)?;
        assert_eq!(value["owner_id"], identity["owner_id"]);
        assert_eq!(value["pid"], child.0.id());
        assert_eq!(value["released"], false);
        let status = report(&root, false, None);
        assert_eq!(status["status"], "running", "{status}");
        if root.join("updated").exists() {
            break;
        }
    }
    assert_eq!(observe_pid_advisory_guard(&path), Some(true));
    assert!(DaemonLock::acquire_for_finite_worker(&root)?.is_none());
    input.write_all(b"+")?;
    while child.0.try_wait()?.is_none() {
        anyhow::ensure!(Instant::now() < deadline, "writer release timed out");
        let value: Value = serde_json::from_slice(&fs::read(&path)?)?;
        assert_eq!(value["owner_id"], identity["owner_id"]);
        std::thread::yield_now();
    }
    assert!(child.0.wait()?.success());
    let released: Value = serde_json::from_slice(&fs::read(&path)?)?;
    assert_eq!(released["owner_id"], identity["owner_id"]);
    assert_eq!(released["released"], true);
    assert_eq!(observe_pid_advisory_guard(&path), Some(false));
    assert!(DaemonLock::acquire_for_finite_worker(&root)?.is_some());
    Ok(())
}

#[test]
fn current_reader_spans_guard_acquisition_through_first_publication() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    create_private_dir_all(&ctx_daemon_runtime::daemon_root_path(root))?;
    let path = daemon_lock_path(root);
    let mut payload = ctx_daemon_runtime::current_daemon_lock_identity(root)?;
    payload["persistent"] = json!(false);
    assert_ne!(report(root, false, None)["status"], "stale_lock");
    let guard = private_create_new_lock_file(&pid_lock_guard_path(&path))?;
    assert_ne!(report(root, false, None)["status"], "stale_lock");
    assert!(ctx_daemon_runtime::try_lock_pid_file(&guard)?);
    // Hold the exact first-publication state deterministically: the guard is
    // owned, but the new metadata has not been renamed into place yet.
    assert!(!path.exists());
    for _ in 0..32 {
        let status = report(root, false, None);
        assert_eq!(status["status"], "unverified", "{status}");
        assert_eq!(status["recoverable"], false);
        assert!(status.get("live_pid").is_none());
    }
    assert!(DaemonLock::acquire_for_finite_worker(root)?.is_none());
    assert!(publish_pid_lock_metadata(&path, &payload)?);
    let published = report(root, false, None);
    assert_eq!(published["status"], "running", "{published}");
    assert_eq!(published["recoverable"], false);
    assert_eq!(published["live_pid"], process::id());
    Ok(())
}

#[test]
fn an_unreleased_lock_without_a_guard_owner_is_still_stale() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let lock = DaemonLock::acquire_for_finite_worker(temp.path())?.context("seed identity")?;
    let path = daemon_lock_path(temp.path());
    let mut abandoned = read_pid_lock_json(&path).context("seed metadata")?;
    drop(lock);
    abandoned["pid"] = json!(u32::MAX);
    abandoned["released"] = json!(false);
    write_private_json_file(&path, &abandoned)?;
    write_lifecycle_status(temp.path(), "running", None)?;
    let stale = report(temp.path(), false, None);
    assert_eq!(stale["status"], "stale_lock");
    assert_eq!(stale["running"], false);
    assert_eq!(stale["recoverable"], true);
    assert!(stale.get("live_pid").is_none());
    Ok(())
}

#[test]
fn an_incomplete_older_writer_is_unverified_until_publication_finishes() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let _lock = DaemonLock::acquire_for_finite_worker(temp.path())?.context("older writer")?;
    let path = daemon_lock_path(temp.path());
    let complete = fs::read(&path)?;
    for (lifecycle, expected) in [
        ("running", "unverified"),
        ("completed", "completed"),
        ("stopped", "stopped"),
        ("failed", "failed"),
    ] {
        write_lifecycle_status(temp.path(), lifecycle, None)?;
        for incomplete in [b"".as_slice(), b"{\"pid\":"] {
            fs::write(&path, incomplete)?;
            let status = report(temp.path(), false, None);
            assert_eq!(status["status"], expected, "{status}");
            assert_eq!(status["running"], false);
            assert_eq!(status["recoverable"], false);
            assert!(status.get("live_pid").is_none());
        }
    }
    fs::write(&path, complete)?;
    write_lifecycle_status(temp.path(), "running", None)?;
    assert_eq!(report(temp.path(), false, None)["status"], "running");
    Ok(())
}
