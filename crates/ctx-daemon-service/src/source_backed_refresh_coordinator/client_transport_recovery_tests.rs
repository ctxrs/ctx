use super::observation_recovery::SourceRefreshObservationRecoveryFailed;
use super::*;

use std::{
    io::{Read as _, Write as _},
    os::unix::net::UnixListener,
    sync::Mutex,
};

use ctx_history_core::CaptureProvider;

use crate::query_service::{
    ctx_authenticated_request_handler, start_daemon_source_refresh_service_with_request_timeout,
    write_daemon_service_endpoint, DaemonIpcService, DaemonQueryEndpoint,
};
use crate::SharedSemanticRuntime;

const TEST_ENDPOINT_TOKEN: &str = "0123456789abcdef0123456789abcdef";

#[derive(Default)]
struct RecordingAvailability(Mutex<Vec<(crate::DaemonTrigger, crate::DaemonAvailabilityDemand)>>);

impl crate::DaemonAvailabilityPort for RecordingAvailability {
    fn ensure_available(
        &self,
        _data_root: &Path,
        trigger: crate::DaemonTrigger,
        demand: crate::DaemonAvailabilityDemand,
    ) -> Result<crate::DaemonAvailability> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((trigger, demand));
        Ok(crate::DaemonAvailability::Available)
    }
}

fn short_data_root() -> Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix("ctx-refresh-")
        .tempdir_in(std::fs::canonicalize("/tmp")?)
        .map_err(Into::into)
}

fn source_refresh_endpoint(socket_path: &Path) -> DaemonQueryEndpoint {
    DaemonQueryEndpoint::Unix {
        path: socket_path.to_owned(),
        token: TEST_ENDPOINT_TOKEN.to_owned(),
    }
}

#[test]
fn background_durable_request_is_accepted_through_client_and_coordinator() -> Result<()> {
    let data_root = short_data_root()?;
    ctx_history_platform::platform_security::establish_private_data_root(data_root.path())?;
    let generation = super::publish_authoritative_empty_generation_for_test(
        &source_backed_index_root(data_root.path()),
        "background-maintenance-wake-fixture",
        ctx_history_refresh::RefreshOperation::Refresh,
        ctx_history_capture::SourceBackedRefreshScope::All,
        None,
    )?
    .generation_id;
    let coordinator = Arc::new(CoreRefreshEngine::new());
    let handler = ctx_authenticated_request_handler(
        data_root.path(),
        SharedSemanticRuntime::default(),
        Arc::clone(&coordinator),
        Arc::new(crate::daemon_wakeup::DaemonWakeup::default()),
        &crate::test_support::CONFIG,
    );
    let service = start_daemon_source_refresh_service_with_request_timeout(
        data_root.path(),
        handler,
        StdDuration::from_secs(1),
    )?;

    let observation = coordinate_source_backed_refresh_with_policy(
        &crate::test_support::AVAILABILITY,
        data_root.path(),
        SourceBackedRefreshMode::Background,
        SourceBackedRefreshRequestPolicy {
            intent: RefreshIntent::AutomaticMaintenance,
            trigger: RefreshRequestTrigger::Search,
            allow_daemon_autostart: false,
        },
        false,
        None,
    )?;

    assert_eq!(observation.mode, SourceBackedRefreshMode::Background);
    assert_eq!(observation.status, "admission_pending");
    assert!(observation.daemon_available);
    assert!(observation.request_id.is_some());
    assert_eq!(observation.pin.generation_id(), generation);
    assert!(coordinator.has_pending_request());
    let request_id = observation.request_id.as_deref().context("background ID")?;
    assert!(coordinator.status(request_id).is_some());
    let durable = crate::paths_status::read_daemon_job_status(
        &crate::paths_status::daemon_source_backed_refresh_job_path(data_root.path()),
    )
    .context("background durable admission")?;
    assert_eq!(durable["request_id"], request_id);
    drop(service);
    Ok(())
}

