use super::*;

#[test]
fn advisory_pid_lock_does_not_expire_or_trust_a_reused_pid() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let lock = DaemonLock::acquire(temp.path())?.expect("daemon lock");
    let path = daemon_lock_path(temp.path());
    assert!(pid_lock_file_reports_running(
        &path,
        Some(ProcessState::Running),
        "running"
    ));
    assert!(!daemon_lock_is_stale(&path));
    assert_eq!(
        observe_pid_advisory_lock(&path),
        Some(PidAdvisoryLockObservation {
            held: true,
            released: false,
        })
    );
    assert!(DaemonLock::acquire(temp.path())?.is_none());

    drop(lock);
    assert!(!pid_lock_file_reports_running(
        &path,
        Some(ProcessState::Running),
        "running"
    ));
    assert!(daemon_lock_is_stale(&path));
    assert_eq!(
        observe_pid_advisory_lock(&path),
        Some(PidAdvisoryLockObservation {
            held: false,
            released: true,
        })
    );
    let replacement = DaemonLock::acquire(temp.path())?
        .expect("released advisory lock should be reusable despite live payload pid");
    assert!(pid_lock_file_reports_running(
        &path,
        Some(ProcessState::Running),
        "running"
    ));
    drop(replacement);

    fs::write(&path, b"{")?;
    assert!(!daemon_lock_is_stale(&path));
    Ok(())
}

#[test]
fn advisory_pid_lock_allows_only_one_concurrent_reclaimer() -> Result<()> {
    let temp = tempfile::tempdir()?;
    drop(DaemonLock::acquire(temp.path())?.expect("seed lock"));
    let root = temp.path().to_path_buf();
    let contenders = 8;
    let start = Arc::new(std::sync::Barrier::new(contenders + 1));
    let finish = Arc::new(std::sync::Barrier::new(contenders + 1));
    let (send, receive) = std::sync::mpsc::channel();
    let mut threads = Vec::new();
    for _ in 0..contenders {
        let root = root.clone();
        let start = Arc::clone(&start);
        let finish = Arc::clone(&finish);
        let send = send.clone();
        threads.push(std::thread::spawn(move || -> Result<()> {
            start.wait();
            let lock = DaemonLock::acquire(&root)?;
            send.send(lock.is_some())?;
            finish.wait();
            drop(lock);
            Ok(())
        }));
    }
    drop(send);
    start.wait();
    let acquired = (0..contenders)
        .map(|_| receive.recv())
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|acquired| *acquired)
        .count();
    finish.wait();
    for thread in threads {
        thread.join().expect("lock contender panicked")?;
    }
    assert_eq!(acquired, 1);
    Ok(())
}

#[test]
fn advisory_pid_lock_waits_out_a_status_probe() -> Result<()> {
    let temp = tempfile::tempdir()?;
    drop(DaemonLock::acquire(temp.path())?.expect("seed lock"));
    let path = daemon_lock_path(temp.path());
    let probe = private_open_existing_lock_file(&pid_lock_guard_path(&path))?;
    fs2::FileExt::lock_shared(&probe)?;
    let root = temp.path().to_path_buf();
    let contender = std::thread::spawn(move || DaemonLock::acquire(&root));
    std::thread::sleep(StdDuration::from_millis(5));
    fs2::FileExt::unlock(&probe)?;
    let lock = contender
        .join()
        .expect("lock contender panicked")?
        .expect("status probe should not make acquisition give up");
    drop(lock);
    Ok(())
}

#[test]
fn advisory_guard_survives_metadata_path_replacement() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = daemon_lock_path(temp.path());
    let lock = DaemonLock::acquire(temp.path())?.expect("daemon lock");
    fs::remove_file(&path)?;
    fs::write(&path, serde_json::to_vec(&pid_lock_payload(json!({})))?)?;
    assert!(DaemonLock::acquire(temp.path())?.is_none());
    drop(lock);
    assert!(DaemonLock::acquire(temp.path())?.is_some());
    Ok(())
}

#[test]
fn advisory_publication_does_not_overwrite_a_late_legacy_owner() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = daemon_lock_path(temp.path());
    create_private_dir_all(path.parent().expect("lock parent"))?;
    fs::write(
        &path,
        serde_json::to_vec(&json!({
            "pid": process::id(),
            "started_at_ms": utc_now().timestamp_millis(),
        }))?,
    )?;
    assert!(!publish_pid_lock_metadata(
        &path,
        &pid_lock_payload(json!({}))
    )?);
    assert!(!pid_lock_uses_advisory_protocol(
        &read_pid_lock_json(&path).expect("legacy metadata")
    ));
    Ok(())
}

#[test]
fn advisory_lock_reclaims_dead_legacy_metadata_for_upgrade_handoff() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = daemon_lock_path(temp.path());
    create_private_dir_all(path.parent().expect("lock parent"))?;
    fs::write(
        &path,
        serde_json::to_vec(&json!({
            "pid": u32::MAX,
            "started_at_ms": 0,
        }))?,
    )?;
    assert!(daemon_lock_is_stale(&path));
    let lock = DaemonLock::acquire(temp.path())?
        .expect("dead legacy owner should be reclaimed during upgrade");
    assert!(pid_lock_uses_advisory_protocol(
        &read_pid_lock_json(&path).expect("advisory metadata")
    ));
    drop(lock);
    Ok(())
}
