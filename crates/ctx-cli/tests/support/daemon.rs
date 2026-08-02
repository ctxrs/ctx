use serde_json::Value;
use std::{
    fs,
    ops::Deref,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

use super::runner::{
    ctx, ctx_from_binary, data_root, json_output, tempdir, test_binary_copy_path,
    PERSISTENT_DAEMON_TEST_ROOT_MARKER,
};

const DAEMON_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const DAEMON_STOP_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// A temporary CLI root that owns every daemon started from its copied binary.
///
/// The marker makes commands use the production-persistent daemon contract.
/// Teardown first asks that exact binary to disable its daemon, then uses the
/// lock's root, owner, binary, and live-process identities before any fallback
/// signal. A PID alone is never sufficient authority.
pub(crate) struct DaemonTestRoot {
    temp: TempDir,
}

impl DaemonTestRoot {
    fn new() -> Self {
        let temp = tempdir();
        fs::write(
            temp.path().join(PERSISTENT_DAEMON_TEST_ROOT_MARKER),
            b"test-owned persistent daemon root\n",
        )
        .unwrap();
        Self { temp }
    }
}

impl Deref for DaemonTestRoot {
    type Target = TempDir;

    fn deref(&self) -> &Self::Target {
        &self.temp
    }
}

impl AsRef<Path> for DaemonTestRoot {
    fn as_ref(&self) -> &Path {
        self.temp.path()
    }
}

impl Drop for DaemonTestRoot {
    fn drop(&mut self) {
        if let Err(error) = stop_test_owned_daemon(&self.temp) {
            if thread::panicking() {
                eprintln!("test-owned daemon teardown also failed: {error}");
            } else {
                panic!("test-owned daemon teardown failed: {error}");
            }
        }
    }
}

pub(crate) fn daemon_test_root() -> DaemonTestRoot {
    DaemonTestRoot::new()
}

pub(crate) fn wait_for_test_daemon_source_refresh(temp: &TempDir) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let path = data_root(temp).join("daemon/jobs/core-refresh.json");
    loop {
        if let Ok(bytes) = fs::read(&path) {
            let job: Value = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                panic!("parse source refresh job {}: {error}", path.display())
            });
            if !matches!(job["request_state"].as_str(), Some("queued" | "running")) {
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for test daemon source refresh to become terminal"
        );
        thread::sleep(DAEMON_STOP_POLL_INTERVAL);
    }
}

