use super::*;

pub(super) fn wait_for_process_state(
    pid: u32,
    expected_running: bool,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let running = process_is_running(pid);
        if running == expected_running {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "pid {pid} running={running}, expected {expected_running}"
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(unix)]
pub(super) fn process_is_running(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    let running = unsafe { libc::kill(pid, 0) } == 0;
    #[cfg(target_os = "linux")]
    if running {
        return fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| stat.rsplit_once(") ").map(|(_, rest)| rest.to_owned()))
            .and_then(|rest| rest.split_whitespace().next().map(str::to_owned))
            .is_some_and(|state| state != "Z");
    }
    running
}

#[cfg(windows)]
pub(super) fn process_is_running(pid: u32) -> bool {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, STILL_ACTIVE},
        System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return false;
    }
    let mut exit_code = 0;
    let queried = unsafe { GetExitCodeProcess(handle, &mut exit_code) } != 0;
    unsafe {
        CloseHandle(handle);
    }
    queried && exit_code == STILL_ACTIVE as u32
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
pub(super) enum ShutdownSignal {
    Interrupt,
    Terminate,
}

#[cfg(unix)]
pub(super) fn request_shutdown(pid: u32, signal: ShutdownSignal) -> std::io::Result<()> {
    let pid = libc::pid_t::try_from(pid)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "pid exceeds pid_t"))?;
    let signal = match signal {
        ShutdownSignal::Interrupt => libc::SIGINT,
        ShutdownSignal::Terminate => libc::SIGTERM,
    };
    if unsafe { libc::kill(pid, signal) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
pub(super) fn request_graceful_shutdown(pid: u32) -> std::io::Result<()> {
    request_shutdown(pid, ShutdownSignal::Terminate)
}

#[cfg(unix)]
pub(super) fn terminate_process(pid: u32) -> std::io::Result<()> {
    let pid = libc::pid_t::try_from(pid)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "pid exceeds pid_t"))?;
    if unsafe { libc::kill(pid, libc::SIGKILL) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(windows)]
pub(super) fn terminate_process(pid: u32) -> std::io::Result<()> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE},
    };

    let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    let terminated = unsafe { TerminateProcess(handle, 137) } != 0;
    unsafe {
        CloseHandle(handle);
    }
    if terminated {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
pub(super) fn assert_single_daemon_process(harness: &Harness, expected_pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let pids = linux_daemon_processes(harness);
        if pids == vec![expected_pid] {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "concurrent triggers did not deduplicate to pid {expected_pid}: {pids:?}"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) fn assert_single_daemon_process(_harness: &Harness, expected_pid: u32) {
    assert!(
        process_is_running(expected_pid),
        "deduplicated daemon owner {expected_pid} is not running"
    );
}

#[cfg(target_os = "linux")]
pub(super) fn linux_daemon_processes(harness: &Harness) -> Vec<u32> {
    let expected_binary = fs::canonicalize(&harness.binary).unwrap();
    let expected_root = harness.root().as_os_str().as_encoded_bytes();
    let mut pids = Vec::new();
    for entry in fs::read_dir("/proc").unwrap() {
        let entry = entry.unwrap();
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(command_line) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let args = command_line
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .collect::<Vec<_>>();
        let binary_matches = args
            .first()
            .and_then(|arg| std::str::from_utf8(arg).ok())
            .and_then(|arg| fs::canonicalize(arg).ok())
            .is_some_and(|binary| binary == expected_binary);
        let root_matches = args.contains(&expected_root);
        let daemon_run = args
            .windows(2)
            .any(|args| args[0] == b"daemon" && args[1] == b"run");
        if binary_matches && root_matches && daemon_run && process_is_running(pid) {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    pids
}

#[cfg(target_os = "linux")]
pub(super) fn linux_process_cpu_ticks(pid: u32) -> u64 {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))
        .unwrap_or_else(|error| panic!("read Linux process stat for {pid}: {error}"));
    let (_, fields) = stat
        .rsplit_once(") ")
        .unwrap_or_else(|| panic!("invalid Linux process stat for {pid}: {stat}"));
    let fields = fields.split_whitespace().collect::<Vec<_>>();
    let user_ticks = fields
        .get(11)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_else(|| panic!("missing Linux utime for {pid}: {stat}"));
    let system_ticks = fields
        .get(12)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_else(|| panic!("missing Linux stime for {pid}: {stat}"));
    user_ticks.saturating_add(system_ticks)
}

#[cfg(target_os = "linux")]
pub(super) fn write_linux_idle_receipt(root: &Path, receipt: &Value) {
    let directory = root.join("daemon/qualification");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("linux-idle-soak.json"),
        serde_json::to_vec_pretty(receipt).unwrap(),
    )
    .unwrap();
}
