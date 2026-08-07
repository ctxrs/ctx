use super::*;

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Barrier,
    },
};

use anyhow::Context as _;

use crate::semantic::{
    daemon_wakeup::DaemonWakeup, source_backed_refresh_coordinator::CoreRefreshEngine,
};

fn private_data_root() -> Result<(tempfile::TempDir, PathBuf)> {
    let temp = tempfile::tempdir()?;
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root)?;
    Ok((temp, data_root))
}

fn successful_publication(generation_id: &str) -> SourceBackedRefreshPublication {
    SourceBackedRefreshPublication {
        generation_id: generation_id.to_owned(),
        published_explicit_source_catalog: None,
        unsupported_routes: 0,
        certified_source_count: 0,
        certified_source_bytes: 0,
        current: Default::default(),
        timings: Default::default(),
        route_results: Vec::new(),
        zero_source_authority: Vec::new(),
        catalog_route_bindings: Vec::new(),
        verified_index: None,
    }
}

fn refresh_request(request_id: &str) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "op": "source_refresh_request",
        "request_id": request_id,
        "mode": "wait",
        "operation": "refresh",
        "fresh_after_admitted_snapshot": true,
    }))
}

fn source_refresh_roundtrip(data_root: &Path, request: Value) -> Result<Value> {
    daemon_source_refresh_request(data_root, request, StdDuration::from_secs(1), 64 * 1024)?
        .ok_or_else(|| anyhow!("source refresh endpoint unavailable"))
}

#[cfg(any(unix, windows))]
#[test]
fn running_cold_periodic_all_and_manual_all_share_one_physical_executor() -> Result<()> {
    let (_temp, data_root) = private_data_root()?;
    let admission_entered = Arc::new(Barrier::new(2));
    let admission_release = Arc::new(Barrier::new(2));
    let fence_entered = Arc::clone(&admission_entered);
    let fence_release = Arc::clone(&admission_release);
    let coordinator = Arc::new(CoreRefreshEngine::with_admission_fence_for_test(Arc::new(
        move |_data_root, _catalog| {
            fence_entered.wait();
            fence_release.wait();
            Ok(BTreeMap::new())
        },
    )));
    let periodic = coordinator.enqueue_periodic(&data_root)?;
    let periodic_id = periodic["request_id"]
        .as_str()
        .expect("periodic request ID")
        .to_owned();

    let execution_entered = Arc::new(Barrier::new(2));
    let execution_release = Arc::new(Barrier::new(2));
    let runner_entered = Arc::clone(&execution_entered);
    let runner_release = Arc::clone(&execution_release);
    let execution_count = Arc::new(AtomicUsize::new(0));
    let observed_executions = Arc::clone(&execution_count);
    let runner_coordinator = Arc::clone(&coordinator);
    let runner = std::thread::spawn(move || {
        runner_coordinator.run_next_with(
            move |_request_id, _coordinator| {
                observed_executions.fetch_add(1, Ordering::SeqCst);
                runner_entered.wait();
                runner_release.wait();
                Ok(successful_publication("cold-periodic-generation"))
            },
            || Ok(Some("cold-periodic-generation".to_owned())),
            |_job| Ok(()),
            |_error| Ok(()),
        )
    });
    execution_entered.wait();

    let service = start_daemon_source_refresh_service_with_coordinator_for_test(
        &data_root,
        SharedSemanticRuntime::default(),
        TEST_QUERY_REQUEST_READ_TIMEOUT,
        Arc::clone(&coordinator),
    )?;
    let planner_coordinator = Arc::clone(&coordinator);
    let planner_data_root = data_root.clone();
    let planner = std::thread::spawn(move || -> Result<()> {
        let deadline = Instant::now() + StdDuration::from_secs(2);
        loop {
            if planner_coordinator.prepare_next_pending_admission(&planner_data_root)? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "admission planner was not released after acknowledgement"
                ));
            }
            std::thread::sleep(StdDuration::from_millis(5));
        }
    });
    let manual_id = "019fcaaa-0000-7000-8000-000000000294";
    let acknowledged_at = Instant::now();
    let acknowledgement = source_refresh_roundtrip(&data_root, refresh_request(manual_id))?;
    let acknowledgement_elapsed = acknowledged_at.elapsed();

    assert!(
        acknowledgement_elapsed < StdDuration::from_millis(750),
        "durable acknowledgement took {acknowledgement_elapsed:?}"
    );
    assert_eq!(acknowledgement["request_id"], manual_id);
    assert_eq!(acknowledgement["request_state"], "admission_pending");
    assert_eq!(
        acknowledgement["disconnect_policy"],
        "retain_after_durable_admission"
    );
    assert_eq!(acknowledgement["coalesced_into_request_id"], periodic_id);

    admission_entered.wait();
    let ping_started = Instant::now();
    let ping = source_refresh_roundtrip(
        &data_root,
        compact_json(json!({"schema_version": 1, "op": "ping"})),
    )?;
    assert_eq!(ping["ok"], true);
    assert!(ping_started.elapsed() < StdDuration::from_millis(500));
    let status_started = Instant::now();
    let planning_status = source_refresh_roundtrip(
        &data_root,
        compact_json(json!({
            "schema_version": 1,
            "op": "source_refresh_status",
            "request_id": manual_id,
        })),
    )?;
    assert_eq!(planning_status["request_state"], "admission_pending");
    assert!(status_started.elapsed() < StdDuration::from_millis(500));

    admission_release.wait();
    planner
        .join()
        .expect("admission planner panicked")
        .context("plan admitted manual demand")?;
    let resolved_at = Instant::now();
    while coordinator
        .status_for_test(manual_id)
        .is_some_and(|status| {
            status["request_state"] == "admission_pending"
                && resolved_at.elapsed() < StdDuration::from_secs(2)
        })
    {
        std::thread::sleep(StdDuration::from_millis(5));
    }
    assert_eq!(
        coordinator.status_for_test(manual_id).unwrap()["request_state"],
        "queued"
    );

    execution_release.wait();
    let periodic_run = runner
        .join()
        .expect("periodic runner panicked")
        .expect("periodic physical run");
    assert_eq!(periodic_run.job["request_id"], periodic_id);
    assert_eq!(periodic_run.job["request_state"], "published");
    assert_eq!(execution_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        coordinator.status_for_test(&periodic_id).unwrap()["coalesced_logical_demands"],
        1
    );

    let logical = coordinator
        .resolve_fully_covered_continuation_for_test(&data_root, |_catalog| Ok(BTreeMap::new()))
        .expect("covered logical demand terminal result");
    assert_eq!(logical.job["request_id"], manual_id);
    assert_eq!(logical.job["request_state"], "published");
    assert_eq!(logical.job["scanned_routes"], 0);
    assert_eq!(execution_count.load(Ordering::SeqCst), 1);

    drop(service);
    Ok(())
}

