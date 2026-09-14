use std::{fs, os::fd::AsRawFd as _};

use super::*;

#[test]
fn family_validation_preserves_active_sqlite_write_lock() -> anyhow::Result<()> {
    const CHILD_ENV: &str = "CTX_TEST_USAGE_WAL_LOCK_PATH";
    const BYTE_ENV: &str = "CTX_TEST_USAGE_WAL_LOCK_BYTE";
    const TYPE_ENV: &str = "CTX_TEST_USAGE_WAL_LOCK_TYPE";
    if let Some(path) = std::env::var_os(CHILD_ENV) {
        let file = fs::File::open(path)?;
        let mut lock: libc::flock = unsafe { std::mem::zeroed() };
        lock.l_type = libc::F_WRLCK as _;
        lock.l_whence = libc::SEEK_SET as _;
        lock.l_start = std::env::var(BYTE_ENV)?.parse()?;
        lock.l_len = 1;
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETLK, &mut lock) } == -1 {
            return Err(io::Error::last_os_error().into());
        }
        assert_eq!(
            lock.l_type,
            std::env::var(TYPE_ENV)?.parse::<libc::c_short>()?,
            "family access released an active SQLite lock"
        );
        return Ok(());
    }

    let root = tempfile::tempdir()?;
    let path = usage_path(&root.path().join("private"));
    let mut store = open_writable(&path, true, BUSY_TIMEOUT)?.expect("created store");
    let assert_locked = |phase: &str, byte: i64, kind: libc::c_short| -> anyhow::Result<()> {
        let output = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "local_usage::store::lock_tests::family_validation_preserves_active_sqlite_write_lock",
                "--nocapture",
            ])
            .env(CHILD_ENV, path.with_file_name("usage.sqlite-shm"))
            .env(BYTE_ENV, byte.to_string())
            .env(TYPE_ENV, kind.to_string())
            .output()?;
        assert!(
            output.status.success(),
            "{phase}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    };
    // SQLite uses bytes120..127 for WAL-index locks and byte128 for its
    // shared dead-man-switch lock throughout an open WAL connection.
    assert_locked("after writable admission", 128, libc::F_RDLCK as _)?;
    let transaction = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute(
        "INSERT INTO maintenance(singleton, last_retention_day) VALUES (1, '2026-09-12')",
        [],
    )?;
    assert_locked("before family recheck", 120, libc::F_WRLCK as _)?;
    store.family_guard.recheck(&path)?;
    drop(store.family_guard.before_commit(&path)?);
    store.family_guard.protect(&path)?;
    assert_locked("after validation and hardening", 120, libc::F_WRLCK as _)?;

    let authority = LocalUsageStorageAuthority::new(path.clone(), TEST_PRODUCT_VERSION);
    assert!(usage_store_exists(&authority).is_err());
    assert!(open_read_only(&path).is_err());
    assert!(open_writable(&path, false, BUSY_TIMEOUT).is_err());
    assert_locked(
        "after competing same-process access",
        120,
        libc::F_WRLCK as _,
    )?;

    let other = usage_path(&root.path().join("other"));
    drop(open_writable(&other, true, BUSY_TIMEOUT)?.expect("independent root"));
    assert_locked("after independent-root access", 120, libc::F_WRLCK as _)?;

    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640))?;
    assert!(store.family_guard.before_commit(&path).is_err());
    assert_locked("after failed validation", 120, libc::F_WRLCK as _)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    store.family_guard.before_commit(&path)?;
    transaction.commit()?;
    store.family_guard.protect(&path)?;
    assert_locked("after commit and hardening", 128, libc::F_RDLCK as _)?;
    drop(store);

    let reader = open_read_only(&path)?;
    reader.verify_unchanged()?;
    assert!(open_writable(&path, false, BUSY_TIMEOUT).is_err());
    drop(reader);
    drop(open_writable(&path, false, BUSY_TIMEOUT)?.expect("admission released"));
    Ok(())
}
