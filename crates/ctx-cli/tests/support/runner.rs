use assert_cmd::Command;
use serde_json::Value;
use std::{
    fs, io,
    path::{Path, PathBuf},
};
use tempfile::{Builder, TempDir};

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
    command.env("CTX_DATA_ROOT", temp.path());
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
    // Exercise the public enabled-by-default contract while bounding detached
    // process lifetime in tests. Production has no implicit idle exit; this is
    // the explicit finite test option retained for hermetic temporary roots.
    command.env("CTX_DAEMON_AUTOSTART_IDLE_EXIT_SECONDS", "2");
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
    command.env_remove("XDG_CONFIG_HOME");
    command.env_remove("XDG_DATA_HOME");
    command.env_remove("XDG_STATE_HOME");
    command.env_remove("LOCALAPPDATA");
    command.env_remove("APPDATA");
}

pub(crate) fn copied_ctx_binary(temp: &TempDir) -> PathBuf {
    let target = test_binary_copy_path(temp);
    if target.exists() {
        return target;
    }
    let source = ctx_binary();
    copied_binary(temp, &source)
}

fn test_binary_copy_path(temp: &TempDir) -> PathBuf {
    temp.path().join(if cfg!(windows) {
        "ctx-test-copy.exe"
    } else {
        "ctx-test-copy"
    })
}

pub(crate) fn copied_binary(temp: &TempDir, source: &Path) -> PathBuf {
    let target = test_binary_copy_path(temp);
    // Close every write handle before the caller attempts to execute the copy.
    // Some Linux filesystems otherwise expose a brief ETXTBSY window here.
    let mut source_file = fs::File::open(source).unwrap();
    let mut target_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
        .unwrap();
    io::copy(&mut source_file, &mut target_file).unwrap();
    target_file.sync_all().unwrap();
    drop(target_file);
    drop(source_file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(&target).unwrap().permissions();
        permissions.set_mode(permissions.mode() | 0o700);
        fs::set_permissions(&target, permissions).unwrap();
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
