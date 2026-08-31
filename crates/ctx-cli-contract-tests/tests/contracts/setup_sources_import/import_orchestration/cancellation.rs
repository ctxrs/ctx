use super::*;
use std::io;

#[cfg(any(unix, windows))]
fn configure_interruptible_client(_command: &mut StdCommand) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;

        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        _command.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
}

#[cfg(unix)]
fn interrupt_client_group(pid: u32) -> io::Result<()> {
    let result = unsafe { libc::kill(pid as libc::pid_t, libc::SIGINT) };
    (result == 0)
        .then_some(())
        .ok_or_else(io::Error::last_os_error)
}

#[cfg(windows)]
fn interrupt_client_group(pid: u32) -> io::Result<()> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[allow(non_snake_case)]
        fn GenerateConsoleCtrlEvent(event: u32, process_group_id: u32) -> i32;
    }
    const CTRL_BREAK_EVENT: u32 = 1;

    let result = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) };
    (result != 0)
        .then_some(())
        .ok_or_else(io::Error::last_os_error)
}

#[cfg(unix)]
struct NativeProcessProbe(u32);

#[cfg(unix)]
impl NativeProcessProbe {
    fn open(pid: u32) -> io::Result<Self> {
        if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
            Ok(Self(pid))
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn assert_running(&self) {
        assert_eq!(unsafe { libc::kill(self.0 as libc::pid_t, 0) }, 0);
    }

    fn assert_exited(&self) {
        assert_eq!(unsafe { libc::kill(self.0 as libc::pid_t, 0) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    }
}

#[cfg(windows)]
struct NativeProcessProbe(std::os::windows::io::OwnedHandle);

#[cfg(windows)]
impl NativeProcessProbe {
    fn open(pid: u32) -> io::Result<Self> {
        use std::os::windows::io::FromRawHandle as _;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        };

        let handle = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(unsafe {
            std::os::windows::io::OwnedHandle::from_raw_handle(handle.cast())
        }))
    }

    fn assert_running(&self) {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::{
            Foundation::WAIT_TIMEOUT, System::Threading::WaitForSingleObject,
        };

        assert_eq!(
            unsafe { WaitForSingleObject(self.0.as_raw_handle().cast(), 0) },
            WAIT_TIMEOUT
        );
    }

    fn assert_exited(&self) {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::{
            Foundation::WAIT_OBJECT_0, System::Threading::WaitForSingleObject,
        };

        assert_eq!(
            unsafe { WaitForSingleObject(self.0.as_raw_handle().cast(), 0) },
            WAIT_OBJECT_0
        );
    }
}

#[cfg(unix)]
fn assert_private_worker_group(pid: u32) {
    assert_eq!(
        unsafe { libc::getpgid(pid as libc::pid_t) },
        pid as libc::pid_t
    );
}

#[cfg(windows)]
fn assert_private_worker_group(_pid: u32) {
    // The native low-level contract proves CREATE_NEW_PROCESS_GROUP and
    // targeted CTRL_BREAK delivery. This final-binary contract proves that
    // the separately grouped client can interrupt and reap only its worker.
}

#[cfg(any(unix, windows))]
fn assert_blocked_owned_wait_exits_130(arguments: &[&str], capability_request: bool) {
    let temp = tempdir();
    let root = data_root(&temp);
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("config.toml"),
        "[indexing]\nmode = \"manual\"\n\n[search]\nsemantic = false\n",
    )
    .unwrap();
    write_codex_setup_session(&temp);

    let daemon_gate = root.join(".block-daemon-main-after-ready-for-test");
    let daemon_blocked = root.join(".daemon-main-blocked-after-ready-for-test");
    let refresh_gate = root.join(".block-source-refresh-after-availability-for-test");
    let refresh_blocked = root.join(".source-refresh-blocked-after-availability-for-test");
    fs::write(&daemon_gate, b"block\n").unwrap();
    fs::write(&refresh_gate, b"block\n").unwrap();

