use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;

use super::{
    ensure_private_dir_sync, open_private_append_sync, read_private_file_to_string_sync,
    write_private_file_atomic_sync, PRIVATE_DIR_MODE, PRIVATE_FILE_MODE,
};

#[test]
fn private_dir_sync_sets_0700() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("private");

    ensure_private_dir_sync(&path).unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, PRIVATE_DIR_MODE);
}

#[test]
fn private_atomic_write_sets_0600() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secret.json");

    write_private_file_atomic_sync(&path, b"secret").unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, PRIVATE_FILE_MODE);
    assert_eq!(std::fs::read(&path).unwrap(), b"secret");
}

#[test]
fn private_append_create_sets_0600() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("daemon.log");

    let mut file = open_private_append_sync(&path).unwrap();
    file.write_all(b"line\n").unwrap();
    drop(file);

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, PRIVATE_FILE_MODE);
}

#[test]
fn private_append_rejects_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("target.log");
    let link = dir.path().join("daemon.log");
    std::fs::write(&target, b"outside").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let err = open_private_append_sync(&link).unwrap_err();

    assert!(format!("{err:#}").contains("must not be a symlink"));
    assert_eq!(std::fs::read(&target).unwrap(), b"outside");
}

#[test]
fn private_read_repairs_permissions_without_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secret.json");
    std::fs::write(&path, "secret").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

    let contents = read_private_file_to_string_sync(&path).unwrap().unwrap();

    assert_eq!(contents, "secret");
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, PRIVATE_FILE_MODE);
}

#[test]
fn private_read_rejects_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("secret.json");
    let link = dir.path().join("secret.json");
    std::fs::write(&target, "outside").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let err = read_private_file_to_string_sync(&link).unwrap_err();

    assert!(format!("{err:#}").contains("must not be a symlink"));
}

#[test]
fn private_atomic_write_rejects_symlinked_parent_chain() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let data_root = dir.path().join("data");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::create_dir_all(outside.path().join("nested")).unwrap();
    std::os::unix::fs::symlink(outside.path(), data_root.join("link")).unwrap();
    let path = data_root.join("link").join("nested").join("secret.json");

    let err = write_private_file_atomic_sync(&path, b"secret").unwrap_err();

    assert!(format!("{err:#}").contains("symlink or reparse point"));
    assert!(!outside.path().join("nested").join("secret.json").exists());
}

#[test]
fn private_read_rejects_symlinked_parent_chain() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let data_root = dir.path().join("data");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::create_dir_all(outside.path().join("nested")).unwrap();
    std::fs::write(outside.path().join("nested").join("secret.json"), "outside").unwrap();
    std::os::unix::fs::symlink(outside.path(), data_root.join("link")).unwrap();
    let path = data_root.join("link").join("nested").join("secret.json");

    let err = read_private_file_to_string_sync(&path).unwrap_err();

    assert!(format!("{err:#}").contains("symlink or reparse point"));
}
