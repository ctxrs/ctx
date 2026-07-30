use assert_cmd::Command;
use serde_json::Value;
use std::{
    fs, io,
    ops::Deref,
    path::{Path, PathBuf},
    process::Child,
    thread,
    time::{Duration, Instant},
};
use tempfile::{Builder, TempDir, TempPath};

pub(super) const PERSISTENT_DAEMON_TEST_ROOT_MARKER: &str = ".ctx-test-owned-daemon";
const BOUND_CTX_BINARY_TEST_ROOT_MARKER: &str = ".ctx-test-bound-binary";
const READY_CTX_BINARY_TEST_ROOT_MARKER: &str = ".ctx-test-copy-ready";
const FINITE_DAEMON_TEST_ROOT_MARKER: &str = ".ctx-test-daemon-idle-seconds";
const FINITE_DAEMON_IDLE_EXIT_SECONDS: &str = "600";
const FINITE_DAEMON_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const FINITE_DAEMON_STOP_POLL_INTERVAL: Duration = Duration::from_millis(20);

pub(crate) fn tempdir() -> TempDir {
    let temp_root = fs::canonicalize(std::env::temp_dir())
        .expect("system temporary directory should be canonicalizable");
    let temp = Builder::new()
        .prefix("ctx-search-mvp-")
        .tempdir_in(temp_root)
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    }
    temp
}

/// A temporary root whose commands may autostart one copied, finite-idle daemon.
///
/// The copied binary remains the daemon's ownership identity for every command
/// in the root. Teardown asks that exact binary to disable its daemon and waits
/// for production status to report that the owned process has exited.
pub(crate) struct FiniteDaemonTestRoot {
    temp: TempDir,
}

impl FiniteDaemonTestRoot {
    fn new() -> Self {
        let temp = tempdir();
        mark_finite_daemon_test_root(&temp);
        Self { temp }
    }
}

impl Deref for FiniteDaemonTestRoot {
    type Target = TempDir;

    fn deref(&self) -> &Self::Target {
        &self.temp
    }
}

impl AsRef<Path> for FiniteDaemonTestRoot {
    fn as_ref(&self) -> &Path {
        self.temp.path()
    }
}

impl Drop for FiniteDaemonTestRoot {
    fn drop(&mut self) {
        if let Err(error) = stop_finite_test_owned_daemon(&self.temp) {
            if thread::panicking() {
                eprintln!("finite test-owned daemon teardown also failed: {error}");
            } else {
                panic!("finite test-owned daemon teardown failed: {error}");
            }
        }
    }
}

pub(crate) fn finite_daemon_test_root() -> FiniteDaemonTestRoot {
    FiniteDaemonTestRoot::new()
}

pub(crate) fn mark_finite_daemon_test_root(temp: &TempDir) {
    bind_test_ctx_binary(temp);
    fs::write(
        temp.path().join(PERSISTENT_DAEMON_TEST_ROOT_MARKER),
        b"test-owned finite daemon root\n",
    )
    .unwrap();
    fs::write(
        temp.path().join(FINITE_DAEMON_TEST_ROOT_MARKER),
        format!("{FINITE_DAEMON_IDLE_EXIT_SECONDS}\n"),
    )
    .unwrap();
}

/// Bind this root's future `ctx(temp)` commands to one copied executable.
///
/// This changes binary selection only. Ordinary hermetic environment,
/// analytics, local-usage, and daemon-autostart policy remain unchanged.
pub(crate) fn bind_test_ctx_binary(temp: &TempDir) -> PathBuf {
    let binary = copied_ctx_binary(temp);
    fs::write(
        temp.path().join(BOUND_CTX_BINARY_TEST_ROOT_MARKER),
        b"test-owned copied ctx binary\n",
    )
    .unwrap();
    binary
}

pub(crate) fn ctx(temp: &TempDir) -> Command {
    let persistent_daemon_test = temp
        .path()
        .join(PERSISTENT_DAEMON_TEST_ROOT_MARKER)
        .is_file();
    let bound_test_binary = temp
        .path()
        .join(BOUND_CTX_BINARY_TEST_ROOT_MARKER)
        .is_file();
    let binary = if persistent_daemon_test || bound_test_binary {
        copied_ctx_binary(temp)
    } else {
        ctx_binary()
    };
    let mut command = Command::new(binary);
    apply_hermetic_env(&mut command, temp);
    command
}

pub(crate) fn data_root(temp: &TempDir) -> PathBuf {
    temp.path().join("ctx-data")
}

fn ctx_binary() -> PathBuf {
    let program = PathBuf::from(Command::cargo_bin("ctx").unwrap().get_program());
    if program.is_absolute() {
        program
    } else {
        std::env::current_dir().unwrap().join(program)
    }
}

