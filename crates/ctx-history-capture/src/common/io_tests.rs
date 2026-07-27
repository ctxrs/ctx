#[cfg(target_os = "windows")]
use std::{
    fs,
    io::ErrorKind,
    os::windows::fs::{symlink_dir, symlink_file},
    path::Path,
};

#[cfg(target_os = "windows")]
use super::{
    ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
    ensure_supported_windows_provider_path_prefix,
};

#[cfg(target_os = "windows")]
fn symlink_unavailable(error: &std::io::Error) -> bool {
    error.kind() == ErrorKind::PermissionDenied || error.raw_os_error() == Some(1314)
}

#[cfg(target_os = "windows")]
#[test]
fn windows_ordinary_absolute_provider_file_is_accepted() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("provider.db");
    fs::write(&path, b"provider").unwrap();

    assert!(path.is_absolute());
    ensure_regular_provider_transcript_file(&path).unwrap();
}

#[cfg(target_os = "windows")]
#[test]
fn windows_supported_rooted_prefixes_are_accepted_without_io() {
    for path in [
        Path::new(r"C:\provider.db"),
        Path::new(r"\\?\C:\provider.db"),
        Path::new(r"\\server\share\provider.db"),
        Path::new(r"\\?\UNC\server\share\provider.db"),
    ] {
        ensure_supported_windows_provider_path_prefix(path).unwrap();
    }
}

#[cfg(target_os = "windows")]
#[test]
fn windows_drive_relative_provider_path_is_rejected() {
    assert!(
        ensure_provider_path_parents_are_not_symlinks(Path::new(r"C:provider\history.jsonl"))
            .is_err()
    );
}

#[cfg(target_os = "windows")]
#[test]
fn windows_reparse_file_is_rejected() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let target = temp.path().join("target.db");
    let link = temp.path().join("link.db");
    fs::write(&target, b"provider").unwrap();
    if let Err(error) = symlink_file(&target, &link) {
        if symlink_unavailable(&error) {
            return;
        }
        panic!("failed to create Windows file symlink: {error}");
    }

    assert!(ensure_regular_provider_transcript_file(&link).is_err());
}

#[cfg(target_os = "windows")]
#[test]
fn windows_reparse_parent_is_rejected() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let target = temp.path().join("target");
    let link = temp.path().join("link");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("provider.db"), b"provider").unwrap();
    if let Err(error) = symlink_dir(&target, &link) {
        if symlink_unavailable(&error) {
            return;
        }
        panic!("failed to create Windows directory symlink: {error}");
    }

    assert!(ensure_provider_path_parents_are_not_symlinks(&link.join("provider.db")).is_err());
}
