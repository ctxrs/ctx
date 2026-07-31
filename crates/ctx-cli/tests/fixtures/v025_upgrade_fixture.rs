#[cfg(unix)]
mod unix {
    use std::{
        env, fs,
        io::Write as _,
        os::unix::fs::PermissionsExt as _,
        path::{Path, PathBuf},
        process::{self, Command},
        thread,
        time::Duration,
    };

    use fs2::FileExt as _;
    use serde_json::json;
    use sha2::{Digest as _, Sha256};

    pub(super) fn run() -> Result<(), Box<dyn std::error::Error>> {
        let arguments = env::args_os().skip(1).collect::<Vec<_>>();
        let (data_root, command_index) = if arguments.first().is_some_and(|arg| arg == "--data-root")
        {
            let root = arguments
                .get(1)
                .ok_or("expected a path after --data-root")?;
            (PathBuf::from(root), 2)
        } else {
            (
                PathBuf::from(env::var_os("CTX_DATA_ROOT").ok_or(
                    "expected --data-root <path> or CTX_DATA_ROOT for v0.25 fixture command",
                )?),
                0,
            )
        };
        match arguments.get(command_index).and_then(|argument| argument.to_str()) {
            Some("daemon")
                if arguments
                    .get(command_index + 1)
                    .is_some_and(|argument| argument == "run") =>
            {
                run_daemon(&data_root)
            }
            Some("daemon")
                if arguments
                    .get(command_index + 1)
                    .is_some_and(|argument| argument == "disable") =>
            {
                disable_daemon(&data_root)
            }
            Some("daemon")
                if arguments
                    .get(command_index + 1)
                    .is_some_and(|argument| argument == "status") =>
            {
                daemon_status(&data_root)
            }
            Some("upgrade")
                if arguments
                    .get(command_index + 1)
                    .is_some_and(|argument| argument == "--candidate") =>
            {
                let candidate = arguments
                    .get(command_index + 2)
                    .ok_or("expected a candidate path")?;
                run_upgrade(&data_root, Path::new(candidate))
            }
            _ => Err("unsupported v0.25 fixture command".into()),
        }
    }

