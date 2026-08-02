use super::client::{source_refresh_request_is_unknown, wait_for_published_generation};
use super::*;

use crate::semantic::{
    model_runtime::SharedSemanticRuntime,
    query_service::{
        daemon_source_refresh_request, start_daemon_source_refresh_service_with_request_timeout,
    },
};

#[cfg(any(unix, windows))]
#[test]
fn old_wait_request_recovers_typed_across_restart_and_returns_exact_generation() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let authority = load_explicit_source_catalog_authority(&data_root).unwrap();

    let prior_process = CoreRefreshEngine::new();
    let old = prior_process
        .handle_ipc_request(
            &data_root,
            &json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "explicit_source_catalog": authority.to_json(),
                "fail_on_source_failure": true,
            }),
        )
        .unwrap()
        .expect("prior-process request");
    let old_request_id = old["request_id"].as_str().unwrap().to_owned();
    drop(prior_process);

    let service = start_daemon_source_refresh_service_with_request_timeout(
        &data_root,
        SharedSemanticRuntime::default(),
        StdDuration::from_millis(100),
    )
    .unwrap();
    let active = service
        .source_refresh
        .handle_ipc_request(
            &data_root,
            &json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "explicit_source_catalog": authority.to_json(),
                "fail_on_source_failure": true,
            }),
        )
        .unwrap()
        .expect("restarted-process equivalent request");
    let active_request_id = active["request_id"].as_str().unwrap().to_owned();
    assert_ne!(active_request_id, old_request_id);

    let unknown = daemon_source_refresh_request(
        &data_root,
        compact_json(json!({
            "schema_version": 1,
            "op": SOURCE_REFRESH_STATUS_OP,
            "request_id": old_request_id,
        })),
        StdDuration::from_secs(1),
        SOURCE_REFRESH_RESPONSE_MAX_BYTES,
    )
    .unwrap()
    .expect("typed unknown-request response");
    assert_eq!(unknown["ok"], false);
    assert_eq!(unknown["schema_version"], 1);
    assert_eq!(unknown["owner"], "daemon");
    assert_eq!(unknown["request_id"], old_request_id);
    assert_eq!(
        unknown["request_state"],
        SOURCE_REFRESH_UNKNOWN_REQUEST_STATE
    );
    assert_eq!(
        unknown["error_code"],
        SOURCE_REFRESH_UNKNOWN_REQUEST_ERROR_CODE
    );
    assert_eq!(unknown["retryable"], true);
    let mut changed_error_text = unknown.clone();
    changed_error_text["error"] = json!("arbitrary localized detail");
    assert!(source_refresh_request_is_unknown(&changed_error_text, &old_request_id).unwrap());
    assert!(source_refresh_request_is_unknown(&changed_error_text, "different-request").is_err());

    let terminal_generation = Arc::new(Mutex::new(None::<String>));
    let observation = std::thread::scope(|scope| {
        let waiter_root = data_root.clone();
        let waiter_authority = authority.clone();
        let waiter_old_request_id = old_request_id.clone();
        let waiter = scope.spawn(move || {
            wait_for_published_generation(
                &waiter_root,
                waiter_old_request_id,
                SourceBackedRefreshMode::Wait,
                Some(&waiter_authority),
                false,
                true,
            )
            .unwrap()
        });

        let started = StdInstant::now();
        loop {
            let status = service
                .source_refresh
                .status(&active_request_id)
                .expect("active restarted request");
            if status["coalesced_requests"].as_u64() == Some(1) {
                break;
            }
            assert!(
                started.elapsed() < StdDuration::from_secs(2),
                "waiter did not attach to equivalent restarted work"
            );
            std::thread::sleep(StdDuration::from_millis(5));
        }
        assert_eq!(
            service
                .source_refresh
                .request_authority_for_test(&active_request_id),
            Some((authority.clone(), true))
        );

        let execute_generation = Arc::clone(&terminal_generation);
        let execute_authority = authority.clone();
        let probe_generation = Arc::clone(&terminal_generation);
        let run = service
            .source_refresh
            .run_next_with(
                move |_, _| {
                    let writer = ctx_history_index::GenerationWriter::open(
                        source_backed_index_root(&data_root),
                        WriterOptions::default(),
                    )?;
                    let commit = writer.commit(|_| true)?;
                    *execute_generation.lock().unwrap() = Some(commit.generation_id.clone());
                    let mut publication = SourceBackedRefreshPublication {
                        generation_id: commit.generation_id,
                        published_explicit_source_catalog: execute_authority,
                        scanned_routes: 0,
                        unsupported_routes: 0,
                        certified_source_count: 0,
                        certified_source_bytes: 0,
                        current: SourceBackedRefreshCurrent::default(),
                        timings: SourceBackedRefreshTimings::default(),
                        selected_route_ids: Vec::new(),
                        successful_route_ids: Vec::new(),
                        source_failures: Vec::new(),
                    };
                    publication.current.removed_source_count = 0;
                    Ok(publication)
                },
                move || Ok(probe_generation.lock().unwrap().clone()),
                |_| Ok(()),
                |_| Ok(()),
            )
            .expect("restarted terminal refresh");
        assert!(!run.failed);
        waiter.join().unwrap()
    });

    let expected_generation = terminal_generation
        .lock()
        .unwrap()
        .clone()
        .expect("terminal generation");
    assert_eq!(
        observation.request_id.as_deref(),
        Some(active_request_id.as_str())
    );
    assert_eq!(observation.pin.generation_id(), expected_generation);
    assert_eq!(
        observation
            .receipt
            .as_ref()
            .map(|receipt| receipt.published_generation.as_str()),
        Some(expected_generation.as_str())
    );
    assert_eq!(
        observation
            .receipt
            .as_ref()
            .map(|receipt| &receipt.published_explicit_source_catalog),
        Some(&authority)
    );
    drop(service);
}