    let prepared = ctx(&temp);
    let mut command = StdCommand::new(prepared.get_program());
    for (name, value) in prepared.get_envs() {
        match value {
            Some(value) => {
                command.env(name, value);
            }
            None => {
                command.env_remove(name);
            }
        }
    }
    command
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if capability_request {
        command.stdin(Stdio::piped());
    }
    configure_interruptible_client(&mut command);
    let mut client = SourceRefreshDaemon {
        child: Some(command.spawn().expect("start blocked finite import")),
    };
    if capability_request {
        let request = json!({
            "data_root": root.clone(),
            "operation": "RefreshAndWait",
            "options": {},
            "protocol_version": 3,
            "schema_version": 1,
        });
        let mut input = client
            .child
            .as_mut()
            .unwrap()
            .stdin
            .take()
            .expect("hidden capability stdin");
        serde_json::to_writer(&mut input, &request).unwrap();
        input.write_all(b"\n").unwrap();
    }
    let client_pid = client.child.as_ref().unwrap().id();

    let marker_deadline = Instant::now() + Duration::from_secs(15);
    while !(daemon_blocked.exists() && refresh_blocked.exists()) {
        if let Some(status) = client.child.as_mut().unwrap().try_wait().unwrap() {
            panic!("finite import exited before its cancellation gates: {status}");
        }
        assert!(
            Instant::now() < marker_deadline,
            "finite import did not reach both cancellation gates"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    let lock_path = root.join("daemon/daemon.lock");
    let lock: Value = serde_json::from_slice(&fs::read(&lock_path).unwrap()).unwrap();
    let worker_pid = u32::try_from(lock["pid"].as_u64().expect("finite worker pid")).unwrap();
    assert_ne!(worker_pid, client_pid);
    assert_private_worker_group(worker_pid);
    let worker = NativeProcessProbe::open(worker_pid).expect("open finite worker process probe");
    worker.assert_running();
    assert!(root.join("daemon/source-refresh-endpoint.json").exists());

    interrupt_client_group(client_pid).expect("interrupt finite import process group");
    let exit_deadline = Instant::now() + Duration::from_secs(8);
    let status = loop {
        if let Some(status) = client.child.as_mut().unwrap().try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < exit_deadline,
            "interrupted finite import did not exit within the reap bound"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let output = client.child.take().unwrap().wait_with_output().unwrap();

    assert_eq!(
        status.code(),
        Some(130),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status, status);
    assert!(
        output.stdout.is_empty(),
        "stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.stderr.is_empty(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        daemon_gate.exists(),
        "test released the daemon gate unexpectedly"
    );
    assert!(
        refresh_gate.exists(),
        "test released the refresh gate unexpectedly"
    );
    worker.assert_exited();
    assert!(!root.join("daemon/source-refresh-endpoint.json").exists());
    let released: Value = serde_json::from_slice(&fs::read(lock_path).unwrap()).unwrap();
    assert_eq!(released["pid"], worker_pid);
    assert_eq!(released["released"], true, "{released:#}");

    for path in [daemon_gate, daemon_blocked, refresh_gate, refresh_blocked] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("remove finite cancellation gate: {error}"),
        }
    }
}

#[cfg(any(unix, windows))]
#[test]
fn blocked_import_sigint_exits_130_and_reaps_only_its_finite_worker() {
    for arguments in [
        &["import", "--all", "--progress", "none"][..],
        &["import", "--all", "--format=json", "--progress", "none"],
        &["import", "--all", "--progress", "none", "--quiet"],
    ] {
        assert_blocked_owned_wait_exits_130(arguments, false);
    }
}

#[cfg(any(unix, windows))]
#[test]
fn blocked_search_wait_sigint_exits_130_and_reaps_only_its_finite_worker() {
    for arguments in [
        &[
            "search",
            "setup should import",
            "--provider=codex",
            "--refresh=wait",
        ][..],
        &[
            "search",
            "setup should import",
            "--provider=codex",
            "--refresh=wait",
            "--format=json",
        ],
        &[
            "search",
            "setup should import",
            "--provider=codex",
            "--refresh=wait",
            "--quiet",
        ],
    ] {
        assert_blocked_owned_wait_exits_130(arguments, false);
    }
}

#[cfg(any(unix, windows))]
#[test]
fn blocked_hidden_refresh_and_wait_exits_130_and_reaps_only_its_finite_worker() {
    assert_blocked_owned_wait_exits_130(&["--ctx-core-capability-v1"], true);
}

#[cfg(any(unix, windows))]
#[test]
fn interrupted_search_joiner_leaves_existing_finite_owner_untouched() {
    let temp = tempdir();
    let root = data_root(&temp);
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("config.toml"),
        "[indexing]\nmode = \"manual\"\n\n[search]\nsemantic = false\n",
    )
    .unwrap();
    write_codex_setup_session(&temp);