struct BrokenResponseWriter;

impl std::io::Write for BrokenResponseWriter {
    fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "client disconnected before acknowledgement",
        ))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn disconnected_client_does_not_cancel_a_durably_admitted_request() -> Result<()> {
    let (_temp, data_root) = private_data_root()?;
    let coordinator =
        CoreRefreshEngine::with_admission_fence_for_test(Arc::new(|_data_root, _catalog| {
            Ok(BTreeMap::new())
        }));
    let request_id = "019fcaaa-0000-7000-8000-000000000295";
    let token = "0123456789abcdef0123456789abcdef";
    let mut request = refresh_request(request_id);
    request["token"] = Value::String(token.to_owned());

    handle_daemon_query_stream(
        &data_root,
        &SharedSemanticRuntime::default(),
        &coordinator,
        DaemonIpcService::SourceRefresh,
        token,
        BrokenResponseWriter,
        Ok(serde_json::to_string(&request)?),
        Some(&DaemonWakeup::default()),
    );

    let admitted = coordinator
        .status_for_test(request_id)
        .expect("retained admission");
    assert_eq!(admitted["request_state"], "admission_pending");
    assert_eq!(
        admitted["disconnect_policy"],
        "retain_after_durable_admission"
    );
    assert!(coordinator.prepare_next_pending_admission(&data_root)?);
    assert_eq!(
        coordinator.status_for_test(request_id).unwrap()["request_state"],
        "queued"
    );
    let durable = read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root))
        .expect("retained durable request");
    assert_eq!(durable["request_id"], request_id);
    assert_eq!(durable["request_state"], "queued");
    Ok(())
}