pub(crate) fn ctx_from_binary(temp: &TempDir, binary: &Path) -> Command {
    let mut command = Command::new(binary);
    apply_hermetic_env(&mut command, temp);
    command
}

pub(crate) fn ctx_with_enabled_daemon(temp: &TempDir) -> Command {
    let binary = copied_ctx_binary(temp);
    let mut command = ctx_from_binary(temp, &binary);
    command.env_remove("CTX_DAEMON_AUTOSTART_OFF");
    command
}

pub(crate) fn apply_hermetic_env(command: &mut Command, temp: &TempDir) {
    let persistent_daemon_test = temp
        .path()
        .join(PERSISTENT_DAEMON_TEST_ROOT_MARKER)
        .is_file();
    command.env("CTX_DATA_ROOT", data_root(temp));
    command.env("HOME", temp.path());
    command.env("CTX_ANALYTICS_ENABLED", "false");
    // Existing integration tests do not exercise local usage unless they opt in
    // explicitly. This keeps their temporary roots and output expectations stable.
    command.env("CTX_LOCAL_USAGE_ENABLED", "false");
    for name in [
        "CTX_ANALYTICS_OFF",
        "CTX_DISABLE_ANALYTICS",
        "CTX_INSTALL_DIAGNOSTICS_OFF",
        "CTX_DAEMON_OFF",
        "CTX_DISABLE_DAEMON",
        "CTX_UPGRADE_OFF",
        "CTX_DISABLE_AUTO_UPGRADE",
    ] {
        command.env_remove(name);
    }
    command.env_remove("CTX_DAEMON_ENABLED");
    if persistent_daemon_test {
        command.env_remove("CTX_DAEMON_AUTOSTART_OFF");
        let finite_idle_path = temp.path().join(FINITE_DAEMON_TEST_ROOT_MARKER);
        match fs::read_to_string(&finite_idle_path) {
            Ok(seconds) => {
                command.env("CTX_DAEMON_AUTOSTART_IDLE_EXIT_SECONDS", seconds.trim());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                command.env_remove("CTX_DAEMON_AUTOSTART_IDLE_EXIT_SECONDS");
            }
            Err(error) => {
                panic!(
                    "read finite daemon test marker {}: {error}",
                    finite_idle_path.display()
                );
            }
        }
    } else {
        // Ordinary integration commands are process-local. Tests that
        // explicitly remove this override retain a finite fallback until they
        // adopt the identity-bound persistent-daemon harness.
        command.env("CTX_DAEMON_AUTOSTART_OFF", "1");
        command.env("CTX_DAEMON_AUTOSTART_IDLE_EXIT_SECONDS", "2");
    }
    command.env_remove("CTX_QUIET");
    // Tests set CI explicitly when they need CI-only behavior.
    command.env_remove("CI");
    command.env_remove("GITHUB_ACTIONS");
    command.env_remove("BUILDKITE");
    command.env_remove("BUILDKITE_BUILD_ID");
    // Drop provider override variables inherited from the developer
    // machine so discovery never escapes the temp directory.
    command.env_remove("OPENCLAW_STATE_DIR");
    command.env_remove("HERMES_HOME");
    command.env_remove("ASTRBOT_ROOT");
    command.env_remove("SHELLEY_DB");
    command.env_remove("KILO_DB");
    command.env_remove("MIMOCODE_HOME");
    command.env_remove("MIMOCODE_CONFIG_DIR");
    command.env_remove("MIMOCODE_DB");
    command.env_remove("MIMOCODE_DISABLE_CHANNEL_DB");
    command.env_remove("FORGE_CONFIG");
    command.env_remove("VIBE_HOME");
    command.env_remove("CODEX_HOME");
    command.env_remove("CLAUDE_CONFIG_DIR");
    command.env_remove("COPILOT_HOME");
    if persistent_daemon_test {
        let xdg_config = temp.path().join(".config");
        let xdg_data = temp.path().join(".local/share");
        let xdg_state = temp.path().join(".local/state");
        let windows_local = temp.path().join("AppData/Local");
        let windows_roaming = temp.path().join("AppData/Roaming");
        let process_temp = temp.path().join("tmp");
        for path in [
            &xdg_config,
            &xdg_data,
            &xdg_state,
            &windows_local,
            &windows_roaming,
            &process_temp,
        ] {
            fs::create_dir_all(path).unwrap();
        }
        command.env("CTX_PRO_DATA_ROOT", temp.path().join("pro"));
        command.env("USERPROFILE", temp.path());
        command.env("XDG_CONFIG_HOME", &xdg_config);
        command.env("XDG_DATA_HOME", &xdg_data);
        command.env("XDG_STATE_HOME", &xdg_state);
        command.env("LOCALAPPDATA", &windows_local);
        command.env("APPDATA", &windows_roaming);
        command.env("TMPDIR", &process_temp);
        command.env("TEMP", &process_temp);
        command.env("TMP", &process_temp);
        command.env("CTX_MACHINE_ID", "ctx-hermetic-test-machine");
        for name in [
            "CLINE_DATA_DIR",
            "CLINE_DB_DATA_DIR",
            "CLINE_DIR",
            "CLINE_SANDBOX",
            "CLINE_SANDBOX_DATA_DIR",
            "CLINE_SESSION_DATA_DIR",
            "CODEBUDDY_CONFIG_DIR",
            "CONTINUE_GLOBAL_DIR",
            "CRUSH_GLOBAL_CONFIG",
            "CRUSH_GLOBAL_DATA",
            "CURSOR_DATA_DIR",
            "FILE_STORE",
            "FILE_STORE_PATH",
            "FLATPAK_XDG_DATA_HOME",
            "GEMINI_CLI_HOME",
            "GOOSE_PATH_ROOT",
            "JUNIE_HOME",
            "KIMI_CODE_HOME",
            "KIRO_HOME",
            "MUX_ROOT",
            "OH_PERSISTENCE_DIR",
            "OPENCLAW_HOME",
            "OPENHANDS_CONVERSATIONS_DIR",
            "OPENHANDS_PERSISTENCE_DIR",
            "OPENHANDS_USER_ID",
            "OPENCODE_DB",
            "PI_CODING_AGENT_DIR",
            "PI_CODING_AGENT_SESSION_DIR",
            "QODER_CONFIG_DIR",
            "QWEN_CODE_SYSTEM_DEFAULTS_PATH",
            "QWEN_CODE_SYSTEM_SETTINGS_PATH",
            "QWEN_CODE_TRUSTED_FOLDERS_PATH",
            "QWEN_HOME",
            "QWEN_RUNTIME_DIR",
            "SHARED_EVENT_STORAGE_PROVIDER",
            "VIBE_SESSION_LOGGING",
            "VIBE_SESSION_LOGGING__SAVE_DIR",
            "ZED_STATELESS",
            "CTX_HISTORY_PLUGIN_PATH",
            "CTX_PRO_HELPER",
            "CTX_RUNTIME_DIR",
            "CTX_SEARCH_SEMANTIC",
            "CTX_SEMANTIC_MODEL_ONNX",
            "DBUS_SESSION_BUS_ADDRESS",
        ] {
            command.env_remove(name);
        }
    } else {
        command.env_remove("XDG_CONFIG_HOME");
        command.env_remove("XDG_DATA_HOME");
        command.env_remove("XDG_STATE_HOME");
        command.env_remove("LOCALAPPDATA");
        command.env_remove("APPDATA");
    }
}

