use super::*;
use ctx_history_platform::platform_security::{verify_private_directory, verify_private_file};
use serde_json::json;

fn isolated_root() -> Result<tempfile::TempDir> {
    #[cfg(target_os = "linux")]
    let parent = std::path::PathBuf::from("/tmp");
    #[cfg(not(target_os = "linux"))]
    let parent = std::env::temp_dir();
    Ok(tempfile::Builder::new()
        .prefix("ctx-private-json-")
        .tempdir_in(parent)?)
}

#[test]
fn cold_and_existing_private_json_replacements_preserve_exact_content() -> Result<()> {
    let temp = isolated_root()?;
    let root = temp.path().join("data");
    let path = root.join("daemon/jobs/status.json");
    for value in [
        json!({"state":"running"}),
        json!({"state":"published","receipt":"é"}),
    ] {
        write_private_json_file(&path, &value)?;
        sync_private_file_parent(&path)?;
        assert_eq!(read_daemon_job_status_strict(&path)?, Some(value));
        verify_private_file(&path)?;
        for directory in [&root, &root.join("daemon"), &root.join("daemon/jobs")] {
            verify_private_directory(directory)?;
        }
    }
    assert_eq!(fs::read_dir(path.parent().unwrap())?.count(), 1);
    Ok(())
}

#[test]
fn private_json_obstruction_errors_preserve_content_and_allow_retry() -> Result<()> {
    let temp = isolated_root()?;
    let blocked = temp.path().join("blocked");
    fs::write(&blocked, "original")?;
    let path = blocked.join("jobs/status.json");
    assert!(write_private_json_file(&path, &json!({"new":true})).is_err());
    assert_eq!(fs::read_to_string(&blocked)?, "original");
    fs::remove_file(&blocked)?;
    create_private_dir_all(path.parent().unwrap())?;
    // A directory at the target causes an actual OS replacement error, after
    // the temporary file was written. Its sentinel and cleanup must survive.
    fs::create_dir(&path)?;
    fs::write(path.join("retained"), "retained")?;
    let value = json!({"new":true});
    assert!(write_private_json_file(&path, &value).is_err());
    assert_eq!(fs::read_to_string(path.join("retained"))?, "retained");
    assert_eq!(fs::read_dir(path.parent().unwrap())?.count(), 1);
    fs::remove_dir_all(&path)?;
    write_private_json_file(&path, &value)?;
    sync_private_file_parent(&path)?;
    assert_eq!(read_daemon_job_status_strict(&path)?, Some(value));
    verify_private_file(&path)?;
    Ok(())
}

#[cfg(windows)]
#[test]
fn windows_no_delete_sharing_preserves_old_json_until_handle_closes() -> Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
    let temp = isolated_root()?;
    let path = temp.path().join("status.json");
    let old = json!({"old":true});
    let new = json!({"new":true});
    write_private_json_file(&path, &old)?;
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(&path)?;
    assert!(write_private_json_file(&path, &new).is_err());
    assert_eq!(read_daemon_job_status_strict(&path)?, Some(old));
    assert_eq!(fs::read_dir(temp.path())?.count(), 1);
    drop(held);
    write_private_json_file(&path, &new)?;
    assert_eq!(read_daemon_job_status_strict(&path)?, Some(new));
    verify_private_file(&path)?;
    Ok(())
}
