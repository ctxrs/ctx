use super::*;
use ctx_daemon_runtime::{write_private_json_file, DaemonLock};

#[test]
fn hashless_worker_is_readable_and_reusable_without_losing_handoff_identity() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let lock = DaemonLock::acquire_for_finite_worker(temp.path())?.expect("finite owner");
    let owner = read_daemon_owner_identity(temp.path())?.expect("read hashless owner");
    assert_eq!(owner.binary_sha256, None);
    assert!(owner.binary_metadata.is_some());
    assert!(active_daemon_matches_current_executable(temp.path())?);
    let config = test_config();
    match request_daemon_autostart_with(
        &crate::TestHost,
        temp.path(),
        &config,
        DaemonTrigger::Import,
        DaemonLaunchProfile::FiniteCoreWorker,
        &mut || Ok(()),
    )? {
        DaemonAutostartRequest::Existing(observed) => assert_eq!(observed, owner),
        _ => panic!("hashless owner must be reused"),
    }

    let now_ms = utc_now().timestamp_millis();
    let candidate = daemon_handoff_status_observation_from(
        Some(&running_status(&owner, &config, now_ms)),
        Some(&owner),
        None,
        &config,
        DaemonReadinessRequirement::Core,
        now_ms,
    );
    assert_eq!(
        complete_daemon_handoff_observation(
            candidate.clone(),
            Some(&owner),
            Some(&owner),
            DaemonLifecycleEndpointObservation::Unavailable,
        ),
        DaemonHandoffObservation::Pending,
    );
    assert!(matches!(
        complete_daemon_handoff_observation(
            candidate.clone(),
            Some(&owner),
            Some(&owner),
            DaemonLifecycleEndpointObservation::Ready,
        ),
        DaemonHandoffObservation::Running(_),
    ));
    let path = daemon_lock_path(temp.path());
    let original = read_pid_lock_json(&path).expect("published worker lock");
    let mut changed = original.clone();
    changed["binary_metadata"]["len"] =
        json!(changed["binary_metadata"]["len"].as_u64().unwrap() + 1);
    write_private_json_file(&path, &changed)?;
    let changed_owner = read_daemon_owner_identity(temp.path())?.expect("changed owner stamp");
    assert_eq!(
        complete_daemon_handoff_observation(
            candidate,
            Some(&owner),
            Some(&changed_owner),
            DaemonLifecycleEndpointObservation::Ready,
        ),
        DaemonHandoffObservation::Pending,
    );
    assert!(!active_daemon_matches_current_executable(temp.path())?);
    for malformed in [Value::Null, json!({}), json!("invalid stamp")] {
        changed["binary_metadata"] = malformed;
        write_private_json_file(&path, &changed)?;
        assert!(read_daemon_owner_identity(temp.path())?.is_none());
    }
    write_private_json_file(&path, &original)?;
    drop(lock);
    assert!(read_daemon_owner_identity(temp.path())?.is_none());
    Ok(())
}

#[test]
fn recovery_never_signals_a_hashless_owner_even_when_it_is_unresponsive() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let _lock = DaemonLock::acquire_for_finite_worker(temp.path())?.expect("finite owner");
    let owner = read_daemon_owner_identity(temp.path())?.expect("finite identity");
    let events = RefCell::new(Vec::new());
    assert!(!recover_unusable_daemon_owner_with(
        &owner,
        || {
            events.borrow_mut().push("probe");
            Ok(false)
        },
        || {
            events.borrow_mut().push("revalidate");
            read_daemon_owner_identity(temp.path())
        },
        |_| panic!("metadata must not authorize forced termination"),
        || Ok(()),
    )?);
    assert_eq!(events.borrow().as_slice(), &["probe", "revalidate"]);
    assert!(daemon_lock_is_active(temp.path()));
    Ok(())
}

#[test]
fn legacy_sha_only_owner_remains_readable_and_reusable() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let _lock = DaemonLock::acquire(temp.path())?.expect("legacy fixture owner");
    let path = daemon_lock_path(temp.path());
    let mut legacy = read_pid_lock_json(&path).expect("fixture identity");
    legacy.as_object_mut().unwrap().remove("binary_metadata");
    write_private_json_file(&path, &legacy)?;
    let owner = read_daemon_owner_identity(temp.path())?.expect("legacy identity");
    assert!(owner.binary_metadata.is_none());
    assert!(owner.binary_sha256.is_some());
    assert!(active_daemon_matches_current_executable(temp.path())?);
    legacy["binary_metadata"] = json!({});
    write_private_json_file(&path, &legacy)?;
    assert!(read_daemon_owner_identity(temp.path())?.is_none());
    assert!(!active_daemon_matches_current_executable(temp.path())?);
    Ok(())
}