#[test]
fn pre_submission_connection_refusal_remains_typed_unavailable() -> Result<()> {
    let data_root = short_data_root()?;
    let socket_path = data_root.path().join("refused.sock");
    let listener = UnixListener::bind(&socket_path)?;
    drop(listener);
    write_daemon_service_endpoint(
        data_root.path(),
        DaemonIpcService::SourceRefresh,
        &source_refresh_endpoint(&socket_path),
    )?;

    let error = daemon_source_refresh_request(
        data_root.path(),
        compact_json(json!({
            "schema_version": 1,
            "op": SOURCE_REFRESH_REQUEST_OP,
            "request_id": "019fcaaa-0000-7000-8000-000000000299",
        })),
        StdDuration::from_millis(100),
        SOURCE_REFRESH_RESPONSE_MAX_BYTES,
    )
    .expect_err("a closed pre-submission endpoint must be unavailable");

    assert!(error
        .downcast_ref::<DaemonSourceRefreshServiceUnavailable>()
        .is_some());
    Ok(())
}

#[test]
fn same_id_reenqueue_replays_the_exact_payload_after_a_lost_ack() -> Result<()> {
    let data_root = short_data_root()?;
    let socket_path = data_root.path().join("lost-ack.sock");
    let listener = UnixListener::bind(&socket_path)?;
    write_daemon_service_endpoint(
        data_root.path(),
        DaemonIpcService::SourceRefresh,
        &source_refresh_endpoint(&socket_path),
    )?;
    let request_id = "019fcaaa-0000-7000-8000-000000000300";
    let server = std::thread::spawn(move || -> Result<[Vec<u8>; 2]> {
        let mut requests = Vec::with_capacity(2);
        for attempt in 0..2 {
            let (mut stream, _) = listener.accept()?;
            let mut received = Vec::new();
            stream.read_to_end(&mut received)?;
            requests.push(received);
            if attempt == 0 {
                continue;
            }
            stream.write_all(
                format!(
                    "{{\"ok\":true,\"owner\":\"daemon\",\"request_id\":\"{request_id}\",\"request_state\":\"admission_pending\",\"schema_version\":1}}\n"
                )
                .as_bytes(),
            )?;
        }
        requests
            .try_into()
            .map_err(|_| anyhow!("test server did not observe exactly two requests"))
    });

    let recovered = enqueue_equivalent_wait_refresh_request(
        &crate::test_support::AVAILABILITY,
        data_root.path(),
        request_id,
        RefreshIntent::SelectedImport(RefreshSelection::Provider(CaptureProvider::Codex)),
        RefreshRequestTrigger::Import,
    )?;
    let requests = server.join().expect("lost-ack test server panicked")?;

    assert_eq!(recovered, request_id);
    assert_eq!(requests[0], requests[1]);
    for request in requests {
        let request: Value = serde_json::from_slice(&request)?;
        assert_eq!(request["request_id"], request_id);
        assert_eq!(request["trigger"], "import");
        assert_eq!(
            request["refresh_intent"],
            json!({
                "kind": "selected_import",
                "selection": {"kind": "provider", "provider": "codex"},
            })
        );
        assert!(request.get("operation").is_none());
        assert!(request.get("refresh_selector").is_none());
    }
    Ok(())
}

#[test]
fn provider_import_recovery_payload_keeps_selector_and_import_identity() -> Result<()> {
    let canonical = RefreshRequest::selected_import(
        "019fcaaa-0000-7000-8000-000000000414".to_owned(),
        RefreshSelection::Provider(CaptureProvider::Codex),
    );
    let request = wait_authority_request_json(SourceBackedRefreshMode::Wait, &canonical)?;

    assert_eq!(request["trigger"], "import");
    assert_eq!(
        request["refresh_intent"],
        json!({
            "kind": "selected_import",
            "selection": {"kind": "provider", "provider": "codex"},
        })
    );
    assert!(request.get("operation").is_none());
    assert!(request.get("refresh_selector").is_none());
    assert!(request.get("explicit_source_catalog").is_none());
    Ok(())
}

