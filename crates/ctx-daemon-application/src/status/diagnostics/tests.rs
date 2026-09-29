use std::time::Duration;

use super::*;

fn snapshot(id: &str, observed_at: Instant) -> DaemonDiagnosticSnapshot {
    DaemonDiagnosticSnapshot {
        owner: Some(Owner {
            id: id.to_owned(),
            pid: 42,
            started_at_ms: 100,
        }),
        daemon: json!({"pid": 42, "started_at_ms": 100}),
        refresh: json!({"request_state": "running"}),
        wakeup: json!({"wakeup": {"work_cycles": 10}}),
        observed_at,
        running: ProcessState::Running,
        cpu: Err(ProcessCpuUnavailable::Unsupported),
        cpu_at: observed_at,
    }
}

fn bind_wakeup(snapshot: &mut DaemonDiagnosticSnapshot) {
    let owner = snapshot.owner.as_ref().unwrap();
    snapshot.wakeup["daemon_owner"] =
        json!({"owner_id": owner.id, "pid": owner.pid, "started_at_ms": owner.started_at_ms});
}

#[test]
fn observed_scheduler_arithmetic_and_resets_use_one_owner_epoch() {
    let start = Instant::now();
    let mut first = snapshot("owner-a", start);
    let mut last = snapshot("owner-a", start + Duration::from_millis(2500));
    bind_wakeup(&mut first);
    bind_wakeup(&mut last);
    last.wakeup["wakeup"]["work_cycles"] = json!(13);
    let report = last.observation_since(&first);
    assert_eq!(report["observed"]["scheduler"]["status"], "observed");
    assert_eq!(report["observed"]["scheduler"]["work_cycles"], 3);
    assert_eq!(
        report["observed"]["scheduler"]["work_cycles_per_second"],
        1.2
    );
    assert!(report["observed"]["scheduler"]["ipc_signals"].is_null());
    first.wakeup["wakeup"]["ipc_signals"] = json!(u64::MAX);
    last.wakeup["wakeup"]["ipc_signals"] = json!(0);
    let report = last.observation_since(&first);
    assert_eq!(report["observed"]["scheduler"]["status"], "counter_reset");
    assert!(report["observed"]["scheduler"]["work_cycles"].is_null());
    last.wakeup["daemon_owner"]["owner_id"] = json!("old-owner");
    assert_eq!(
        last.observation_since(&first)["observed"]["scheduler"]["status"],
        "daemon_restarted"
    );
}

