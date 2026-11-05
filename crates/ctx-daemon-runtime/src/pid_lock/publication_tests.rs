use super::*;
use std::{
    io::Read,
    process::{Child, Command, Stdio},
    time::Instant,
};

const CHILD_ROOT: &str = "CTX_PID_CRASH_TEST_ROOT";
const CHILD_TEST: &str =
    "pid_lock::publication_tests::restart_reclaims_killed_publishers_without_replacing_guard";

struct ReapedChild(Child);

impl Drop for ReapedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn pause_before_rename(path: &Path) {
    fs::write(path.with_extension("ready"), b"ready").expect("signal publication pause");
    std::io::stdin()
        .read_exact(&mut [0])
        .expect("wait for parent at publication pause");
}

#[test]
fn restart_reclaims_killed_publishers_without_replacing_guard() -> Result<()> {
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        crate::private_json::tests::BEFORE_REPLACE.with(|hook| hook.set(Some(pause_before_rename)));
        let _lock = DaemonLock::acquire_for_finite_worker(Path::new(&root))?
            .context("child acquires guard")?;
        anyhow::bail!("publisher should be killed before rename");
    }
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("data");
    let path = daemon_lock_path(&root);
    let parent = path.parent().context("metadata directory")?;
    create_private_dir_all(parent)?;
    let retained = [
        "status.json.123.0.tmp",
        "other-daemon.json.123.0.tmp",
        "daemon.json.123.tmp",
        "daemon.json.pid.0.tmp",
        "daemon.json.123.sequence.tmp",
        "daemon.json.123.0.tmp.backup",
    ];
    for name in retained {
        fs::write(parent.join(name), b"retained")?;
    }
    let retained_directory = parent.join("daemon.json.123.4.tmp");
    fs::create_dir(&retained_directory)?;
    let guard_path = pid_lock_guard_path(&path);
    // Keep a handle across every crash/restart. Contention on this handle
    // proves that ownership still uses the original guard, on every platform.
    let original_guard = private_create_new_lock_file(&guard_path)?;
    #[cfg(unix)]
    let original_inode = {
        use std::os::unix::fs::MetadataExt;
        original_guard.metadata()?.ino()
    };
    for round in 0..3 {
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
            fs::create_dir_all(&isolated)?;
            command.env(variable, isolated);
        }
        let mut child = ReapedChild(command.spawn()?);
        let deadline = Instant::now() + Duration::from_secs(30);
        let ready = path.with_extension("ready");
        while !ready.exists() {
            anyhow::ensure!(child.0.try_wait()?.is_none(), "writer exited before pause");
            anyhow::ensure!(Instant::now() < deadline, "writer timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!path.exists());
        assert_eq!(observe_pid_advisory_guard(&path), Some(true));
        assert!(!try_lock_pid_file(&original_guard)?);
        assert!(DaemonLock::acquire_for_finite_worker(&root)?.is_none());
        let abandoned = path.with_extension(format!("json.{}.0.tmp", child.0.id()));
        assert!(abandoned.is_file());
        child.0.kill()?;
        assert!(!child.0.wait()?.success());
        assert!(abandoned.is_file());
        assert!(try_lock_pid_file(&original_guard)?);
        fs2::FileExt::unlock(&original_guard)?;
        fs::remove_file(ready)?;
        // One publication temporary may remain after death; each later start
        // must reclaim the predecessor's file, including after repeated kills.
        assert_eq!(
            fs::read_dir(parent)?.count(),
            retained.len() + 3,
            "round {round}"
        );
    }
    let lock = DaemonLock::acquire_for_finite_worker(&root)?.context("restart owner")?;
    assert!(!try_lock_pid_file(&original_guard)?);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(fs::metadata(&guard_path)?.ino(), original_inode);
    }
    let identity = read_pid_lock_json(&path).context("restarted metadata")?;
    drop(lock);
    let released = read_pid_lock_json(&path).context("released metadata")?;
    assert_eq!(released["owner_id"], identity["owner_id"]);
    assert_eq!(released["released"], true);
    assert!(try_lock_pid_file(&original_guard)?);
    fs2::FileExt::unlock(&original_guard)?;
    for name in retained {
        assert_eq!(fs::read(parent.join(name))?, b"retained");
    }
    assert!(retained_directory.is_dir());
    assert_eq!(fs::read_dir(parent)?.count(), retained.len() + 3);
    Ok(())
}