pub(crate) fn copied_ctx_binary(temp: &TempDir) -> PathBuf {
    let target = test_binary_copy_path(temp);
    if !target.exists() {
        let source = ctx_binary();
        copied_binary(temp, &source);
    }
    ensure_copied_ctx_binary_is_executable(temp, &target);
    target
}

fn ensure_copied_ctx_binary_is_executable(temp: &TempDir, target: &Path) {
    let ready = temp.path().join(READY_CTX_BINARY_TEST_ROOT_MARKER);
    if ready.is_file() {
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match std::process::Command::new(target).arg("--version").output() {
            Ok(output) if output.status.success() => {
                fs::write(&ready, b"test-owned copied ctx binary is executable\n").unwrap();
                return;
            }
            Ok(output) => {
                panic!(
                    "copied ctx binary {} failed its readiness probe: {}",
                    target.display(),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Err(error)
                if cfg!(unix) && error.raw_os_error() == Some(26) && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                panic!(
                    "copied ctx binary {} failed its readiness probe: {error}",
                    target.display()
                );
            }
        }
    }
}

pub(super) fn test_binary_copy_path(temp: &TempDir) -> PathBuf {
    temp.path().join(if cfg!(windows) {
        "ctx-test-copy.exe"
    } else {
        "ctx-test-copy"
    })
}

pub(crate) fn copied_binary(temp: &TempDir, source: &Path) -> PathBuf {
    let target = test_binary_copy_path(temp);
    if target.exists() {
        return target;
    }
    // Publish the executable only after every write is complete. Multiple
    // commands in one test may race to request the copy; none may observe the
    // final path while a peer still has that inode open for writing.
    let stage_dir = Builder::new()
        .prefix(".ctx-test-copy-stage-")
        .tempdir_in(temp.path())
        .unwrap();
    let staged_path = stage_dir.path().join(if cfg!(windows) {
        "ctx-test-copy.exe"
    } else {
        "ctx-test-copy"
    });
    {
        let mut source_file = fs::File::open(source).unwrap();
        let mut staged_file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged_path)
            .unwrap();
        io::copy(&mut source_file, &mut staged_file).unwrap();
        staged_file.sync_all().unwrap();
        // Keep these handles lexically scoped: the inode is not renamed to its
        // executable path until every writer and source handle is closed.
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(&staged_path).unwrap().permissions();
        permissions.set_mode(permissions.mode() | 0o700);
        fs::set_permissions(&staged_path, permissions).unwrap();
    }
    let staged = TempPath::try_from_path(staged_path).unwrap();
    match staged.persist_noclobber(&target) {
        Ok(()) => {}
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
            drop(error.path);
        }
        Err(error) => {
            panic!(
                "publish copied test binary {}: {}",
                target.display(),
                error.error
            );
        }
    }
    target
}