#[test]
fn cpu_failures_remain_typed_and_do_not_discard_refresh() {
    let start = Instant::now();
    let first = snapshot("owner-a", start);
    for (failure, expected) in [
        (ProcessCpuUnavailable::PermissionDenied, "permission_denied"),
        (ProcessCpuUnavailable::NotRunning, "process_exited"),
        (ProcessCpuUnavailable::Unsupported, "unsupported"),
        (ProcessCpuUnavailable::Unavailable, "unavailable"),
    ] {
        let mut last = snapshot("owner-a", start + Duration::from_secs(1));
        last.cpu = Err(failure);
        let report = last.observation_since(&first);
        assert_eq!(report["observed"]["cpu"]["status"], expected);
        assert!(report["observed"]["cpu"]["cpu_ms"].is_null());
        assert_eq!(report["refresh"]["request_state"], "running");
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn cpu_arithmetic_excludes_other_processes_and_rejects_reset() {
    // Obtain only an opaque identity from our own process; all arithmetic
    // inputs are authored, so no workload timing determines the expectation.
    let identity = observe_process_cpu(std::process::id()).unwrap().identity;
    let start = Instant::now();
    let mut first = snapshot("owner-a", start);
    let mut last = snapshot("owner-a", start + Duration::from_millis(2500));
    first.cpu = Ok(ProcessCpuObservation {
        identity,
        user_cpu_us: 100,
        system_cpu_us: 200,
    });
    last.cpu = Ok(ProcessCpuObservation {
        identity,
        user_cpu_us: 200_100,
        system_cpu_us: 50_200,
    });
    let report = last.observation_since(&first);
    assert_eq!(report["observed"]["cpu"]["cpu_ms"], 250.0);
    assert_eq!(report["observed"]["cpu"]["percent_one_core"], 10.0);
    last.cpu = Ok(ProcessCpuObservation {
        identity,
        user_cpu_us: 99,
        system_cpu_us: 50_200,
    });
    assert_eq!(
        last.observation_since(&first)["observed"]["cpu"]["status"],
        "counter_reset"
    );
}

#[test]
fn daemon_restart_and_legacy_receipts_invalidate_deltas() {
    let start = Instant::now();
    let first = snapshot("owner-a", start);
    let mut last = snapshot("owner-b", start + Duration::from_millis(2500));
    let observation = last.observation_since(&first);
    assert_eq!(
        observation["observed"]["scheduler"]["status"],
        "daemon_restarted"
    );
    assert!(observation["observed"]["scheduler"]["work_cycles"].is_null());
    last.owner = Some(Owner {
        id: "owner-a".to_owned(),
        pid: 42,
        started_at_ms: 100,
    });
    let observation = last.observation_since(&first);
    assert_eq!(
        observation["observed"]["scheduler"]["status"],
        "identity_unknown"
    );
    assert!(observation["observed"]["scheduler"]["work_cycles_per_second"].is_null());
    assert_eq!(observation["elapsed_ms"], 2500);
    last.running = ProcessState::NotRunning;
    assert_eq!(
        last.observation_since(&first)["observed"]["scheduler"]["status"],
        "process_exited"
    );
}

#[test]
fn passive_snapshot_does_not_create_or_modify_files() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("uninitialized");
    let first = DaemonDiagnosticSnapshot::observe(&missing);
    let last = DaemonDiagnosticSnapshot::observe(&missing);
    assert!(!missing.exists());
    assert_eq!(last.observation_since(&first)["daemon"]["state"], "unknown");

    let daemon = daemon_root_path(root.path());
    fs::create_dir_all(daemon.join("jobs")).unwrap();
    let status = daemon_status_path(root.path());
    fs::write(&status, b"{\"status\":\"stopped\"}").unwrap();
    let job = daemon_core_refresh_job_path(root.path());
    fs::write(&job, b"{\"request_state\":\"published\"}").unwrap();
    let before_status = fs::read(&status).unwrap();
    let before_job = fs::read(&job).unwrap();
    let first = DaemonDiagnosticSnapshot::observe(root.path());
    let last = DaemonDiagnosticSnapshot::observe(root.path());
    assert_eq!(
        last.observation_since(&first)["refresh"]["request_state"],
        "published"
    );
    assert_eq!(fs::read(status).unwrap(), before_status);
    assert_eq!(fs::read(job).unwrap(), before_job);
    assert_eq!(fs::read_dir(&daemon).unwrap().count(), 2);
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn same_process_held_lock_is_observable_but_stale_reused_pid_is_not() {
    use ctx_daemon_runtime::{
        current_daemon_lock_identity, pid_lock_guard_path, DaemonQuiescenceGuard, PidFileLock,
    };

    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(daemon_root_path(root.path())).unwrap();
    let lock_path = daemon_lock_path(root.path());
    // The test process stands in for a daemon while holding the real lock.
    // Setup uses the real daemon payload, including its private birth token.
    // Sampling itself never inspects an executable or starts a process.
    let payload = current_daemon_lock_identity(root.path()).unwrap();
    let held = PidFileLock::acquire(&lock_path, payload.clone())
        .unwrap()
        .unwrap();
    fs::write(
        daemon_status_path(root.path()),
        serde_json::to_vec(&json!({
            "status": "running", "pid": std::process::id(), "started_at_ms": payload["started_at_ms"]
        }))
        .unwrap(),
    )
    .unwrap();
    let wakeup = json!({
        "daemon_owner": {
            "owner_id": payload["owner_id"], "pid": payload["pid"],
            "started_at_ms": payload["started_at_ms"],
        },
        "wakeup": {"work_cycles": 5},
    });
    fs::write(
        daemon_root_path(root.path()).join("wakeup.json"),
        serde_json::to_vec(&wakeup).unwrap(),
    )
    .unwrap();
    let first = DaemonDiagnosticSnapshot::observe(root.path());
    let last = DaemonDiagnosticSnapshot::observe(root.path());
    assert!(
        first.owner.is_some(),
        "another handle must detect our same-process exclusive lock"
    );
    let report = last.observation_since(&first);
    assert_eq!(report["daemon"]["state"], "running");
    assert_eq!(report["observed"]["cpu"]["status"], "observed");
    assert!(report["observed"]["cpu"]["cpu_ms"].is_number());
    assert_eq!(report["observed"]["scheduler"]["status"], "observed");
    assert_eq!(report["observed"]["scheduler"]["work_cycles"], 0);

    for (key, value) in [
        ("lock_protocol", Value::Null),
        ("lock_protocol", json!("legacy")),
        ("released", Value::Null),
        ("released", json!(true)),
        ("released", json!("false")),
        ("process_creation_token", Value::Null),
    ] {
        let mut invalid = payload.clone();
        invalid[key] = value;
        let bytes = serde_json::to_vec(&invalid).unwrap();
        fs::write(&lock_path, &bytes).unwrap();
        assert!(
            DaemonDiagnosticSnapshot::observe(root.path())
                .owner
                .is_none(),
            "accepted invalid {key}"
        );
        assert_eq!(fs::read(&lock_path).unwrap(), bytes);
    }
    let mut legacy = payload.clone();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("process_creation_token");
    fs::write(&lock_path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let first = DaemonDiagnosticSnapshot::observe(root.path());
    let last = DaemonDiagnosticSnapshot::observe(root.path());
    let report = last.observation_since(&first);
    assert_eq!(report["daemon"]["state"], "unknown");
    assert_eq!(report["observed"]["cpu"]["status"], "identity_unknown");
    assert_eq!(
        report["observed"]["scheduler"]["status"],
        "identity_unknown"
    );
    assert!(report["observed"]["scheduler"]["work_cycles"].is_null());
    fs::write(&lock_path, serde_json::to_vec(&payload).unwrap()).unwrap();

    drop(held);
    fs::write(&lock_path, serde_json::to_vec(&payload).unwrap()).unwrap();
    let before = fs::read(&lock_path).unwrap();
    assert_eq!(process_state(std::process::id()), ProcessState::Running);
    let first = DaemonDiagnosticSnapshot::observe(root.path());
    let last = DaemonDiagnosticSnapshot::observe(root.path());
    let report = last.observation_since(&first);
    assert!(first.owner.is_none());
    assert_eq!(report["daemon"]["state"], "unknown");
    assert_eq!(report["observed"]["cpu"]["status"], "identity_unknown");
    assert!(report["observed"]["cpu"]["cpu_ms"].is_null());
    assert_eq!(fs::read(&lock_path).unwrap(), before);

    // A real cleanup holder plus coherent stale owner/status/wakeup metadata
    // must not authenticate a reused PID. Only the recorded birth differs.
    let mut stale = payload.clone();
    let born = stale["process_creation_token"]["started"].as_u64().unwrap();
    stale["process_creation_token"]["started"] = json!(born.checked_add(1).unwrap());
    let before = serde_json::to_vec(&stale).unwrap();
    fs::write(&lock_path, &before).unwrap();
    let cleanup = DaemonQuiescenceGuard::acquire(root.path())
        .unwrap()
        .unwrap();
    assert_eq!(observe_pid_advisory_guard(&lock_path), Some(true));
    let first = DaemonDiagnosticSnapshot::observe(root.path());
    let last = DaemonDiagnosticSnapshot::observe(root.path());
    let report = last.observation_since(&first);
    assert!(first.cpu.is_ok());
    assert!(first.owner.is_none());
    assert_eq!(report["daemon"]["state"], "unknown");
    assert_eq!(report["observed"]["cpu"]["status"], "identity_unknown");
    assert_eq!(
        report["observed"]["scheduler"]["status"],
        "identity_unknown"
    );
    assert!(report["observed"]["cpu"]["cpu_ms"].is_null());
    assert!(report["observed"]["scheduler"]["work_cycles"].is_null());
    assert_eq!(fs::read(&lock_path).unwrap(), before);
    drop(cleanup);

    // Missing guards are not repaired just to make diagnostics available.
    let guard_path = pid_lock_guard_path(&lock_path);
    fs::remove_file(&guard_path).unwrap();
    assert!(DaemonDiagnosticSnapshot::observe(root.path())
        .owner
        .is_none());
    assert!(!guard_path.exists());
    assert_eq!(fs::read(&lock_path).unwrap(), before);
}

#[test]
fn malformed_oversized_and_nonregular_records_are_unavailable() {
    let root = tempfile::tempdir().unwrap();
    let record = root.path().join("record.json");
    fs::write(&record, b"not JSON /private/example").unwrap();
    assert!(read_record(&record).is_null());
    fs::write(&record, b"[]").unwrap();
    assert!(read_record(&record).is_null());
    let file = fs::File::create(&record).unwrap();
    file.set_len(MAX_RECORD_BYTES + 1).unwrap();
    assert!(read_record(&record).is_null());
    assert!(read_record(root.path()).is_null());
    #[cfg(unix)]
    {
        let linked = root.path().join("link.json");
        std::os::unix::fs::symlink(&record, &linked).unwrap();
        assert!(read_record(&linked).is_null());
    }
}

#[test]
fn last_recorded_semantic_job_uses_bounded_passive_metadata_read() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("absent");
    assert!(DaemonDiagnosticSnapshot::last_recorded_semantic_job(&missing).is_null());
    assert!(!missing.exists());
    let path = daemon_semantic_job_path(root.path());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let bytes = br#"{"status":"budget_exhausted","source_work_remaining":true,"last_run_at_ms":1}"#;
    fs::write(&path, bytes).unwrap();
    let value = DaemonDiagnosticSnapshot::last_recorded_semantic_job(root.path());
    assert_eq!(value["status"], "budget_exhausted");
    assert_eq!(value["source_work_remaining"], true);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    fs::write(&path, b"malformed").unwrap();
    assert!(DaemonDiagnosticSnapshot::last_recorded_semantic_job(root.path()).is_null());
    assert_eq!(fs::read(&path).unwrap(), b"malformed");
    let file = fs::File::create(&path).unwrap();
    file.set_len(MAX_RECORD_BYTES + 1).unwrap();
    assert!(DaemonDiagnosticSnapshot::last_recorded_semantic_job(root.path()).is_null());
    assert_eq!(file.metadata().unwrap().len(), MAX_RECORD_BYTES + 1);
}
