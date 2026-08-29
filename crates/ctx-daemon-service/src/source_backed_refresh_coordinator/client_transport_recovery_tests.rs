use super::observation_recovery::SourceRefreshObservationRecoveryFailed;
use super::*;

use std::{
    io::{Read as _, Write as _},
    os::unix::net::UnixListener,
};

use ctx_history_core::CaptureProvider;

use crate::query_service::{
    ctx_authenticated_request_handler, start_daemon_source_refresh_service_with_request_timeout,
    write_daemon_service_endpoint, DaemonIpcService, DaemonQueryEndpoint,
};
use crate::SharedSemanticRuntime;

const TEST_ENDPOINT_TOKEN: &str = "0123456789abcdef0123456789abcdef";

fn short_data_root() -> Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix("ctx-refresh-")
        .tempdir_in("/tmp")
        .map_err(Into::into)
}

fn source_refresh_endpoint(socket_path: &Path) -> DaemonQueryEndpoint {
    DaemonQueryEndpoint::Unix {
        path: socket_path.to_owned(),
        token: TEST_ENDPOINT_TOKEN.to_owned(),
    }
}

#[test]
fn background_maintenance_wake_is_accepted_through_client_and_coordinator() -> Result<()> {
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
        None,
    )?;

    assert_eq!(observation.mode, SourceBackedRefreshMode::Background);
    assert_eq!(observation.status, "queued");
    assert!(observation.daemon_available);
    assert!(observation.request_id.is_some());
    assert_eq!(observation.pin.generation_id(), generation);
    assert!(!coordinator.has_pending_request());
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

#[test]
fn acknowledged_typed_unknown_does_not_reenqueue_equivalent_work() -> Result<()> {
    let data_root = short_data_root()?;
    let socket_path = data_root.path().join("acknowledged-typed-unknown.sock");
    let listener = UnixListener::bind(&socket_path)?;
    write_daemon_service_endpoint(
        data_root.path(),
        DaemonIpcService::SourceRefresh,
        &source_refresh_endpoint(&socket_path),
    )?;
    let request_id = "019fcaaa-0000-7000-8000-000000000307";
    let server = std::thread::spawn(move || -> Result<Vec<u8>> {
        let (mut stream, _) = listener.accept()?;
        let mut request = Vec::new();
        stream.read_to_end(&mut request)?;
        stream.write_all(
            format!(
                "{{\"ok\":false,\"owner\":\"daemon\",\"request_id\":\"{request_id}\",\"request_state\":\"request_unknown\",\"error_code\":\"source_refresh_request_unknown\",\"retryable\":true,\"schema_version\":1}}\n"
            )
            .as_bytes(),
        )?;
        Ok(request)
    });

    let error = match wait_for_published_generation(
        data_root.path(),
        request_id.to_owned(),
        SourceBackedRefreshMode::Wait,
        ctx_history_refresh::RefreshOperation::Refresh,
        None,
        false,
    ) {
        Ok(_) => panic!("typed unknown after acknowledgement must not publish or reenqueue"),
        Err(error) => error,
    };
    let request = server.join().expect("typed-unknown test server panicked")?;

    let typed = error
        .downcast_ref::<SourceRefreshObservationRecoveryFailed>()
        .expect("post-ack typed unknown must remain request-bound and unobservable");
    assert_eq!(typed.request_id, request_id);
    assert_eq!(typed.recovery_attempts, 0);
    assert_eq!(typed.disconnect_policy, DISCONNECT_POLICY);

    let request: Value = serde_json::from_slice(&request)?;
    assert_eq!(request["op"], SOURCE_REFRESH_STATUS_OP);
    assert_eq!(request["request_id"], request_id);
    Ok(())
}
