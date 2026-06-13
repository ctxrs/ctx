use super::fixtures::unix_mode;
use crate::daemon::providers::cursor_process_login::capture::{
    cursor_login_home, ensure_private_dir, initialize_cursor_capture_file,
    write_cursor_capture_hook,
};

#[cfg(unix)]
#[tokio::test]
async fn cursor_login_session_dirs_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("tempdir");
    let login_home = cursor_login_home(temp.path(), "login-id");
    let workdir = login_home.join("workspace");

    std::fs::create_dir_all(&workdir).expect("create workdir");
    std::fs::set_permissions(&login_home, std::fs::Permissions::from_mode(0o755))
        .expect("set login_home perms");
    std::fs::set_permissions(&workdir, std::fs::Permissions::from_mode(0o755))
        .expect("set workdir perms");

    ensure_private_dir(&login_home)
        .await
        .expect("secure login_home");
    ensure_private_dir(&workdir).await.expect("secure workdir");

    assert_eq!(unix_mode(&login_home), 0o700);
    assert_eq!(unix_mode(&workdir), 0o700);
}

#[cfg(unix)]
#[tokio::test]
async fn cursor_login_capture_file_is_repermissioned_to_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("tempdir");
    let login_home = cursor_login_home(temp.path(), "login-id");
    let hook_path = login_home.join("capture-hook.cjs");
    let capture_path = login_home.join("captured_tokens.jsonl");

    std::fs::create_dir_all(&login_home).expect("create login_home");
    std::fs::set_permissions(&login_home, std::fs::Permissions::from_mode(0o755))
        .expect("set login_home perms");
    std::fs::write(&capture_path, b"stale").expect("write capture file");
    std::fs::set_permissions(&capture_path, std::fs::Permissions::from_mode(0o644))
        .expect("set capture perms");

    write_cursor_capture_hook(&hook_path)
        .await
        .expect("write capture hook");
    initialize_cursor_capture_file(&capture_path)
        .await
        .expect("initialize capture file");

    assert_eq!(unix_mode(&login_home), 0o700);
    assert_eq!(unix_mode(&hook_path), 0o600);
    assert_eq!(unix_mode(&capture_path), 0o600);
    assert_eq!(
        std::fs::read(&capture_path).expect("read capture file"),
        b"",
        "capture file should be reset before the hook appends tokens"
    );
}
