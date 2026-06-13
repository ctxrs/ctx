use super::*;

#[tokio::test]
async fn sweep_provider_workers_once_dedupes_shared_adapters_and_aggregates_stats() {
    let temp = tempdir().unwrap();
    let stores = StoreManager::open(temp.path()).await.unwrap();
    let shared_adapter = Arc::new(RecordingProviderAdapter::default());
    shared_adapter.set_reap_result(ProviderSessionSweepStats {
        reaped: 1,
        skipped_busy: 2,
        dead_removed: 0,
        status_errors: 0,
    });
    let other_adapter = Arc::new(RecordingProviderAdapter::default());
    other_adapter.set_reap_result(ProviderSessionSweepStats {
        reaped: 0,
        skipped_busy: 0,
        dead_removed: 1,
        status_errors: 1,
    });

    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    providers.insert("root".into(), shared_adapter.clone());
    providers.insert("other".into(), other_adapter.clone());
    let state = Arc::new(DaemonState::new(
        temp.path().to_path_buf(),
        stores,
        providers,
        "http://localhost".to_string(),
        None,
    ));
    state
        .providers
        .upsert_target_provider_adapter("root@host".into(), shared_adapter.clone())
        .await;

    let config = ProviderSessionSweepConfig {
        idle_ttl: Duration::from_secs(7),
        max_idle_sessions: 3,
        interval: Duration::from_secs(11),
    };
    let stats = state.providers.sweep_provider_workers_once(config).await;

    assert_eq!(
        stats,
        ProviderSessionSweepStats {
            reaped: 1,
            skipped_busy: 2,
            dead_removed: 1,
            status_errors: 1,
        }
    );
    assert_eq!(shared_adapter.reap_calls().len(), 1);
    assert_eq!(other_adapter.reap_calls().len(), 1);
    assert_eq!(shared_adapter.reap_calls()[0].idle_ttl, config.idle_ttl);
    assert_eq!(
        shared_adapter.reap_calls()[0].max_idle_sessions,
        config.max_idle_sessions
    );
    assert_eq!(shared_adapter.reap_calls()[0].interval, config.interval);
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn shutdown_shared_substrate_requests_save_or_stop_when_shared_backend_available() {
    let _serial = sandbox_cli_env_test_lock().lock().await;
    let temp = tempdir().unwrap();
    let stores = StoreManager::open(temp.path()).await.unwrap();
    let providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    let state = Arc::new(DaemonState::new(
        temp.path().to_path_buf(),
        stores,
        providers,
        "http://localhost".to_string(),
        None,
    ));
    let (helper_path, log_path) = write_shared_vm_shutdown_helper(temp.path());
    let _helper_guard = EnvGuard::set(
        ctx_avf_linux_runtime::AVF_LINUX_HELPER_PATH_ENV,
        &helper_path.to_string_lossy(),
    );
    let _sandbox_cli_override = EnvGuard::remove(CTX_HARNESS_SANDBOX_CLI_PATH_ENV);

    let record = crate::daemon::lifecycle::shutdown_shared_substrate(&state, "test shutdown")
        .await
        .expect("shared substrate shutdown")
        .expect("shared substrate record");

    assert_eq!(
        record.shutdown_outcome,
        Some(ctx_avf_linux_runtime::SubstrateShutdownOutcome::Saved)
    );
    assert_eq!(record.shutdown_reason, None);
    assert!(!record.save_error_present);
    assert!(record.saved_state_written_on_shutdown);

    let log = std::fs::read_to_string(log_path).expect("read helper log");
    assert!(
        log.lines()
            .any(|line| line == format!("stop-workspace-vm {}", temp.path().display())),
        "expected stop-workspace-vm invocation in log:\n{log}"
    );
}