pub(crate) fn wait_for_test_lexical_projection(temp: &TempDir, generation: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = json_output(ctx(temp).args(["status", "--format=json"]));
        if status["lexical"]["status"] == "ready"
            && status["lexical"]["generation_id"] == generation
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for lexical projection at generation {generation}: {status:#}"
        );
        thread::sleep(DAEMON_STOP_POLL_INTERVAL);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DaemonIdentity {
    pid: u32,
    owner_id: String,
    binary: PathBuf,
}

fn stop_test_owned_daemon(temp: &TempDir) -> Result<(), String> {
    let binary = test_binary_copy_path(temp);
    if !binary.is_file() {
        return Ok(());
    }

    let initial = read_daemon_identity(temp)?;
    if let Some(identity) = initial.as_ref() {
        if process_is_running(identity.pid) {
            verify_live_daemon_identity(temp, &binary, identity)?;
        } else if !same_file(&identity.binary, &binary)? {
            return Err(format!(
                "daemon lock binary {} is not the test copy {}",
                identity.binary.display(),
                binary.display()
            ));
        }
    }
    let output = ctx_from_binary(temp, &binary)
        .env("CTX_DAEMON_AUTOSTART_OFF", "1")
        .args(["daemon", "disable", "--format=json"])
        .output()
        .map_err(|error| format!("run daemon disable with {}: {error}", binary.display()))?;

    if let Some(identity) = initial.as_ref() {
        if wait_for_process_exit(identity.pid, DAEMON_STOP_TIMEOUT) {
            return assert_daemon_released(temp, identity);
        }
        verify_live_daemon_identity(temp, &binary, identity)?;
        terminate_process(identity.pid, false)
            .map_err(|error| format!("terminate verified test daemon {}: {error}", identity.pid))?;
        if !wait_for_process_exit(identity.pid, Duration::from_secs(1)) {
            verify_live_daemon_identity(temp, &binary, identity)?;
            terminate_process(identity.pid, true).map_err(|error| {
                format!(
                    "force-terminate verified test daemon {}: {error}",
                    identity.pid
                )
            })?;
        }
        if !wait_for_process_exit(identity.pid, DAEMON_STOP_TIMEOUT) {
            return Err(format!(
                "verified test daemon {} remained alive after teardown",
                identity.pid
            ));
        }
        return assert_daemon_released(temp, identity);
    }

    if output.status.success() {
        return remove_released_test_daemon_artifacts(temp, &binary);
    }
    Err(format!(
        "daemon disable failed without an attributable lock ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    ))
}

fn remove_released_test_daemon_artifacts(
    temp: &TempDir,
    expected_binary: &Path,
) -> Result<(), String> {
    let path = data_root(temp).join("daemon/daemon.lock");
    if let Ok(bytes) = fs::read(&path) {
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("parse released daemon lock {}: {error}", path.display()))?;
        if value["released"] != true {
            return Err(format!(
                "daemon lock is still active without an attributable live owner: {value:#}"
            ));
        }
        let recorded_data_root = value["data_root"]
            .as_str()
            .map(Path::new)
            .ok_or_else(|| format!("released daemon lock has no data root: {value:#}"))?;
        if !same_path(recorded_data_root, &data_root(temp)) {
            return Err(format!(
                "released daemon lock data root {} does not match test root {}",
                recorded_data_root.display(),
                data_root(temp).display()
            ));
        }
        let recorded_binary = value["binary"]
            .as_str()
            .map(Path::new)
            .ok_or_else(|| format!("released daemon lock has no binary: {value:#}"))?;
        if !same_file(recorded_binary, expected_binary)? {
            return Err(format!(
                "released daemon lock binary {} is not the test copy {}",
                recorded_binary.display(),
                expected_binary.display()
            ));
        }
        if value["pid"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .is_some_and(process_is_running)
        {
            return Err(format!(
                "released daemon lock still identifies a live process: {value:#}"
            ));
        }
    }
    remove_test_daemon_artifacts(temp)
}

fn read_daemon_identity(temp: &TempDir) -> Result<Option<DaemonIdentity>, String> {
    let path = data_root(temp).join("daemon/daemon.lock");
    let Some(bytes) = fs::read(&path)
        .map(Some)
        .or_else(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(error)
            }
        })
        .map_err(|error| format!("read daemon lock {}: {error}", path.display()))?
    else {
        return Ok(None);
    };
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse daemon lock {}: {error}", path.display()))?;
    if value["released"] == true {
        return Ok(None);
    }
    let pid = value["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .ok_or_else(|| format!("daemon lock has no valid pid: {value:#}"))?;
    let owner_id = value["owner_id"]
        .as_str()
        .filter(|owner| !owner.is_empty())
        .ok_or_else(|| format!("daemon lock has no owner identity: {value:#}"))?
        .to_owned();
    let recorded_data_root = value["data_root"]
        .as_str()
        .map(Path::new)
        .ok_or_else(|| format!("daemon lock has no data-root identity: {value:#}"))?;
    if !same_path(recorded_data_root, &data_root(temp)) {
        return Err(format!(
            "daemon lock data root {} does not match test root {}",
            recorded_data_root.display(),
            data_root(temp).display()
        ));
    }
    let binary = value["binary"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| format!("daemon lock has no binary identity: {value:#}"))?;
    Ok(Some(DaemonIdentity {
        pid,
        owner_id,
        binary,
    }))
}

fn verify_live_daemon_identity(
    temp: &TempDir,
    expected_binary: &Path,
    expected: &DaemonIdentity,
) -> Result<(), String> {
    let current = read_daemon_identity(temp)?
        .ok_or_else(|| "daemon lock was released while its process remained alive".to_owned())?;
    if &current != expected {
        return Err(format!(
            "daemon lock identity changed before teardown: expected {expected:?}, found {current:?}"
        ));
    }
    if !same_file(&expected.binary, expected_binary)? {
        return Err(format!(
            "daemon lock binary {} is not the test copy {}",
            expected.binary.display(),
            expected_binary.display()
        ));
    }
    let process_binary = process_executable(expected.pid).ok_or_else(|| {
        format!(
            "cannot verify executable identity for test daemon {}",
            expected.pid
        )
    })?;
    if !same_file(&process_binary, expected_binary)? {
        return Err(format!(
            "daemon {} executable {} is not the test copy {}",
            expected.pid,
            process_binary.display(),
            expected_binary.display()
        ));
    }
    Ok(())
}

fn assert_daemon_released(temp: &TempDir, expected: &DaemonIdentity) -> Result<(), String> {
    if process_is_running(expected.pid) {
        return Err(format!("test daemon {} is still running", expected.pid));
    }
    let path = data_root(temp).join("daemon/daemon.lock");
    if let Ok(bytes) = fs::read(&path) {
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("parse released daemon lock {}: {error}", path.display()))?;
        if value["owner_id"] != expected.owner_id
            || value["pid"].as_u64() != Some(u64::from(expected.pid))
        {
            return Err(format!(
                "daemon lock changed owners during teardown; refusing cleanup: {value:#}"
            ));
        }
    }
    remove_test_daemon_artifacts(temp)
}

fn assert_no_endpoint_identity(temp: &TempDir) -> Result<(), String> {
    for name in [
        "daemon.lock",
        "daemon.guard",
        "query-endpoint.json",
        "source-refresh-endpoint.json",
        "query.sock",
        "source-refresh.sock",
    ] {
        let path = data_root(temp).join("daemon").join(name);
        if path.exists() {
            return Err(format!(
                "test daemon artifact remained after teardown: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn remove_test_daemon_artifacts(temp: &TempDir) -> Result<(), String> {
    for name in [
        "query-endpoint.json",
        "source-refresh-endpoint.json",
        "query.sock",
        "source-refresh.sock",
        "daemon.lock",
        "daemon.guard",
    ] {
        let path = data_root(temp).join("daemon").join(name);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "remove verified test daemon artifact {}: {error}",
                    path.display()
                ));
            }
        }
    }
    assert_no_endpoint_identity(temp)
}

fn wait_for_process_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if !process_is_running(pid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(DAEMON_STOP_POLL_INTERVAL);
    }
}

#[cfg(unix)]
fn same_file(left: &Path, right: &Path) -> Result<bool, String> {
    use std::os::unix::fs::MetadataExt as _;

    let left = fs::metadata(left)
        .map_err(|error| format!("inspect executable identity {}: {error}", left.display()))?;
    let right = fs::metadata(right)
        .map_err(|error| format!("inspect executable identity {}: {error}", right.display()))?;
    Ok(left.dev() == right.dev() && left.ino() == right.ino())
}

#[cfg(windows)]
fn same_file(left: &Path, right: &Path) -> Result<bool, String> {
    Ok(same_path(left, right))
}

fn same_path(left: &Path, right: &Path) -> bool {
    let canonical = |path: &Path| {
        fs::canonicalize(path)
            .ok()
            .map(|path| path.to_string_lossy().to_lowercase())
    };
    matches!(
        (canonical(left), canonical(right)),
        (Some(left), Some(right)) if left == right
    )
}

#[cfg(target_os = "linux")]
fn process_executable(pid: u32) -> Option<PathBuf> {
    fs::read_link(format!("/proc/{pid}/exe")).ok()
}

#[cfg(target_os = "macos")]
fn process_executable(pid: u32) -> Option<PathBuf> {
    use std::ffi::CStr;

    const MAX_PATH_BYTES: usize = 4096;
    unsafe extern "C" {
        fn proc_pidpath(pid: libc::c_int, buffer: *mut libc::c_void, size: u32) -> libc::c_int;
    }
    let mut buffer = vec![0_u8; MAX_PATH_BYTES];
    let length = unsafe {
        proc_pidpath(
            libc::pid_t::try_from(pid).ok()?,
            buffer.as_mut_ptr().cast(),
            u32::try_from(buffer.len()).ok()?,
        )
    };
    if length <= 0 {
        return None;
    }
    CStr::from_bytes_until_nul(&buffer)
        .ok()
        .map(|path| PathBuf::from(path.to_string_lossy().into_owned()))
}

#[cfg(target_os = "freebsd")]
fn process_executable(pid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;

    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PATHNAME,
        libc::c_int::try_from(pid).ok()?,
    ];
    let mut buffer = vec![0_u8; libc::PATH_MAX as usize];
    let mut length = buffer.len();
    let status = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buffer.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 || length == 0 {
        return None;
    }
    let end = buffer[..length]
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(length);
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buffer[..end])))
}

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))
))]
fn process_executable(_pid: u32) -> Option<PathBuf> {
    None
}

#[cfg(windows)]
fn process_executable(pid: u32) -> Option<PathBuf> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    };

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut buffer = vec![0_u16; 32_768];
    let mut length = u32::try_from(buffer.len()).ok()?;
    let queried =
        unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length) } != 0;
    unsafe {
        CloseHandle(handle);
    }
    queried.then(|| PathBuf::from(String::from_utf16_lossy(&buffer[..length as usize])))
}

#[cfg(unix)]
fn process_is_running(pid: u32) -> bool {
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
fn process_is_running(pid: u32) -> bool {
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
fn terminate_process(pid: u32, force: bool) -> std::io::Result<()> {
    let pid = libc::pid_t::try_from(pid)
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
    if unsafe { libc::kill(pid, signal) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn terminate_process(pid: u32, _force: bool) -> std::io::Result<()> {
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