#[test]
fn canonical_requests_emit_only_the_canonical_intent() -> Result<()> {
    let authority = ctx_history_refresh::explicit_source_catalog_authority_for_test(0);
    let all = RefreshRequest::selected_import(
        "019fcaaa-0000-7000-8000-000000000415".to_owned(),
        RefreshSelection::All,
    );
    let exact = RefreshRequest::selected_import(
        "019fcaaa-0000-7000-8000-000000000416".to_owned(),
        RefreshSelection::ExactSource(authority.clone()),
    );

    let all = wait_authority_request_json(SourceBackedRefreshMode::Wait, &all)?;
    let exact = wait_authority_request_json(SourceBackedRefreshMode::Wait, &exact)?;

    assert_eq!(all["trigger"], "import");
    assert_eq!(
        all["refresh_intent"],
        json!({
            "kind": "selected_import",
            "selection": {"kind": "all"},
        })
    );
    assert!(all.get("operation").is_none());
    assert!(all.get("refresh_selector").is_none());
    assert!(all.get("fresh_after_admitted_snapshot").is_none());
    assert!(all.get("explicit_source_catalog").is_none());

    assert_eq!(exact["trigger"], "import");
    assert_eq!(
        exact["refresh_intent"],
        json!({
            "kind": "selected_import",
            "selection": {
                "kind": "exact_source",
                "authority": authority.to_json(),
            },
        })
    );
    assert!(exact.get("operation").is_none());
    assert!(exact.get("refresh_selector").is_none());
    assert!(exact.get("explicit_source_catalog").is_none());
    assert!(exact.get("fresh_after_admitted_snapshot").is_none());
    Ok(())
}

#[test]
fn automatic_provider_import_policy_keeps_selector_and_import_identity() {
    let policy = SourceBackedRefreshRequestPolicy::import(
        RefreshSelection::Provider(CaptureProvider::Codex),
        true,
    );

    assert_eq!(policy.trigger, RefreshRequestTrigger::Import);
    assert_eq!(
        policy.intent,
        RefreshIntent::SelectedImport(RefreshSelection::Provider(CaptureProvider::Codex))
    );
    assert!(policy.intent.is_selected_import());
}

#[test]
fn all_automatic_import_policy_preserves_legacy_refresh_operation() {
    let policy = SourceBackedRefreshRequestPolicy::import(RefreshSelection::All, true);

    assert_eq!(
        policy.intent.operation(),
        ctx_history_refresh::RefreshOperation::Import
    );
    assert_eq!(policy.trigger, RefreshRequestTrigger::Import);
    assert_eq!(
        policy.intent,
        RefreshIntent::SelectedImport(RefreshSelection::All)
    );
}

#[test]
fn background_lost_ack_terminal_replay_is_not_reported_as_pending() -> Result<()> {
    let data_root = short_data_root()?;
    ctx_history_platform::platform_security::establish_private_data_root(data_root.path())?;
    let socket_path = data_root.path().join("lost-ack-terminal.sock");
    let listener = UnixListener::bind(&socket_path)?;
    write_daemon_service_endpoint(
        data_root.path(),
        DaemonIpcService::SourceRefresh,
        &source_refresh_endpoint(&socket_path),
    )?;
    let server = std::thread::spawn(move || -> Result<[Vec<u8>; 2]> {
        let mut requests = Vec::with_capacity(2);
        for attempt in 0..2 {
            let (mut stream, _) = listener.accept()?;
            let mut received = Vec::new();
            stream.read_to_end(&mut received)?;
            let request: Value = serde_json::from_slice(&received)?;
            requests.push(received);
            if attempt == 0 {
                continue;
            }
            let request_id = request["request_id"]
                .as_str()
                .ok_or_else(|| anyhow!("lost-ack request had no request ID"))?;
            stream.write_all(
                format!(
                    "{{\"ok\":true,\"owner\":\"daemon\",\"request_id\":\"{request_id}\",\"request_state\":\"failed\",\"schema_version\":1,\"last_error\":\"replayed terminal failure\"}}\n"
                )
                .as_bytes(),
            )?;
        }
        requests
            .try_into()
            .map_err(|_| anyhow!("test server did not observe exactly two requests"))
    });

    let error = match coordinate_source_backed_refresh_with_policy(
        &crate::test_support::AVAILABILITY,
        data_root.path(),
        SourceBackedRefreshMode::Background,
        SourceBackedRefreshRequestPolicy {
            intent: RefreshIntent::AutomaticMaintenance,
            trigger: RefreshRequestTrigger::Search,
            allow_daemon_autostart: false,
        },
        false,
        None,
    ) {
        Ok(_) => panic!("terminal failed replay must remain a failure"),
        Err(error) => error,
    };
    let requests = server.join().expect("lost-ack test server panicked")?;

    assert_eq!(requests[0], requests[1]);
    assert!(error
        .downcast_ref::<SourceBackedRefreshPendingPublication>()
        .is_none());
    assert!(format!("{error:#}").contains("replayed terminal failure"));
    Ok(())
}

