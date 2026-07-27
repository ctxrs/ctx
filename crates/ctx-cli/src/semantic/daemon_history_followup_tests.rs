use super::*;

#[test]
fn daemon_capture_only_progress_keeps_followup_frontier_alive() {
    let mut summary = ctx_history_capture::ProviderImportSummary::default();
    summary.failed = 1;
    summary.work_remaining = true;
    let mut totals = ImportTotals::default();
    totals.add_rejected_source(&summary, &crate::commands::import::SourceStats::default());
    assert!(totals.capture_work_remaining);
    assert_eq!(totals.imported_sessions, 0);
    assert_eq!(totals.imported_events, 0);
    assert_eq!(totals.imported_edges, 0);
    assert!(
        import_totals_json(&totals)
            .get("capture_work_remaining")
            .is_none(),
        "the internal scheduler signal must not change public import JSON"
    );

    let mut capture_only_group = daemon_history_refresh_job_json(
        "completed",
        1,
        totals,
        utc_now().timestamp_millis(),
        None,
        None,
    );
    capture_only_group["capture_work_remaining"] = Value::Bool(true);
    capture_only_group["discovered_source_count"] = json!(3);
    assert!(!daemon_history_refresh_job_did_work(&capture_only_group));

    let mut runtime = DaemonRuntime::default();
    assert!(finish_daemon_history_refresh_job(
        &mut runtime,
        &mut capture_only_group
    ));
    assert_eq!(runtime.history_followup_passes_remaining, 3);
    assert_eq!(capture_only_group["scheduler_followup_passes_remaining"], 3);

    for expected_remaining in [2, 1, 0] {
        let mut no_insert_group = daemon_history_completed_test_job();
        assert!(!daemon_history_refresh_job_did_work(&no_insert_group));
        let did_work = finish_daemon_history_refresh_job(&mut runtime, &mut no_insert_group);
        assert_eq!(
            runtime.history_followup_passes_remaining,
            expected_remaining
        );
        assert_eq!(did_work, expected_remaining > 0);
    }
}

#[test]
fn daemon_history_rejections_fail_the_pass_without_hiding_completed_work() {
    let totals = ImportTotals {
        failed: 1,
        imported_events: 2,
        ..ImportTotals::default()
    };
    let job = daemon_history_refresh_job_json(
        "completed",
        1,
        totals,
        utc_now().timestamp_millis(),
        None,
        None,
    );

    assert_eq!(job["status"], "completed");
    assert_eq!(job["totals"]["imported_events"], 2);
    assert_eq!(job["totals"]["rejected_records"], 1);
    assert!(daemon_history_job_failed(&job));
}

#[test]
fn daemon_failure_drains_the_current_round_before_global_backoff() -> Result<()> {
    let mut runtime = DaemonRuntime::default();
    let mut failed_source = daemon_history_refresh_failed_job("source-a failed".to_owned());
    failed_source["scheduler_source_index"] = json!(0);
    failed_source["discovered_source_count"] = json!(3);
    let mut failed_source = record_daemon_history_job_retry(&mut runtime, failed_source);

    assert_eq!(runtime.history_retry_drain_passes_remaining, 2);
    assert!(!daemon_history_retry_blocks_scheduler(&runtime));
    assert!(finish_daemon_history_refresh_job(
        &mut runtime,
        &mut failed_source
    ));

    let temp = tempfile::tempdir()?;
    write_daemon_job_status(
        &daemon_history_refresh_job_path(temp.path()),
        &failed_source,
    )?;
    let mut restarted = DaemonRuntime::default();
    restore_daemon_history_runtime_state(&mut restarted, temp.path());
    assert_eq!(restarted.history_retry_drain_passes_remaining, 2);
    assert!(!restarted.history_retry.ready());
    assert!(!daemon_history_retry_blocks_scheduler(&restarted));

    for (source_index, expected_remaining, expected_work) in [(1, 1, true), (2, 0, false)] {
        let mut healthy_source = daemon_history_completed_test_job();
        healthy_source["scheduler_source_index"] = json!(source_index);
        healthy_source["discovered_source_count"] = json!(3);
        let mut healthy_source = record_daemon_history_job_retry(&mut restarted, healthy_source);
        assert_eq!(
            finish_daemon_history_refresh_job(&mut restarted, &mut healthy_source),
            expected_work
        );
        assert_eq!(
            restarted.history_retry_drain_passes_remaining,
            expected_remaining
        );
    }
    assert!(daemon_history_retry_blocks_scheduler(&restarted));
    assert!(!history_retry_due(&restarted));
    restarted.history_retry.retry_not_before = Some(Instant::now() - StdDuration::from_millis(1));
    assert!(history_retry_due(&restarted));
    assert!(!daemon_history_retry_blocks_scheduler(&restarted));
    Ok(())
}

#[cfg(ctx_semantic_fastembed)]
#[test]
fn daemon_prioritizes_semantic_bootstrap_over_history_refresh() -> Result<()> {
    let temp = tempfile::tempdir()?;
    write_semantic_enabled_config(temp.path())?;
    write_test_semantic_cache(&temp.path().join("semantic-model-cache"))?;
    write_searchable_store(temp.path(), SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT + 1)?;
    let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let _hooks = install_test_daemon_jobs(
        calls.clone(),
        Some(daemon_history_completed_test_job()),
        Some(daemon_semantic_indexed_test_job(temp.path())),
    );

    let mut runtime = DaemonRuntime {
        history_source_cursor: 7,
        ..DaemonRuntime::default()
    };
    runtime.history_retry.record_failure();
    let iteration = run_daemon_once(
        &test_daemon_run_args(),
        temp.path(),
        &mut runtime,
        None,
        true,
    )?;

    assert!(iteration.did_work);
    assert!(!iteration.failed);
    assert_eq!(*calls.borrow(), vec!["semantic_index"]);
    let daemon = daemon_report(temp.path(), &semantic_worker_report_for_daemon(temp.path()));
    assert_eq!(daemon["jobs"]["history_refresh"]["status"], "skipped");
    assert_eq!(
        daemon["jobs"]["history_refresh"]["reason"],
        "semantic_bootstrap_in_progress"
    );
    let history_status = read_daemon_job_status(&daemon_history_refresh_job_path(temp.path()))
        .expect("history refresh status");
    assert_eq!(history_status["scheduler_next_source_cursor"], 7);
    assert_eq!(history_status["consecutive_failures"], 1);
    assert!(history_status["retryable"].as_bool().unwrap_or(false));
    assert_eq!(
        daemon["jobs"]["semantic_index"]["last_run_status"],
        "budget_exhausted"
    );
    Ok(())
}
