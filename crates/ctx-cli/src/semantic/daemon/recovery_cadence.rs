use super::*;
use crate::semantic::source_backed_refresh_coordinator::{
    coordinate_source_backed_refresh, SourceBackedRefreshMode, SourceBackedRefreshReceipt,
};
use ctx_history_index::IndexError;

#[test]
fn recovered_periodic_publication_restores_crash_cooldown_before_explicit_bypass() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root)?;
    let interrupted = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let request_id = execution.request_id.to_owned();
            let published = GenerationWriter::open(execution.index_root, WriterOptions::default())?
                .commit_with_publication_metadata(
                    |_| true,
                    |context| {
                        let generation_id = context.generation_id().to_owned();
                        let receipt = SourceBackedRefreshReceipt {
                            previous_generation: None,
                            published_generation: generation_id.clone(),
                            generation_changed: true,
                            published_explicit_source_catalog: None,
                            current: SourceBackedRefreshCurrent::default(),
                            route_results: Vec::new(),
                            catalog_route_bindings: Vec::new(),
                        };
                        serde_json::to_vec(&json!({
                            "version": 1,
                            "request_id": request_id,
                            "operation": "refresh",
                            "refresh_scope": {"kind": "all"},
                            "receipt": receipt.to_json(),
                            "route_observations": [],
                        }))
                        .map_err(|error| IndexError::PublicationMetadata(error.to_string()))
                    },
                )?;
            Err(anyhow!(
                "injected crash after automatic publication {}",
                published.receipt().generation_id
            ))
        },
    ));
    interrupted.enqueue_periodic(&data_root)?;
    let failed = interrupted
        .run_next(&data_root)
        .expect("interrupted automatic publication");
    assert!(failed.failed);
    let interrupted_status = read_daemon_job_status(&daemon_core_refresh_job_path(&data_root))
        .expect("interrupted automatic status");
    assert_eq!(interrupted_status["trigger"], "periodic");
    assert_eq!(interrupted_status["trigger_provenance"], "daemon_scheduler");
    let interrupted_request_id = interrupted_status["request_id"]
        .as_str()
        .expect("interrupted request ID")
        .to_owned();
    drop(interrupted);

    assert!(
        coordinate_source_backed_refresh(&data_root, SourceBackedRefreshMode::Off).is_ok(),
        "committed generation must remain readable at the crash point"
    );

    let calls = Arc::new(AtomicUsize::new(0));
    let executor_calls = Arc::clone(&calls);
    let coordinator = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            executor_calls.fetch_add(1, Ordering::SeqCst);
            let receipt = GenerationWriter::open(execution.index_root, WriterOptions::default())?
                .commit(|_| true)?;
            Ok(SourceBackedRefreshPublication {
                generation_id: receipt.generation_id,
                published_explicit_source_catalog: None,
                unsupported_routes: 0,
                certified_source_count: 0,
                certified_source_bytes: 0,
                current: SourceBackedRefreshCurrent::default(),
                timings: SourceBackedRefreshTimings::default(),
                route_results: Vec::new(),
                catalog_route_bindings: Vec::new(),
                verified_index: None,
            })
        },
    ));
    let mut runtime = DaemonRuntime::default();
    recover_source_refresh_before_background_cadence(&mut runtime, &data_root, Some(&coordinator))?;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let recovered_status = read_daemon_job_status(&daemon_core_refresh_job_path(&data_root))
        .expect("actual recovered automatic status");
    assert_eq!(recovered_status["request_id"], interrupted_request_id);
    assert_eq!(recovered_status["request_state"], "published");
    assert_eq!(recovered_status["status"], "completed");
    assert_eq!(recovered_status["trigger"], "recovery");
    assert_eq!(recovered_status["trigger_provenance"], "commit_payload");
    assert!(
        runtime
            .background_refresh_cadence
            .remaining(Instant::now())
            .is_some_and(|remaining| remaining > StdDuration::ZERO),
        "actual recovered automatic publication must retain background cooldown"
    );

    coordinator
        .handle_ipc_request(
            &data_root,
            &json!({
                "schema_version": 1,
                "op": "source_refresh_request",
                "mode": "wait",
                "operation": "refresh",
            }),
        )?
        .expect("explicit freshness response");

    let iteration = run_daemon_scheduler_cycle_with_activity(
        &test_daemon_run_args(),
        &data_root,
        &mut runtime,
        None,
        false,
        None,
        Some(&coordinator),
    )?;
    assert!(!iteration.failed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!coordinator.has_pending_request());
    Ok(())
}