#[test]
fn exhausted_post_submission_disconnects_return_typed_ambiguous_admission() -> Result<()> {
    let data_root = short_data_root()?;
    let socket_path = data_root.path().join("lost-all-acks.sock");
    let listener = UnixListener::bind(&socket_path)?;
    write_daemon_service_endpoint(
        data_root.path(),
        DaemonIpcService::SourceRefresh,
        &source_refresh_endpoint(&socket_path),
    )?;
    let request_id = "019fcaaa-0000-7000-8000-000000000304";

    let server = std::thread::spawn(move || -> Result<Vec<Vec<u8>>> {
        let mut requests = Vec::new();
        for _ in 0..=AMBIGUOUS_ADMISSION_RECOVERY_ATTEMPT_LIMIT {
            let (mut stream, _) = listener.accept()?;
            let mut received = Vec::new();
            stream.read_to_end(&mut received)?;
            requests.push(received);
        }
        Ok(requests)
    });

    let error = enqueue_equivalent_wait_refresh_request(
        &crate::test_support::AVAILABILITY,
        data_root.path(),
        request_id,
        RefreshIntent::AutomaticMaintenance,
        RefreshRequestTrigger::Search,
    )
    .unwrap_err();
    let requests = server.join().expect("lost-ack test server panicked")?;

    let recovery = error
        .downcast_ref::<SourceRefreshAdmissionRecoveryFailed>()
        .expect("post-submission disconnect must use typed admission recovery");
    assert_eq!(recovery.request_id, request_id);
    assert_eq!(
        recovery.recovery_attempts,
        AMBIGUOUS_ADMISSION_RECOVERY_ATTEMPT_LIMIT
    );
    assert_eq!(
        requests.len(),
        1 + AMBIGUOUS_ADMISSION_RECOVERY_ATTEMPT_LIMIT
    );
    assert!(requests.windows(2).all(|pair| pair[0] == pair[1]));
    assert!(!error.to_string().contains("timed out"));
    Ok(())
}

// Real client transport with a controlled daemon-side state transition. The
// listener records the independently constructed request and actual wire response.
fn foreground_transport_fixture<T>(
    data_root: &Path,
    mut respond: impl FnMut(&Value) -> Result<Value> + Send + 'static,
    client: impl FnOnce() -> T,
) -> Result<(T, Vec<(Value, Value)>)> {
    use std::sync::atomic::{AtomicBool, Ordering};
    let socket_path = data_root.join("identity.sock");
    let listener = UnixListener::bind(&socket_path)?;
    listener.set_nonblocking(true)?;
    write_daemon_service_endpoint(
        data_root,
        DaemonIpcService::SourceRefresh,
        &source_refresh_endpoint(&socket_path),
    )?;
    let finished = Arc::new(AtomicBool::new(false));
    let stop = finished.clone();
    let server = std::thread::spawn(move || -> Result<Vec<(Value, Value)>> {
        let deadline = StdInstant::now() + StdDuration::from_secs(30);
        let mut exchanges = Vec::new();
        while !stop.load(Ordering::Acquire) {
            let (mut stream, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if StdInstant::now() >= deadline {
                        bail!("identity fixture exceeded its bounded wait");
                    }
                    std::thread::sleep(StdDuration::from_millis(1));
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            stream.set_read_timeout(Some(StdDuration::from_secs(2)))?;
            stream.set_write_timeout(Some(StdDuration::from_secs(2)))?;
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes)?;
            let request: Value = serde_json::from_slice(&bytes)?;
            let response = respond(&request).map_err(|error| {
                eprintln!(
                    "foreground fixture server failed for {}: {error:#}",
                    request["op"]
                );
                error
            })?;
            serde_json::to_writer(&mut stream, &response)?;
            stream.write_all(b"\n")?;
            exchanges.push((request, response));
        }
        Ok(exchanges)
    });
    let result = client();
    finished.store(true, Ordering::Release);
    Ok((
        result,
        server.join().expect("identity fixture server panicked")?,
    ))
}

