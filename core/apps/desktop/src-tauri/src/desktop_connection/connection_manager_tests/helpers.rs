use super::*;

pub(super) struct EnvVarGuard {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    pub(super) fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(value) => unsafe {
                std::env::set_var(self.key, value);
            },
            None => unsafe {
                std::env::remove_var(self.key);
            },
        }
    }
}

#[cfg(unix)]
pub(super) fn spawn_detached_sleep_pid() -> u32 {
    let output = Command::new("sh")
        .arg("-c")
        .arg("sleep 30 >/dev/null 2>&1 & echo $!")
        .output()
        .expect("spawn detached sleep");
    assert!(
        output.status.success(),
        "detached sleep spawn failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .trim()
        .parse::<u32>()
        .expect("parse detached sleep pid")
}

#[cfg(unix)]
pub(super) fn pid_is_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(unix)]
pub(super) fn wait_for_pid_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !pid_is_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(80));
    }
    !pid_is_alive(pid)
}

#[cfg(unix)]
pub(super) fn wait_for_file(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(80));
    }
    path.exists()
}

#[cfg(unix)]
pub(super) fn spawn_tokio_sleep_child() -> Child {
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg("sleep 30 >/dev/null 2>&1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command.spawn().expect("spawn tokio sleep child")
}

#[cfg(unix)]
pub(super) fn spawn_term_trap_child(term_marker: &Path) -> Child {
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg("trap 'printf term > \"$CTX_TEST_TERM_MARKER\"; exit 0' TERM; while :; do sleep 1; done")
        .env("CTX_TEST_TERM_MARKER", term_marker)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command.spawn().expect("spawn term trap child")
}
