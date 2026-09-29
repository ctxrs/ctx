use super::*;
use ctx_daemon_runtime::{pid_lock_payload, PidFileLock};

#[test]
fn receipt_owner_accepts_only_the_current_daemon_tuple() -> Result<()> {
    let root = tempfile::tempdir()?;
    let path = daemon_lock_path(root.path());
    create_private_dir_all(&daemon_root_path(root.path()))?;
    assert!(WakeupReceiptOwner::read(root.path()).is_none());
    let valid = pid_lock_payload(json!({
        "owner_id": "fixture-owner",
        "started_at_ms": 1234,
        "binary": "private-path-canary",
        "unexpected": "private-field-canary",
    }));
    write_private_json_file(&path, &valid)?;
    assert_eq!(
        WakeupReceiptOwner::read(root.path()).unwrap().to_json(),
        json!({
            "owner_id": "fixture-owner",
            "pid": std::process::id(),
            "started_at_ms": 1234,
        })
    );
    for (key, value) in [
        ("owner_id", Value::Null),
        ("owner_id", json!("")),
        ("pid", json!(0)),
        ("pid", json!(u64::MAX)),
        ("released", json!(true)),
        ("released", Value::Null),
        ("started_at_ms", json!(0)),
        ("started_at_ms", json!("1234")),
        ("lock_protocol", json!("unknown")),
    ] {
        let mut invalid = valid.clone();
        invalid[key] = value;
        write_private_json_file(&path, &invalid)?;
        assert!(WakeupReceiptOwner::read(root.path()).is_none(), "{key}");
    }
    fs::write(&path, b"not json")?;
    assert!(WakeupReceiptOwner::read(root.path()).is_none());
    Ok(())
}

#[test]
fn receipt_keeps_its_captured_epoch_and_recreated_watcher_keeps_scheduler_counters() -> Result<()> {
    let root = tempfile::tempdir()?;
    create_private_dir_all(&daemon_root_path(root.path()))?;
    let payload = pid_lock_payload(json!({"owner_id": "first-owner", "started_at_ms": 1234}));
    let path = daemon_lock_path(root.path());
    let _lock = PidFileLock::acquire(&path, payload)?.expect("fixture owner");
    let wakeup = Arc::new(DaemonWakeup::default());
    let catalog = catalog_owner(watch_catalog([]));
    let watcher = DaemonFileWatcher::start(root.path(), Arc::clone(&wakeup), catalog.clone())?;
    wakeup.record_cycle(true);
    watcher.write_receipt("active")?;
    let first = daemon_wakeup_report(root.path());
    assert_eq!(
        first["daemon_owner"],
        json!({
            "owner_id": "first-owner", "pid": std::process::id(), "started_at_ms": 1234,
        })
    );
    assert_eq!(first["wakeup"]["work_cycles"], 1);
    drop(watcher);
    let watcher = DaemonFileWatcher::start(root.path(), Arc::clone(&wakeup), catalog)?;
    wakeup.record_cycle(true);
    watcher.write_receipt("active")?;
    let recreated = daemon_wakeup_report(root.path());
    assert_eq!(recreated["daemon_owner"], first["daemon_owner"]);
    assert_eq!(recreated["wakeup"]["work_cycles"], 2);

    // Inject replacement metadata: an old writer must not label its counters
    // with that replacement, including in its final stopped receipt.
    let replacement = pid_lock_payload(json!({"owner_id": "second-owner", "started_at_ms": 5678}));
    write_private_json_file(&path, &replacement)?;
    watcher.write_receipt("idle")?;
    assert_eq!(
        daemon_wakeup_report(root.path())["daemon_owner"],
        first["daemon_owner"]
    );
    drop(watcher);
    assert_eq!(
        daemon_wakeup_report(root.path())["daemon_owner"],
        first["daemon_owner"]
    );
    Ok(())
}

#[test]
fn unbound_watcher_receipt_leaves_epoch_unknown() -> Result<()> {
    let root = tempfile::tempdir()?;
    let watcher = DaemonFileWatcher::start(
        root.path(),
        Arc::new(DaemonWakeup::default()),
        catalog_owner(watch_catalog([])),
    )?;
    watcher.write_receipt("idle")?;
    let receipt = daemon_wakeup_report(root.path());
    assert!(receipt.get("daemon_owner").is_none());
    assert_eq!(receipt["wakeup"]["work_cycles"], 0);
    Ok(())
}