#[test]
fn foreground_client_ids_survive_admission_and_status_beside_periodic_work() -> Result<()> {
    let data_root = short_data_root()?;
    ctx_history_platform::platform_security::establish_private_data_root(data_root.path())?;
    let engine = Arc::new(CoreRefreshEngine::new());
    let periodic = engine.enqueue_periodic(data_root.path())?;
    let server_engine = engine.clone();
    let server_root = data_root.path().to_owned();
    let (errors, exchanges) = foreground_transport_fixture(
        data_root.path(),
        move |request| {
            server_engine
                .handle_ipc_request(&server_root, request)?
                .context("wire response")
        },
        || {
            (0..2)
                .map(|_| {
                    coordinate_source_backed_refresh_with_progress(
                        &RecordingAvailability::default(),
                        data_root.path(),
                        SourceBackedRefreshMode::Wait,
                        &mut |_| bail!("stop after observing admitted status"),
                    )
                    .err()
                    .expect("reporter ends the wait after real admission and status")
                })
                .collect::<Vec<_>>()
        },
    )?;
    for error in errors {
        assert!(format!("{error:#}").contains("stop after observing admitted status"));
    }
    assert_eq!(exchanges.len(), 4);
    let mut ids = BTreeSet::new();
    for pair in exchanges.chunks_exact(2) {
        let (admission, accepted) = &pair[0];
        let (status_request, status) = &pair[1];
        assert_eq!(admission["op"], SOURCE_REFRESH_REQUEST_OP);
        assert_eq!(admission["mode"], "wait");
        assert_eq!(admission["trigger"], "search");
        assert_eq!(admission["refresh_intent"]["kind"], "automatic_maintenance");
        let id = admission["request_id"].as_str().context("client UUID")?;
        Uuid::parse_str(id)?;
        assert!(ids.insert(id.to_owned()));
        assert_ne!(admission["request_id"], periodic["request_id"]);
        assert_eq!(accepted["request_id"], admission["request_id"]);
        assert_eq!(status_request["op"], SOURCE_REFRESH_STATUS_OP);
        assert_eq!(status_request["request_id"], admission["request_id"]);
        assert_eq!(status["request_id"], admission["request_id"]);
        assert!(engine.status(id).is_some());
    }
    Ok(())
}

#[test]
fn foreground_forgotten_request_replays_exact_authority_and_pins_terminal_generation() -> Result<()>
{
    let data_root = short_data_root()?;
    ctx_history_platform::platform_security::establish_private_data_root(data_root.path())?;
    let source = tempfile::tempdir()?;
    let source_path = source.path().join("source.jsonl");
    std::fs::write(
        &source_path,
        "{\"record_type\":\"manifest\",\"schema_version\":\"ctx-history-jsonl-v2\"}\n",
    )?;
    let source =
        ctx_history_refresh::explicit_source_for_path(data_root.path(), &source_path, None, true)?;
    let authority =
        ctx_history_refresh::upsert_explicit_source(data_root.path(), &source)?.authority;
    let server_root = data_root.path().to_owned();
    let mut engine = CoreRefreshEngine::new();
    let mut step = 0;
    let availability = RecordingAvailability::default();
    let (observation, exchanges) = foreground_transport_fixture(
        data_root.path(),
        move |request| {
            step += 1;
            if step == 2 {
                // Inject the supported forgotten-state boundary. Ordinary
                // restart normally restores the journal; this deliberately
                // tests the response when this ID is no longer retained.
                engine = CoreRefreshEngine::new();
            }
            if step == 4 {
                assert!(engine.prepare_next_pending_admission(&server_root)?);
                let run = engine
                    .run_next(&server_root)
                    .context("readmitted refresh run")?;
                assert!(!run.failed, "{:#}", run.job);
            }
            engine
                .handle_ipc_request(&server_root, request)?
                .context("wire response")
        },
        || {
            coordinate_source_backed_refresh_with_policy(
                &availability,
                data_root.path(),
                SourceBackedRefreshMode::Wait,
                SourceBackedRefreshRequestPolicy::import(
                    RefreshSelection::ExactSource(authority.clone()),
                    false,
                ),
                false,
                None,
            )
        },
    )?;
    let observation = observation?;
    assert_eq!(exchanges.len(), 4);
    assert_eq!(exchanges[0].0, exchanges[2].0);
    assert_eq!(exchanges[1].0, exchanges[3].0);
    assert_eq!(
        exchanges[1].1["error_code"],
        SOURCE_REFRESH_UNKNOWN_REQUEST_ERROR_CODE
    );
    assert_eq!(exchanges[3].1["request_state"], "published");
    assert_eq!(
        observation.request_id.as_deref(),
        exchanges[0].0["request_id"].as_str()
    );
    assert_eq!(
        observation.pin.generation_id(),
        exchanges[3].1["published_generation"].as_str().unwrap()
    );
    let receipt = observation.receipt.context("terminal receipt")?;
    assert_eq!(
        receipt.published_generation,
        observation.pin.generation_id()
    );
    assert_eq!(receipt.published_explicit_source_catalog, Some(authority));
    assert!(
        availability.0.lock().unwrap().is_empty(),
        "healthy no-start recovery must not start a worker"
    );
    Ok(())
}

