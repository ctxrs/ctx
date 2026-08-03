use super::*;

#[test]
fn scheduler_retries_terminal_status_without_republishing_core() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let executions = Arc::new(AtomicUsize::new(0));
    let execution_count = Arc::clone(&executions);
    let executor = Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
        execution_count.fetch_add(1, Ordering::SeqCst);
        let commit = GenerationWriter::open(execution.index_root, WriterOptions::default())?
            .commit(|_| true)?;
        Ok(SourceBackedRefreshPublication {
            generation_id: commit.generation_id,
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
    });
    let terminal_failures = Arc::new(AtomicUsize::new(0));
    let failures = Arc::clone(&terminal_failures);
    let writer = Arc::new(move |path: &Path, job: &Value| {
        if job.get("request_state").and_then(Value::as_str) == Some("published")
            && failures.fetch_add(1, Ordering::SeqCst) == 0
        {
            anyhow::bail!("injected scheduler terminal persistence failure");
        }
        write_daemon_job_status(path, job)
    });
    let coordinator = CoreRefreshEngine::with_status_writer_for_test(executor, writer);
    coordinator.enqueue_periodic(&data_root).unwrap();
    let mut runtime = DaemonRuntime::default();

    let first = run_pending_core_refresh(&data_root, &mut runtime, Some(&coordinator))
        .unwrap()
        .expect("first scheduler refresh");
    assert!(first.failed);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(
        read_daemon_job_status(&daemon_core_refresh_job_path(&data_root)).unwrap()["request_state"],
        "running"
    );

    let retry = run_pending_core_refresh(&data_root, &mut runtime, Some(&coordinator))
        .unwrap()
        .expect("terminal persistence retry");
    assert!(!retry.failed);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    let terminal = read_daemon_job_status(&daemon_core_refresh_job_path(&data_root)).unwrap();
    assert_eq!(terminal["request_state"], "published");
    assert!(terminal.get("last_error").is_none());
    assert!(terminal.get("failure_type").is_none());
}
