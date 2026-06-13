use std::os::unix::fs::PermissionsExt;

use super::{
    acquire_daemon_lock, daemon_auth_path, load_or_init_daemon_auth, write_daemon_auth_file,
    DaemonAuthFile,
};

fn mode(path: &std::path::Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn load_or_init_daemon_auth_repairs_existing_file_permissions() {
    let temp = tempfile::tempdir().unwrap();
    let path = daemon_auth_path(temp.path());
    write_daemon_auth_file(
        &path,
        &DaemonAuthFile {
            token: "token".to_string(),
            daemon_url: None,
        },
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

    let auth = load_or_init_daemon_auth(temp.path()).unwrap();

    assert_eq!(auth.token, "token");
    assert_eq!(mode(&path), 0o600);
}

#[test]
fn load_or_init_daemon_auth_rejects_symlinked_auth_file() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let path = daemon_auth_path(temp.path());
    let outside_path = outside.path().join("daemon_auth.json");
    std::fs::write(&outside_path, r#"{"token":"outside"}"#).unwrap();
    std::os::unix::fs::symlink(&outside_path, &path).unwrap();

    let err = load_or_init_daemon_auth(temp.path()).unwrap_err();

    assert!(format!("{err:#}").contains("must not be a symlink"));
}

#[test]
fn acquire_daemon_lock_creates_private_lock_file() {
    let temp = tempfile::tempdir().unwrap();

    let lock = acquire_daemon_lock(temp.path()).unwrap();

    assert_eq!(mode(&temp.path().join("daemon.lock")), 0o600);
    drop(lock);
}

#[test]
fn acquire_daemon_lock_rejects_symlinked_lock_file() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_path = outside.path().join("daemon.lock");
    std::fs::write(&outside_path, b"outside").unwrap();
    std::os::unix::fs::symlink(&outside_path, temp.path().join("daemon.lock")).unwrap();

    let err = acquire_daemon_lock(temp.path()).unwrap_err();

    assert!(format!("{err:#}").contains("must not be a symlink"));
}

#[test]
fn prepare_daemon_data_root_creates_private_root_and_logs_dir() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("ctx-data");

    let prepared = super::prepare_daemon_data_root(data_root.clone()).unwrap();

    assert!(prepared.is_absolute());
    assert_eq!(mode(&prepared), 0o700);
    assert_eq!(mode(&prepared.join("logs")), 0o700);
}