#[test]
fn foreground_unknown_recovery_is_bounded_and_rejects_mismatched_identity() -> Result<()> {
    for mutation in [
        None,
        Some(("request_id", json!("different-request"))),
        Some(("owner", json!("different-owner"))),
        Some(("schema_version", json!(2))),
        Some(("ok", json!(true))),
        Some(("request_state", json!("failed"))),
        Some(("reason", json!("unrecognized-reason"))),
        Some(("retryable", json!(true))),
        Some(("retryable", Value::Null)),
        Some(("error_code", Value::Null)),
        Some((
            "admission_durability",
            json!("replacement_visible_or_indeterminate"),
        )),
        Some((
            "admission_acknowledgement",
            json!("retained_after_durability_error"),
        )),
        Some(("receipt", json!({}))),
        Some(("receipt", Value::Null)),
        Some(("published_generation", json!("generation"))),
        Some(("previous_generation", json!("previous"))),
        Some(("generation_changed", json!(false))),
        Some(("outcome", json!("published"))),
        Some(("structured_outcome", json!({}))),
        Some(("finished_at_ms", json!(123))),
    ] {
        let malformed = mutation.is_some();
        let data_root = short_data_root()?;
        let (error, exchanges) = foreground_transport_fixture(
            data_root.path(),
            move |request| {
                let id = request["request_id"].as_str().context("client ID")?;
                Ok(if request["op"] == SOURCE_REFRESH_REQUEST_OP {
                    json!({"ok":true,"owner":"daemon","request_id":id,"request_state":"admission_pending","schema_version":1})
                } else {
                    let mut response = json!({"ok":false,"owner":"daemon","request_id":id,
                        "request_state":"request_unknown","error_code":"source_refresh_request_unknown",
                        "reason":"request_not_retained_after_restart","retryable":false,"schema_version":1,
                        "error":"arbitrary human text cannot authorize recovery",
                        "diagnostic_extension":"informational"});
                    if let Some((field, value)) = &mutation {
                        response[*field] = value.clone();
                    }
                    response
                })
            },
            || {
                coordinate_source_backed_refresh(
                    &RecordingAvailability::default(),
                    data_root.path(),
                    SourceBackedRefreshMode::Wait,
                )
                .err()
                .expect("unobservable or mismatched request must fail")
            },
        )?;
        assert_eq!(exchanges.len(), if malformed { 2 } else { 4 });
        if !malformed {
            assert_eq!(exchanges[0].0, exchanges[2].0);
            let typed = error
                .downcast_ref::<SourceRefreshObservationRecoveryFailed>()
                .expect("bounded unknown recovery");
            assert_eq!(
                typed.request_id,
                exchanges[0].0["request_id"].as_str().unwrap()
            );
            assert_eq!(typed.recovery_attempts, 1);
        }
    }
    Ok(())
}

