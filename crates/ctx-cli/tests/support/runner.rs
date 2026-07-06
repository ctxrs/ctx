use assert_cmd::Command;
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};
use tempfile::{Builder, TempDir};

pub(crate) fn tempdir() -> TempDir {
    Builder::new().prefix("ctx-search-mvp-").tempdir().unwrap()
}

pub(crate) fn ctx(temp: &TempDir) -> Command {
    let mut command = Command::cargo_bin("ctx").unwrap();
    apply_hermetic_env(&mut command, temp);
    command
}

pub(crate) fn ctx_from_binary(temp: &TempDir, binary: &Path) -> Command {
    let mut command = Command::new(binary);
    apply_hermetic_env(&mut command, temp);
    command
}

pub(crate) fn apply_hermetic_env(command: &mut Command, temp: &TempDir) {
    command.env("CTX_DATA_ROOT", temp.path());
    command.env("HOME", temp.path());
    command.env("CTX_ANALYTICS_OFF", "1");
    // Drop provider override variables inherited from the developer
    // machine so discovery never escapes the temp directory.
    command.env_remove("OPENCLAW_STATE_DIR");
    command.env_remove("HERMES_HOME");
    command.env_remove("ASTRBOT_ROOT");
    command.env_remove("SHELLEY_DB");
    command.env_remove("KILO_DB");
    command.env_remove("FORGE_CONFIG");
    command.env_remove("VIBE_HOME");
    command.env_remove("XDG_CONFIG_HOME");
    command.env_remove("XDG_DATA_HOME");
    command.env_remove("XDG_STATE_HOME");
    command.env_remove("LOCALAPPDATA");
    command.env_remove("APPDATA");
}

pub(crate) fn copied_ctx_binary(temp: &TempDir) -> PathBuf {
    let source = PathBuf::from(Command::cargo_bin("ctx").unwrap().get_program().to_owned());
    let target = temp.path().join(if cfg!(windows) {
        "ctx-test-copy.exe"
    } else {
        "ctx-test-copy"
    });
    if fs::hard_link(&source, &target).is_err() {
        fs::copy(&source, &target).unwrap();
    }
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
    ctx(temp)
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