    fn run_daemon(data_root: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let daemon_root = data_root.join("daemon");
        fs::create_dir_all(&daemon_root)?;
        let guard_path = daemon_root.join("daemon.guard");
        let guard = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&guard_path)?;
        guard.try_lock_exclusive()?;
        let executable = fs::canonicalize(env::current_exe()?)?;
        let pid = process::id();
        write_json(
            &daemon_root.join("daemon.lock"),
            &json!({
                "lock_protocol": "advisory-v1",
                "owner_id": format!("v025-fixture-{pid}"),
                "pid": pid,
                "released": false,
                "started_at_ms": 1,
                "binary": executable,
                "data_root": data_root,
            }),
        )?;
        write_json(
            &daemon_root.join("status.json"),
            &json!({
                "schema_version": 1,
                "status": "running",
                "pid": pid,
                "started_at_ms": 1,
                "heartbeat_at_ms": 1,
                "start_mode": "auto",
                "trigger_command": "setup",
                "semantic_runtime_active": false,
            }),
        )?;
        loop {
            thread::sleep(Duration::from_secs(60));
        }
    }

    fn disable_daemon(data_root: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let lock_path = data_root.join("daemon/daemon.lock");
        let mut lock: serde_json::Value =
            serde_json::from_slice(&fs::read(&lock_path)?)?;
        let pid = lock["pid"].as_u64().ok_or("daemon lock has no pid")?;
        let pid = libc::pid_t::try_from(pid).map_err(|_| "daemon pid is invalid")?;
        if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error.into());
            }
        }
        for _ in 0..100 {
            if unsafe { libc::kill(pid, 0) } != 0 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        lock["released"] = serde_json::Value::Bool(true);
        write_json(&lock_path, &lock)?;
        Ok(())
    }

    fn daemon_status(data_root: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let lock_path = data_root.join("daemon/daemon.lock");
        let lock = fs::read(&lock_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        let running = lock.as_ref().is_some_and(|lock| {
            lock["released"] != true
                && lock["pid"]
                    .as_u64()
                    .and_then(|pid| libc::pid_t::try_from(pid).ok())
                    .is_some_and(|pid| unsafe { libc::kill(pid, 0) } == 0)
        });
        println!(
            "{}",
            json!({
                "schema_version": 1,
                "daemon": {
                    "running": running,
                },
            })
        );
        Ok(())
    }

    fn run_upgrade(data_root: &Path, candidate: &Path) -> Result<(), Box<dyn std::error::Error>> {
        fs::create_dir_all(data_root)?;
        let lock_path = data_root.join("upgrade.lock");
        acquire_legacy_upgrade_lock(&lock_path)?;
        let target = fs::canonicalize(env::current_exe()?)?;
        let parent = target.parent().ok_or("fixture target has no parent")?;
        let staged = parent.join(format!(".ctx-upgrade-{}.1.new", process::id()));
        fs::copy(candidate, &staged)?;
        let mut permissions = fs::metadata(&staged)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&staged, permissions)?;
        let output = Command::new(&staged).arg("--version").output()?;
        if !output.status.success() || !String::from_utf8_lossy(&output.stdout).contains("1.0.0") {
            return Err(format!(
                "staged v1 version probe failed (status {}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )
            .into());
        }
        if env::var_os("CTX_V025_ABORT_AFTER_PROBE_FOR_TESTS").is_some() {
            process::exit(86);
        }

        let marker_path = install_marker_path(&target);
        let staged_marker = parent.join(format!(".ctx-upgrade-{}.install.json.new", process::id()));
        write_json(
            &staged_marker,
            &json!({
                "schema_version": 1,
                "manager": "ctx-hosted-installer",
                "install_attempt_id": "ia_v025_fixture_upgrade",
                "install_path": target,
                "platform": platform_key(),
                "channel": "stable",
                "version": "1.0.0",
                "sha256": sha256_file(&staged)?,
                "installed_at": "2026-07-30T00:00:00Z",
            }),
        )?;
        let previous = target.with_file_name("ctx.previous");
        let _ = fs::remove_file(&previous);
        fs::rename(&target, &previous)?;
        fs::rename(&staged, &target)?;
        fs::rename(&staged_marker, &marker_path)?;
        fs::remove_file(lock_path)?;
        Ok(())
    }

    fn acquire_legacy_upgrade_lock(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(mut file) => {
                writeln!(file, "{} 1", process::id())?;
                file.sync_all()?;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let stale = fs::read_to_string(path)
                    .ok()
                    .and_then(|text| text.split_whitespace().next()?.parse::<u32>().ok())
                    .is_none_or(|pid| unsafe { libc::kill(pid as libc::pid_t, 0) } != 0);
                if !stale {
                    return Err("another v0.25 fixture upgrade is active".into());
                }
                fs::remove_file(path)?;
                acquire_legacy_upgrade_lock(path)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn write_json(path: &Path, value: &serde_json::Value) -> std::io::Result<()> {
        let mut file = fs::File::create(path)?;
        serde_json::to_writer_pretty(&mut file, value)?;
        file.write_all(b"\n")?;
        file.sync_all()
    }

    fn sha256_file(path: &Path) -> std::io::Result<String> {
        let bytes = fs::read(path)?;
        let digest = Sha256::digest(bytes);
        Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
    }

    fn install_marker_path(target: &Path) -> PathBuf {
        let mut name = target.file_name().unwrap().to_os_string();
        name.push(".install.json");
        target.with_file_name(name)
    }

    fn platform_key() -> &'static str {
        match (env::consts::OS, env::consts::ARCH) {
            ("linux", "x86_64") => "linux-x64",
            ("linux", "aarch64") => "linux-aarch64",
            ("macos", "x86_64") => "macos-x64",
            ("macos", "aarch64") => "macos-arm64",
            _ => "unsupported",
        }
    }
}

#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    unix::run()
}

#[cfg(not(unix))]
fn main() {
    eprintln!("the v0.25 automatic-upgrade fixture is Unix-only");
    std::process::exit(2);
}