#[test]
fn manual_background_reads_published_generation_without_ipc_admission() -> Result<()> {
    struct ManualAvailability;
    impl crate::DaemonAvailabilityPort for ManualAvailability {
        fn ensure_available(
            &self,
            _data_root: &Path,
            trigger: crate::DaemonTrigger,
            demand: crate::DaemonAvailabilityDemand,
        ) -> Result<crate::DaemonAvailability> {
            assert_eq!(trigger, crate::DaemonTrigger::Search);
            assert_eq!(demand, crate::DaemonAvailabilityDemand::Background);
            Ok(crate::DaemonAvailability::Disabled)
        }
    }
    let data_root = short_data_root()?;
    ctx_history_platform::platform_security::establish_private_data_root(data_root.path())?;
    let generation = super::publish_authoritative_empty_generation_for_test(
        &source_backed_index_root(data_root.path()),
        "manual-background-fixture",
        ctx_history_refresh::RefreshOperation::Refresh,
        ctx_history_capture::SourceBackedRefreshScope::All,
        None,
    )?
    .generation_id;
    let (observation, exchanges) = foreground_transport_fixture(
        data_root.path(),
        |_| bail!("manual background must not contact the endpoint"),
        || {
            coordinate_source_backed_refresh(
                &ManualAvailability,
                data_root.path(),
                SourceBackedRefreshMode::Background,
            )
        },
    )?;
    let observation = observation?;
    assert!(exchanges.is_empty());
    assert!(observation.request_id.is_none());
    assert!(!observation.daemon_available);
    assert_eq!(observation.pin.generation_id(), generation);
    assert!(!crate::paths_status::daemon_source_backed_refresh_job_path(data_root.path()).exists());
    Ok(())
}

#[test]
fn optional_background_queue_full_reads_pin_without_acceptance() -> Result<()> {
    for retain_peer in [false, true] {
        let data_root = short_data_root()?;
        ctx_history_platform::platform_security::establish_private_data_root(data_root.path())?;
        let generation = super::publish_authoritative_empty_generation_for_test(
            &source_backed_index_root(data_root.path()),
            "background-overload-fixture",
            ctx_history_refresh::RefreshOperation::Refresh,
            ctx_history_capture::SourceBackedRefreshScope::All,
            None,
        )?
        .generation_id;
        let engine = Arc::new(CoreRefreshEngine::new());
        for index in 0..8 {
            let response = engine
                .handle_ipc_request(
                    data_root.path(),
                    &json!({
                        "op": SOURCE_REFRESH_REQUEST_OP, "mode": "background", "trigger": "search",
                        "request_id": format!("019fcaaa-0000-7000-8000-{index:012}"),
                        "refresh_intent": {"kind": "automatic_maintenance"},
                    }),
                )?
                .context("background admission")?;
            assert_eq!(response["ok"], true);
        }
        let job_path = crate::paths_status::daemon_source_backed_refresh_job_path(data_root.path());
        let before = std::fs::read(&job_path)?;
        let server_engine = Arc::clone(&engine);
        let server_root = data_root.path().to_owned();
        let availability = RecordingAvailability::default();
        let (outcome, exchanges) = foreground_transport_fixture(
            data_root.path(),
            move |request| {
                server_engine
                    .handle_ipc_request(&server_root, request)?
                    .context("wire response")
            },
            || {
                coordinate_source_backed_refresh_with_policy(
                    &availability,
                    data_root.path(),
                    SourceBackedRefreshMode::Background,
                    SourceBackedRefreshRequestPolicy::refresh(RefreshRequestTrigger::Search),
                    retain_peer,
                    None,
                )
            },
        )?;
        let outcome = outcome?;
        assert_eq!(
            exchanges.len(),
            1,
            "rejected optional work does not retry or poll"
        );
        assert_eq!(exchanges[0].1["error_code"], "source_refresh_queue_full");
        assert!(engine
            .status(exchanges[0].0["request_id"].as_str().unwrap())
            .is_none());
        assert_eq!(outcome.status, "admission_rejected");
        assert_eq!(outcome.mode, SourceBackedRefreshMode::Background);
        assert!(outcome.daemon_available);
        assert!(outcome.request_id.is_none());
        assert!(outcome.receipt.is_none());
        assert!(outcome.scanned_routes.is_none());
        assert!(!outcome.request_generation_changed);
        assert_eq!(outcome.pin.generation_id(), generation);
        assert_eq!(std::fs::read(&job_path)?, before);
        assert_eq!(availability.0.lock().unwrap().len(), 1);
    }
    Ok(())
}

fn queue_full_response_for_test() -> Value {
    json!({
        "ok": false, "schema_version": 1, "owner": "daemon", "status": "busy",
        "error_code": "source_refresh_queue_full", "reason": "queue_full", "retryable": true,
        "active_pending_requests": 8, "max_active_pending_requests": 8,
        "error": "source refresh queue is full",
    })
}

