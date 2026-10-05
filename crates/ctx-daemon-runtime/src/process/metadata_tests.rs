use super::*;

fn stamped_lock(executable: &Path) -> Result<Value> {
    Ok(json!({
        "binary": fs::canonicalize(executable)?,
        "binary_metadata": executable_metadata_stamp(executable)?,
    }))
}

#[test]
fn metadata_reuse_detects_same_path_rebuild_and_atomic_replacement() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    fs::write(&executable, b"old")?;
    let initial = stamped_lock(&executable)?;
    assert!(daemon_lock_metadata_identity_matches(
        &initial,
        &executable
    )?);

    fs::write(&executable, b"rebuilt executable")?;
    assert!(!daemon_lock_metadata_identity_matches(
        &initial,
        &executable
    )?);
    let rebuilt = stamped_lock(&executable)?;
    let modified = fs::metadata(&executable)?.modified()?;
    let replacement = temp.path().join("ctx.new");
    fs::write(&replacement, b"different artifact")?;
    fs::OpenOptions::new()
        .write(true)
        .open(&replacement)?
        .set_modified(modified + std::time::Duration::from_secs(1))?;
    fs::rename(&replacement, &executable)?;
    assert!(!daemon_lock_metadata_identity_matches(
        &rebuilt,
        &executable
    )?);
    assert!(daemon_lock_metadata_identity_matches(
        &stamped_lock(&executable)?,
        &executable
    )?);
    Ok(())
}

#[cfg(unix)]
#[test]
fn inode_and_ctime_detect_replacements_with_preserved_length_and_mtime() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    fs::write(&executable, b"old executable")?;
    let modified = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    fs::OpenOptions::new()
        .write(true)
        .open(&executable)?
        .set_modified(modified)?;
    let initial = stamped_lock(&executable)?;
    // Cross coarse timestamp resolution before changing the same inode and
    // restoring mtime; ctime is the only differing recorded fact here.
    std::thread::sleep(std::time::Duration::from_millis(1_100));
    fs::write(&executable, b"new executable")?;
    fs::OpenOptions::new()
        .write(true)
        .open(&executable)?
        .set_modified(modified)?;
    let rebuilt = stamped_lock(&executable)?;
    assert_eq!(
        initial["binary_metadata"]["len"],
        rebuilt["binary_metadata"]["len"]
    );
    assert_eq!(
        initial["binary_metadata"]["modified_at"],
        rebuilt["binary_metadata"]["modified_at"]
    );
    assert_eq!(
        initial["binary_metadata"]["inode"],
        rebuilt["binary_metadata"]["inode"]
    );
    assert!(!daemon_lock_metadata_identity_matches(
        &initial,
        &executable
    )?);

    let replacement = temp.path().join("ctx.new");
    fs::write(&replacement, b"old executable")?;
    fs::OpenOptions::new()
        .write(true)
        .open(&replacement)?
        .set_modified(modified)?;
    fs::rename(&replacement, &executable)?;
    let replaced = stamped_lock(&executable)?;
    assert_eq!(
        rebuilt["binary_metadata"]["len"],
        replaced["binary_metadata"]["len"]
    );
    assert_eq!(
        rebuilt["binary_metadata"]["modified_at"],
        replaced["binary_metadata"]["modified_at"]
    );
    assert_ne!(
        rebuilt["binary_metadata"]["inode"],
        replaced["binary_metadata"]["inode"]
    );
    assert!(!daemon_lock_metadata_identity_matches(
        &rebuilt,
        &executable
    )?);
    Ok(())
}

#[test]
fn metadata_reuse_checks_the_selected_executable_and_never_reads_its_bytes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("selected-ctx");
    let other = temp.path().join("other-ctx");
    fs::write(&executable, b"selected executable")?;
    fs::write(&other, b"selected executable")?;
    let mut lock = stamped_lock(&executable)?;
    lock["binary_sha256"] = json!("unused legacy digest");
    let before = EXECUTABLE_HASH_READS.with(std::cell::Cell::get);
    assert!(daemon_lock_metadata_identity_matches(&lock, &executable)?);
    assert!(!daemon_lock_metadata_identity_matches(&lock, &other)?);
    assert_eq!(EXECUTABLE_HASH_READS.with(std::cell::Cell::get), before);
    Ok(())
}

#[cfg(windows)]
#[test]
fn windows_creation_time_participates_in_the_metadata_stamp() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    fs::write(&executable, b"executable")?;
    let mut lock = stamped_lock(&executable)?;
    let stamp = daemon_lock_executable_metadata(&lock).unwrap();
    assert_eq!(stamp.created_at, fs::metadata(&executable)?.created().ok());
    assert!(daemon_lock_metadata_identity_matches(&lock, &executable)?);
    if let Some(created_at) = stamp.created_at {
        lock["binary_metadata"]["created_at"] =
            json!(created_at + std::time::Duration::from_secs(1));
        assert!(!daemon_lock_metadata_identity_matches(&lock, &executable)?);
    }
    Ok(())
}

#[test]
fn missing_paths_and_malformed_stamps_cannot_match() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    fs::write(&executable, b"executable")?;
    let original = stamped_lock(&executable)?;
    for stamp in [Value::Null, json!({}), json!("invalid"), json!({"len": 10})] {
        let mut malformed = original.clone();
        malformed["binary_metadata"] = stamp;
        malformed["binary_sha256"] = json!(executable_sha256(&executable)?);
        assert!(!daemon_lock_metadata_identity_matches(
            &malformed,
            &executable
        )?);
    }
    fs::remove_file(&executable)?;
    assert!(!daemon_lock_metadata_identity_matches(
        &original,
        &executable
    )?);
    assert!(!daemon_lock_metadata_identity_matches(
        &original,
        &temp.path().join("also-missing")
    )?);
    assert!(!daemon_lock_binary_identity_matches(
        &original,
        &executable
    )?);
    Ok(())
}

#[test]
fn legacy_sha_only_locks_retain_mismatch_detection() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    fs::write(&executable, b"old executable")?;
    let lock = json!({
        "binary": executable,
        "binary_sha256": executable_sha256(&executable)?,
    });
    assert!(daemon_lock_metadata_identity_matches(&lock, &executable)?);
    fs::write(&executable, b"new executable")?;
    assert!(!daemon_lock_metadata_identity_matches(&lock, &executable)?);
    Ok(())
}

#[test]
fn live_metadata_observation_rejects_an_unrelated_process_image() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("unrelated-ctx");
    fs::write(&executable, b"unrelated executable")?;
    let mut lock = stamped_lock(&executable)?;
    lock["pid"] = json!(std::process::id());
    assert!(daemon_lock_metadata_identity_matches(&lock, &executable)?);
    assert!(!daemon_owner_metadata_identity_matches(&lock, &executable)?);
    lock["pid"] = json!(0);
    assert!(!daemon_owner_metadata_identity_matches(&lock, &executable)?);
    Ok(())
}