fn stop_finite_test_owned_daemon(temp: &TempDir) -> Result<(), String> {
    let binary = test_binary_copy_path(temp);
    if !binary.is_file() {
        return Ok(());
    }
    let output = ctx_from_binary(temp, &binary)
        .env("CTX_DAEMON_AUTOSTART_OFF", "1")
        .args(["daemon", "disable", "--format=json"])
        .output()
        .map_err(|error| format!("run daemon disable with {}: {error}", binary.display()))?;
    if !output.status.success() {
        return Err(format!(
            "daemon disable with {} failed ({}): {}",
            binary.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let deadline = Instant::now() + FINITE_DAEMON_STOP_TIMEOUT;
    loop {
        let status = ctx_from_binary(temp, &binary)
            .env("CTX_DAEMON_AUTOSTART_OFF", "1")
            .args(["daemon", "status", "--format=json"])
            .output()
            .map_err(|error| {
                format!("inspect disabled daemon with {}: {error}", binary.display())
            })?;
        if status.status.success() {
            let packet: Value = serde_json::from_slice(&status.stdout).map_err(|error| {
                format!(
                    "parse disabled daemon status from {}: {error}",
                    binary.display()
                )
            })?;
            if packet["daemon"]["running"] != true {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "daemon owned by {} remained active after disable: {}",
                binary.display(),
                String::from_utf8_lossy(&status.stderr)
            ));
        }
        thread::sleep(FINITE_DAEMON_STOP_POLL_INTERVAL);
    }
}

pub(crate) fn terminate_and_reap_test_child(
    child: &mut Option<Child>,
    description: &str,
) -> Result<Option<u32>, String> {
    let Some(mut child) = child.take() else {
        return Ok(None);
    };
    let pid = child.id();
    if child
        .try_wait()
        .map_err(|error| format!("inspect {description} {pid}: {error}"))?
        .is_none()
    {
        if let Err(kill_error) = child.kill() {
            if child
                .try_wait()
                .map_err(|error| {
                    format!(
                        "inspect {description} {pid} after termination failed ({kill_error}): {error}"
                    )
                })?
                .is_none()
            {
                return Err(format!("terminate {description} {pid}: {kill_error}"));
            }
            return Ok(Some(pid));
        }
        child
            .wait()
            .map_err(|error| format!("reap {description} {pid}: {error}"))?;
    }
    Ok(Some(pid))
}

pub(crate) fn hosted_install_marker_path(binary: &Path) -> PathBuf {
    let mut marker = binary.as_os_str().to_owned();
    marker.push(".install.json");
    PathBuf::from(marker)
}

pub(crate) fn initialize_empty_store(temp: &TempDir) {
    fs::create_dir_all(temp.path().join(".codex").join("sessions")).unwrap();
    ctx_with_enabled_daemon(temp)
        .args(["setup", "--catalog-only", "--progress", "none"])
        .assert()
        .success();
}

pub(crate) fn initialize_empty_store_with_env(
    temp: &TempDir,
    data_root: &Path,
    home: &Path,
    state: &Path,
) {
    fs::create_dir_all(home.join(".codex").join("sessions")).unwrap();
    ctx(temp)
        .args(["setup", "--catalog-only", "--progress", "none"])
        .env("CTX_DATA_ROOT", data_root)
        .env("HOME", home)
        .env("XDG_STATE_HOME", state)
        .env("LOCALAPPDATA", state)
        .assert()
        .success();
}

pub(crate) fn file_url(path: &Path) -> String {
    format!("file://{}", path.display())
}

pub(crate) fn json_output(command: &mut Command) -> Value {
    let output = command.assert().success().get_output().stdout.clone();
    serde_json::from_slice(&output).unwrap()
}

pub(crate) fn failure_stderr(command: &mut Command) -> String {
    let stderr = command.assert().failure().get_output().stderr.clone();
    String::from_utf8(stderr).unwrap()
}
