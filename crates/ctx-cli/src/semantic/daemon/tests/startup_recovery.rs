use super::*;

#[cfg(any(unix, windows))]
#[test]
fn interrupted_queue_is_recovered_before_endpoint_accepts_a_new_admission() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root)?;

    let interrupted = CoreRefreshEngine::new();
    let predecessor = interrupted.enqueue_periodic(&data_root)?;
    let predecessor_id = predecessor["request_id"]
        .as_str()
        .expect("interrupted request ID")
        .to_owned();
    interrupted.persist_job_status_for_test(&data_root, &predecessor_id)?;
    drop(interrupted);
    assert!(!daemon_service_endpoint_path(&data_root, DaemonIpcService::SourceRefresh).exists());

    let config = AppConfig::default();
    let mut runtime = DaemonRuntime {
        config: config.clone(),
        ..DaemonRuntime::default()
    };
    let startup_source_refresh =
        recover_source_refresh_coordinator_before_ipc(&mut runtime, &data_root)?;
    assert_eq!(
        startup_source_refresh
            .status_for_test(&predecessor_id)
            .unwrap()["request_state"],
        "queued"
    );
    assert!(!daemon_service_endpoint_path(&data_root, DaemonIpcService::SourceRefresh).exists());

    let args = DaemonRunArgs {
        foreground: false,
        idle_exit_seconds: None,
        loop_interval_seconds: None,
        max_chunks: None,
        max_seconds: None,
        force: false,
        start_mode: Some(DaemonStartModeArg::Auto),
        trigger_command: Some(DaemonTriggerCommandArg::Search),
        format: crate::output::JsonOutputFormat::Json,
    };
    let wakeup = Arc::new(DaemonWakeup::default());
    let mut query_service = None;
    let mut refresh_service = None;
    let mut reload = DaemonConfigReloadState::pending(&config);
    assert_eq!(
        reload_daemon_runtime_config(
            &data_root,
            &args,
            &mut runtime,
            &mut query_service,
            &mut refresh_service,
            &mut reload,
            &wakeup,
        ),
        DaemonConfigReloadOutcome::Continue
    );
    let service = refresh_service.as_ref().expect("source refresh service");
    assert!(Arc::ptr_eq(
        &service.source_refresh,
        &startup_source_refresh
    ));
    assert!(daemon_service_endpoint_path(&data_root, DaemonIpcService::SourceRefresh).exists());

    let successor_id = "019fcaaa-0000-7000-8000-000000000308";
    let acknowledgement = daemon_source_refresh_request(
        &data_root,
        compact_json(json!({
            "schema_version": 1,
            "op": "source_refresh_request",
            "request_id": successor_id,
            "mode": "wait",
            "operation": "refresh",
            "fresh_after_admitted_snapshot": true,
        })),
        StdDuration::from_secs(1),
        64 * 1024,
    )?
    .expect("new admission after recovery");
    assert_eq!(acknowledgement["coalesced_into_request_id"], predecessor_id);

    let durable = read_daemon_job_status(&daemon_core_refresh_job_path(&data_root))
        .expect("recovered root plus new admission");
    assert_eq!(durable["request_id"], predecessor_id);
    assert_eq!(durable["queued_successors"][0]["request_id"], successor_id);
    drop(query_service);
    drop(refresh_service);
    Ok(())
}
