use assert_cmd::Command;
use serde_json::Value;
use std::{
    fs, io,
    path::{Path, PathBuf},
};
use tempfile::{Builder, TempDir, TempPath};

pub(super) const PERSISTENT_DAEMON_TEST_ROOT_MARKER: &str = ".ctx-test-owned-daemon";

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

pub(crate) fn ctx(temp: &TempDir) -> Command {
    ctx_with_enabled_daemon(temp)
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
    ctx_from_binary(temp, &binary)
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
    command.env_remove("CTX_DAEMON_AUTOSTART_OFF");
    if persistent_daemon_test {
        command.env_remove("CTX_DAEMON_AUTOSTART_IDLE_EXIT_SECONDS");
    } else {
        // Tests outside the persistent-daemon harness retain their explicit
        // finite fallback until they adopt identity-bound teardown.
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
    if target.exists() {
        return target;
    }
    let source = ctx_binary();
    copied_binary(temp, &source)
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
    // `fs::copy` closes both file handles before returning. Convert only the
    // resulting path into a TempPath so atomic publication cannot retain a
    // writable descriptor on the executable inode.
    fs::copy(source, &staged_path).unwrap();
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
