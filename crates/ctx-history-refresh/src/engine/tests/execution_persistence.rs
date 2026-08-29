use super::*;

#[test]
fn production_run_persists_discovering_before_executor_entry() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let observed = Arc::new(AtomicBool::new(false));
    let observed_from_executor = Arc::clone(&observed);
    let coordinator = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let job =
                read_daemon_job_status(&daemon_source_backed_refresh_job_path(execution.data_root))
                    .expect("running source refresh status");
            assert_eq!(job["request_state"], "running");
            assert_eq!(job["progress"]["phase"], "discovering");
            assert_eq!(job["progress"]["total_sources_known"], false);
            assert!(job["progress"]["current_source"].is_null());
            assert!(job["progress"]["completed_records"].is_null());
            assert!(job["progress"]["completed_bytes"].is_null());
            assert!(job["progress"]["current_source_progress"].is_null());
            observed_from_executor.store(true, Ordering::SeqCst);
            Err(anyhow!("stop after observing persisted discovery phase"))
        },
    ));
    let _request = manual_all_request_without_catalog(&coordinator, &data_root);

    let run = coordinator.run_next(&data_root).expect("queued refresh");
    assert!(run.failed);
    assert!(observed.load(Ordering::SeqCst));
}

#[test]
fn default_executor_uses_capture_owned_execution() {
    let coordinator = CoreRefreshEngine::new();
    assert_eq!(
        coordinator.executor.implementation_name(),
        std::any::type_name::<CaptureOwnedSourceBackedRefreshExecutor>()
    );
}
