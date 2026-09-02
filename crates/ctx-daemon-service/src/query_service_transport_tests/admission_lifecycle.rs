use super::*;

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Barrier,
    },
};

use crate::{daemon_wakeup::DaemonWakeup, source_backed_refresh_coordinator::CoreRefreshEngine};

fn private_data_root() -> Result<(tempfile::TempDir, PathBuf)> {
    let temp = tempfile::tempdir()?;
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root)?;
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
        verified_publication: None,
    }
}

fn refresh_request(request_id: &str) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "op": "source_refresh_request",
        "request_id": request_id,
        "mode": "wait",
        "trigger": "import",
        "refresh_intent": {
            "kind": "selected_import",
            "selection": {"kind": "all"},
        },
    }))
}

fn source_refresh_roundtrip(data_root: &Path, request: Value) -> Result<Value> {
    daemon_source_refresh_request(data_root, request, StdDuration::from_secs(1), 64 * 1024)?
        .ok_or_else(|| anyhow!("source refresh endpoint unavailable"))
}

#[cfg(any(unix, windows))]
#[test]
fn running_cold_periodic_all_and_manual_all_keep_distinct_physical_policy() -> Result<()> {
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
    coordinator.complete_pending_admission_for_test(&data_root, &periodic_id, BTreeMap::new())?;

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

    let wakeup = Arc::new(DaemonWakeup::default());
    let handler = ctx_authenticated_request_handler(
        &data_root,
        SharedSemanticRuntime::default(),
        Arc::clone(&coordinator),
        wakeup,
        &crate::test_support::CONFIG,
    );
    let service = start_daemon_source_refresh_service_with_request_timeout(
        &data_root,
        handler,
        TEST_QUERY_REQUEST_READ_TIMEOUT,
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
    assert!(acknowledgement["coalesced_into_request_id"].is_null());

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
        0
    );

    let manual_execution_count = Arc::clone(&execution_count);
    let manual = coordinator
        .run_next_with(
            move |_request_id, _coordinator| {
                manual_execution_count.fetch_add(1, Ordering::SeqCst);
                Ok(successful_publication("manual-all-generation"))
            },
            || Ok(Some("manual-all-generation".to_owned())),
            |_job| Ok(()),
            |_error| Ok(()),
        )
        .expect("manual All physical run");
    assert_eq!(manual.job["request_id"], manual_id);
    assert_eq!(manual.job["request_state"], "published");
    assert_eq!(manual.job["operation"], "import");
    assert_eq!(execution_count.load(Ordering::SeqCst), 2);

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

struct BarrierOrderingWriter<'a> {
    data_root: &'a Path,
    coordinator: &'a CoreRefreshEngine,
    wakeup: &'a DaemonWakeup,
    writes: usize,
}

impl std::io::Write for BarrierOrderingWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        assert!(
            !self
                .coordinator
                .prepare_next_pending_admission(self.data_root)
                .expect("check admission barrier during response write"),
            "admission barrier released before the response write attempt"
        );
        assert!(
            self.wakeup.wait(StdDuration::ZERO).timed_out,
            "scheduler woke before the response write attempt completed"
        );
        self.writes += 1;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn admission_barrier_releases_before_post_write_scheduler_wakeup() -> Result<()> {
    let (_temp, data_root) = private_data_root()?;
    let coordinator = Arc::new(CoreRefreshEngine::with_admission_fence_for_test(Arc::new(
        |_data_root, _catalog| Ok(BTreeMap::new()),
    )));
    let wakeup = Arc::new(DaemonWakeup::default());
    let handler = ctx_authenticated_request_handler(
        &data_root,
        SharedSemanticRuntime::default(),
        Arc::clone(&coordinator),
        Arc::clone(&wakeup),
        &crate::test_support::CONFIG,
    );
    let token = "0123456789abcdef0123456789abcdef";
    let request_id = "019fcaaa-0000-7000-8000-000000000296";
    let mut request = refresh_request(request_id);
    request["token"] = Value::String(token.to_owned());
    let mut writer = BarrierOrderingWriter {
        data_root: &data_root,
        coordinator: &coordinator,
        wakeup: &wakeup,
        writes: 0,
    };

    handle_authenticated_daemon_stream(
        handler.as_ref(),
        &ServiceId::new("source-refresh")?,
        token,
        &mut writer,
        Ok(serde_json::to_string(&request)?),
    )?;

    assert!(writer.writes > 0);
    assert!(!wakeup.wait(StdDuration::ZERO).timed_out);
    assert!(coordinator.prepare_next_pending_admission(&data_root)?);
    assert_eq!(
        coordinator.status_for_test(request_id).unwrap()["request_state"],
        "queued"
    );
    Ok(())
}