    let daemon_gate = root.join(".block-daemon-main-after-ready-for-test");
    let daemon_blocked = root.join(".daemon-main-blocked-after-ready-for-test");
    let refresh_gate = root.join(".block-source-refresh-after-availability-for-test");
    let refresh_blocked = root.join(".source-refresh-blocked-after-availability-for-test");
    fs::write(&daemon_gate, b"block\n").unwrap();
    fs::write(&refresh_gate, b"block\n").unwrap();

    let spawn = |args: &[&str]| {
        let prepared = ctx(&temp);
        let mut command = StdCommand::new(prepared.get_program());
        for (name, value) in prepared.get_envs() {
            match value {
                Some(value) => {
                    command.env(name, value);
                }
                None => {
                    command.env_remove(name);
                }
            }
        }
        command
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_interruptible_client(&mut command);
        SourceRefreshDaemon {
            child: Some(command.spawn().expect("start blocked foreground client")),
        }
    };
    let mut owner = spawn(&[
        "import",
        "--all",
        "--format=json",
        "--progress",
        "none",
        "--quiet",
    ]);
    let owner_pid = owner.child.as_ref().unwrap().id();
    let marker_deadline = Instant::now() + Duration::from_secs(15);
    while !(daemon_blocked.exists()
        && fs::read_to_string(&refresh_blocked)
            .is_ok_and(|pid| pid.trim() == owner_pid.to_string()))
    {
        if let Some(status) = owner.child.as_mut().unwrap().try_wait().unwrap() {
            panic!("finite owner exited before its joiner arrived: {status}");
        }
        assert!(
            Instant::now() < marker_deadline,
            "finite owner did not reach both shared-owner gates"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    let lock_path = root.join("daemon/daemon.lock");
    let lock: Value = serde_json::from_slice(&fs::read(&lock_path).unwrap()).unwrap();
    let worker_pid = u32::try_from(lock["pid"].as_u64().expect("finite worker pid")).unwrap();
    let worker = NativeProcessProbe::open(worker_pid).expect("open shared finite worker probe");
    let mut joiner = spawn(&[
        "search",
        "setup should import",
        "--provider=codex",
        "--refresh=wait",
        "--format=json",
        "--quiet",
    ]);
    let joiner_pid = joiner.child.as_ref().unwrap().id();
    let join_deadline = Instant::now() + Duration::from_secs(15);
    while !fs::read_to_string(&refresh_blocked)
        .is_ok_and(|pid| pid.trim() == joiner_pid.to_string())
    {
        if let Some(status) = joiner.child.as_mut().unwrap().try_wait().unwrap() {
            panic!("search joiner exited before its cancellation gate: {status}");
        }
        assert!(
            Instant::now() < join_deadline,
            "search joiner did not join the existing finite owner"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    interrupt_client_group(joiner_pid).expect("interrupt joined search process group");
    let exit_deadline = Instant::now() + Duration::from_secs(8);
    let status = loop {
        if let Some(status) = joiner.child.as_mut().unwrap().try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < exit_deadline,
            "interrupted search joiner did not exit within the cancellation bound"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let output = joiner.child.take().unwrap().wait_with_output().unwrap();
    assert_eq!(status.code(), Some(130));
    assert_eq!(output.status, status);
    assert!(
        output.stdout.is_empty(),
        "stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.stderr.is_empty(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    worker.assert_running();
    assert_private_worker_group(worker_pid);
    assert!(owner.child.as_mut().unwrap().try_wait().unwrap().is_none());
    let retained: Value = serde_json::from_slice(&fs::read(&lock_path).unwrap()).unwrap();
    assert_eq!(retained["pid"], worker_pid);
    assert_eq!(retained["released"], false, "{retained:#}");
    assert!(root.join("daemon/source-refresh-endpoint.json").exists());

    fs::remove_file(&daemon_gate).unwrap();
    fs::remove_file(&refresh_gate).unwrap();
    let owner_deadline = Instant::now() + Duration::from_secs(25);
    let owner_status = loop {
        if let Some(status) = owner.child.as_mut().unwrap().try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < owner_deadline,
            "finite owner did not finish after its gates were released"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let owner_output = owner.child.take().unwrap().wait_with_output().unwrap();
    assert_eq!(owner_output.status, owner_status);
    assert!(
        owner_status.success(),
        "owner stderr={}",
        String::from_utf8_lossy(&owner_output.stderr)
    );
    let stopped = wait_for_daemon_status(&temp, "disabled", false, "import");
    assert_eq!(stopped["daemon"]["running"], false, "{stopped:#}");
    assert!(!root.join("daemon/source-refresh-endpoint.json").exists());
    worker.assert_exited();
}

#[cfg(any(unix, windows))]
#[test]
fn interrupted_search_joiner_leaves_persistent_daemon_untouched() {
    let temp = tempdir();
    let mut daemon = start_full_source_refresh_daemon(&temp);
    let daemon_pid = daemon.child.as_ref().unwrap().id();
    let daemon_process =
        NativeProcessProbe::open(daemon_pid).expect("open persistent daemon process probe");
    let root = data_root(&temp);
    let refresh_gate = root.join(".block-source-refresh-after-availability-for-test");
    let refresh_blocked = root.join(".source-refresh-blocked-after-availability-for-test");
    fs::write(&refresh_gate, b"block\n").unwrap();

    let prepared = ctx(&temp);
    let mut command = StdCommand::new(prepared.get_program());
    for (name, value) in prepared.get_envs() {
        match value {
            Some(value) => {
                command.env(name, value);
            }
            None => {
                command.env_remove(name);
            }
        }
    }
    command
        .args([
            "search",
            "persistent joined cancellation oracle",
            "--refresh=wait",
            "--format=json",
            "--quiet",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_interruptible_client(&mut command);
    let mut joiner = SourceRefreshDaemon {
        child: Some(command.spawn().expect("start persistent-daemon joiner")),
    };
    let joiner_pid = joiner.child.as_ref().unwrap().id();
    let marker_deadline = Instant::now() + Duration::from_secs(15);
    while !fs::read_to_string(&refresh_blocked)
        .is_ok_and(|pid| pid.trim() == joiner_pid.to_string())
    {
        if let Some(status) = joiner.child.as_mut().unwrap().try_wait().unwrap() {
            panic!("persistent-daemon joiner exited before cancellation: {status}");
        }
        assert!(
            Instant::now() < marker_deadline,
            "search did not reach the persistent-daemon join gate"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    interrupt_client_group(joiner_pid).expect("interrupt persistent-daemon search joiner");
    let exit_deadline = Instant::now() + Duration::from_secs(8);
    let status = loop {
        if let Some(status) = joiner.child.as_mut().unwrap().try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < exit_deadline,
            "persistent-daemon search joiner did not exit within the cancellation bound"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let output = joiner.child.take().unwrap().wait_with_output().unwrap();
    assert_eq!(status.code(), Some(130));
    assert_eq!(output.status, status);
    assert!(
        output.stdout.is_empty(),
        "stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.stderr.is_empty(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    daemon_process.assert_running();
    assert!(daemon.child.as_mut().unwrap().try_wait().unwrap().is_none());
    let lock: Value = serde_json::from_slice(&fs::read(root.join("daemon/daemon.lock")).unwrap())
        .expect("persistent daemon lock JSON");
    assert_eq!(lock["pid"], daemon_pid);
    assert_eq!(lock["released"], false, "{lock:#}");
    let endpoint: Value = serde_json::from_slice(
        &fs::read(root.join("daemon/source-refresh-endpoint.json")).unwrap(),
    )
    .expect("persistent daemon endpoint JSON");
    assert_eq!(endpoint["pid"], daemon_pid, "{endpoint:#}");

    fs::remove_file(refresh_gate).unwrap();
    match fs::remove_file(refresh_blocked) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => panic!("remove persistent join marker: {error}"),
    }
}