#[test]
fn optional_background_does_not_hide_cold_or_wait_import_overload() -> Result<()> {
    for (warm, mode, intent, trigger) in [
        (
            false,
            SourceBackedRefreshMode::Background,
            RefreshIntent::AutomaticMaintenance,
            RefreshRequestTrigger::Search,
        ),
        (
            true,
            SourceBackedRefreshMode::Wait,
            RefreshIntent::AutomaticMaintenance,
            RefreshRequestTrigger::Search,
        ),
        (
            true,
            SourceBackedRefreshMode::Wait,
            RefreshIntent::SelectedImport(RefreshSelection::All),
            RefreshRequestTrigger::Import,
        ),
    ] {
        let data_root = short_data_root()?;
        ctx_history_platform::platform_security::establish_private_data_root(data_root.path())?;
        if warm {
            super::publish_authoritative_empty_generation_for_test(
                &source_backed_index_root(data_root.path()),
                "overload-control-fixture",
                ctx_history_refresh::RefreshOperation::Refresh,
                ctx_history_capture::SourceBackedRefreshScope::All,
                None,
            )?;
        }
        let (outcome, exchanges) = foreground_transport_fixture(
            data_root.path(),
            |_| Ok(queue_full_response_for_test()),
            || {
                coordinate_source_backed_refresh_with_policy(
                    &RecordingAvailability::default(),
                    data_root.path(),
                    mode,
                    SourceBackedRefreshRequestPolicy {
                        intent,
                        trigger,
                        allow_daemon_autostart: false,
                    },
                    false,
                    None,
                )
            },
        )?;
        let error = outcome.err().context("overload must remain an error")?;
        assert!(error.to_string().contains("source refresh queue is full"));
        assert!(error
            .downcast_ref::<SourceBackedRefreshPendingPublication>()
            .is_none());
        assert_eq!(exchanges.len(), 1);
        assert_eq!(exchanges[0].1["error_code"], "source_refresh_queue_full");
    }
    Ok(())
}

#[test]
fn optional_background_rejects_malformed_or_indeterminate_admission_denials() -> Result<()> {
    let data_root = short_data_root()?;
    ctx_history_platform::platform_security::establish_private_data_root(data_root.path())?;
    super::publish_authoritative_empty_generation_for_test(
        &source_backed_index_root(data_root.path()),
        "malformed-overload-fixture",
        ctx_history_refresh::RefreshOperation::Refresh,
        ctx_history_capture::SourceBackedRefreshScope::All,
        None,
    )?;
    for (field, value) in [
        ("owner", json!("other")),
        ("schema_version", json!(2)),
        ("ok", json!(true)),
        ("status", json!("queued")),
        ("error_code", Value::Null),
        ("reason", json!("unknown")),
        ("retryable", json!(false)),
        ("request_id", json!("different-request")),
        ("request_state", json!("admission_pending")),
        (
            "admission_durability",
            json!("replacement_visible_or_indeterminate"),
        ),
        (
            "admission_acknowledgement",
            json!("retained_after_durability_error"),
        ),
        ("active_pending_requests", json!(7)),
        ("max_active_pending_requests", Value::Null),
        ("error_code", json!("source_refresh_admission_unconfirmed")),
        ("receipt", json!({})),
        ("receipt", Value::Null),
        ("published_generation", json!("generation")),
        ("previous_generation", json!("previous")),
        ("generation_changed", json!(false)),
        ("outcome", json!("published")),
        ("structured_outcome", json!({})),
        ("finished_at_ms", json!(123)),
    ] {
        let mut response = queue_full_response_for_test();
        response[field] = value;
        assert!(
            background_admission_rejected_fallback(
                data_root.path(),
                SourceBackedRefreshMode::Background,
                &RefreshIntent::AutomaticMaintenance,
                false,
                &response,
            )?
            .is_none(),
            "{field}: {response}"
        );
    }
    let mut extended = queue_full_response_for_test();
    extended["diagnostic_extension"] = json!("informational");
    assert!(background_admission_rejected_fallback(
        data_root.path(),
        SourceBackedRefreshMode::Background,
        &RefreshIntent::AutomaticMaintenance,
        false,
        &extended,
    )?
    .is_some());
    assert!(background_admission_rejected_fallback(
        data_root.path(),
        SourceBackedRefreshMode::Background,
        &RefreshIntent::SelectedImport(RefreshSelection::All),
        false,
        &queue_full_response_for_test(),
    )?
    .is_none());
    Ok(())
}

#[path = "discovered_batch_tests.rs"]
mod discovered_batch_tests;