#[test]
fn source_refresh_finisher_releases_barrier_before_signaling_scheduler() -> Result<()> {
    let (_temp, data_root) = private_data_root()?;
    let coordinator =
        CoreRefreshEngine::with_admission_fence_for_test(Arc::new(|_data_root, _catalog| {
            Ok(BTreeMap::new())
        }));
    let request_id = "019fcaaa-0000-7000-8000-000000000297";
    let response = crate::source_backed_refresh_adapter::wire::handle_ipc_request(
        &coordinator,
        &data_root,
        &refresh_request(request_id),
    )?
    .expect("source refresh wire response");
    assert_eq!(
        coordinator.status_for_test(request_id).unwrap()["request_state"],
        "admission_pending"
    );

    let mut signaled = false;
    crate::source_backed_refresh_adapter::wire::finish_wire_response_for_test(
        response,
        &coordinator,
        || {
            signaled = true;
            assert!(
                coordinator
                    .prepare_next_pending_admission(&data_root)
                    .expect("admission barrier must be released before scheduler signal"),
                "scheduler signal observed before admission barrier release"
            );
        },
    );

    assert!(signaled);
    assert_eq!(
        coordinator.status_for_test(request_id).unwrap()["request_state"],
        "queued"
    );
    Ok(())
}

#[cfg(any(unix, windows))]
#[test]
fn typed_admission_failure_is_durable_and_visible_over_the_status_transport() -> Result<()> {
    let (_temp, data_root) = private_data_root()?;
    let route = ctx_history_index::SourceRouteIdentity::from_sha256("ab".repeat(32))?;
    let failed_route = route.clone();
    let coordinator = Arc::new(CoreRefreshEngine::with_admission_fence_for_test(Arc::new(
        move |_data_root, _catalog| {
            Err(
                ctx_history_capture::SourceBackedCoordinatorError::NoUsableSourceRoutes {
                    failed_routes: ctx_history_capture::SourceBackedSourceFailures::from_failures(
                        [ctx_history_capture::SourceBackedFailedRoute::new(
                            failed_route.clone(),
                            "cd".repeat(32),
                            ctx_history_core::CaptureProvider::Shelley,
                            ctx_history_capture::SourceBackedSourceFailureClass::Unreadable,
                            false,
                            "shelley.db",
                            "file is not a database",
                        )],
                    ),
                }
                .into(),
            )
        },
    )));
    let wakeup = Arc::new(DaemonWakeup::default());
    let handler = ctx_authenticated_request_handler(
        &data_root,
        SharedSemanticRuntime::default(),
        Arc::clone(&coordinator),
        wakeup,
        &crate::test_support::CONFIG,
    );
    let service = start_daemon_source_refresh_service_with_request_timeout(
        &data_root,
        handler,
        TEST_QUERY_REQUEST_READ_TIMEOUT,
    )?;
    let request_id = "019fcaaa-0000-7000-8000-000000000296";

    let acknowledged = source_refresh_roundtrip(&data_root, refresh_request(request_id))?;
    assert_eq!(acknowledged["request_state"], "admission_pending");
    assert!(coordinator.prepare_next_pending_admission(&data_root)?);
    let terminal = source_refresh_roundtrip(
        &data_root,
        compact_json(json!({
            "schema_version": 1,
            "op": "source_refresh_status",
            "request_id": request_id,
        })),
    )?;

    assert_eq!(terminal["request_state"], "failed", "{terminal:#}");
    assert_eq!(terminal["failure_type"], "malformed_source", "{terminal:#}");
    assert_eq!(terminal["error_code"], "malformed_source", "{terminal:#}");
    assert_eq!(terminal["reason"], "unreadable", "{terminal:#}");
    assert_eq!(
        terminal["structured_outcome"]["affected_routes"],
        json!([route.as_str()]),
        "{terminal:#}"
    );
    assert_eq!(
        terminal["structured_outcome"]["blocked_routes"],
        json!([route.as_str()]),
        "{terminal:#}"
    );
    assert_eq!(terminal["structured_outcome"]["retryable"], false);
    assert_eq!(
        terminal["structured_outcome"]["retry_advice"],
        "inspect_sources"
    );
    let durable = read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root))
        .expect("typed terminal admission failure");
    assert_eq!(durable["request_id"], terminal["request_id"]);
    assert_eq!(durable["request_state"], terminal["request_state"]);
    assert_eq!(durable["failure_type"], terminal["failure_type"]);
    assert_eq!(durable["error_code"], terminal["error_code"]);
    assert_eq!(durable["reason"], terminal["reason"]);
    assert_eq!(
        durable["structured_outcome"],
        terminal["structured_outcome"]
    );
    assert_eq!(coordinator.pending_scheduler_retry_root_for_test(), None);
    drop(service);
    Ok(())
}

#[test]
fn disconnected_client_does_not_cancel_a_durably_admitted_request() -> Result<()> {
    let (_temp, data_root) = private_data_root()?;
    let coordinator = Arc::new(CoreRefreshEngine::with_admission_fence_for_test(Arc::new(
        |_data_root, _catalog| Ok(BTreeMap::new()),
    )));
    let request_id = "019fcaaa-0000-7000-8000-000000000295";
    let token = "0123456789abcdef0123456789abcdef";
    let mut request = refresh_request(request_id);
    request["token"] = Value::String(token.to_owned());

    let wakeup = Arc::new(DaemonWakeup::default());
    let handler = ctx_authenticated_request_handler(
        &data_root,
        SharedSemanticRuntime::default(),
        Arc::clone(&coordinator),
        wakeup,
        &crate::test_support::CONFIG,
    );
    let _ = handle_authenticated_daemon_stream(
        handler.as_ref(),
        &ServiceId::new("source-refresh")?,
        token,
        BrokenResponseWriter,
        Ok(serde_json::to_string(&request)?),
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
