//! Durable request-journal recovery coverage owned by the refresh engine.

use super::*;

fn enqueue_synthetic_manual_all_request(
    coordinator: &super::super::CoreRefreshEngine,
    data_root: &Path,
) -> Value {
    coordinator
        .enqueue_manual_all_demand_for_test(data_root, None, Uuid::now_v7().to_string())
        .expect("synthetic manual-all request")
}

fn queued_successor_ids(job: &Value) -> BTreeSet<String> {
    job.get("queued_successors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|successor| successor["request_id"].as_str().map(str::to_owned))
        .collect()
}

fn publish_synthetic_terminal(
    coordinator: &super::super::CoreRefreshEngine,
    data_root: &Path,
    generation: &str,
) -> SourceBackedRefreshRun {
    coordinator
        .run_next_with(
            |_, _| Ok(test_publication(generation)),
            || Ok(Some(generation.to_owned())),
            |job| write_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root), job),
            |_| Ok(()),
        )
        .expect("synthetic terminal publication")
}

#[test]
fn published_terminal_without_generation_recovers_from_the_journal() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let first = test_refresh_engine();
    let request = first.enqueue(None);
    let request_id = request_id(&request);
    let published = publish_synthetic_terminal(&first, &data_root, "journal-generation");
    assert_eq!(published.job["request_state"], "published");
    drop(first);

    let restarted = test_refresh_engine();
    assert!(!restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    assert!(!restarted.has_pending_request());
    assert!(restarted.pinned_core_publication().is_none());
    let recovered = restarted.status(&request_id).unwrap();
    assert_eq!(recovered["request_id"], request_id);
    assert_eq!(recovered["request_state"], "published");
    assert_eq!(recovered["published_generation"], "journal-generation");
    assert_eq!(recovered["receipt"], published.job["receipt"]);
}

#[test]
fn published_terminal_journal_fields_must_match_its_receipt() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let first = test_refresh_engine();
    first.enqueue(None);
    let published = publish_synthetic_terminal(&first, &data_root, "journal-generation");
    let mut malformed = published.job;
    malformed["outcome"] = json!("completed_with_rejections");
    write_daemon_job_status(
        &daemon_source_backed_refresh_job_path(&data_root),
        &malformed,
    )
    .unwrap();
    drop(first);

    let error = test_refresh_engine()
        .recover_interrupted_publication(&data_root)
        .expect_err("mismatched terminal fields must fail closed");
    assert!(format!("{error:#}").contains("does not match its terminal receipt"));
}

#[test]
fn published_terminal_recovery_preserves_its_exact_queued_successor() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let first = test_refresh_engine();
    let root = first.enqueue(None);
    let root_id = request_id(&root);
    let successor = enqueue_synthetic_manual_all_request(&first, &data_root);
    let successor_id = request_id(&successor);
    let terminal = publish_synthetic_terminal(&first, &data_root, "journal-generation");
    assert_eq!(
        queued_successor_ids(&terminal.job),
        BTreeSet::from([successor_id.clone()])
    );
    drop(first);

    let restarted = test_refresh_engine();
    assert!(restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    assert_eq!(
        restarted.status(&root_id).unwrap()["request_state"],
        "published"
    );
    let recovered_successor = restarted.status(&successor_id).unwrap();
    assert_eq!(recovered_successor["request_id"], successor_id);
    assert_eq!(recovered_successor["request_state"], "admission_pending");
}

#[test]
fn advanced_pointer_does_not_substitute_for_interrupted_same_id_replay() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let first = test_refresh_engine();
    let root = enqueue_synthetic_manual_all_request(&first, &data_root);
    let root_id = request_id(&root);
    let successor = enqueue_synthetic_manual_all_request(&first, &data_root);
    let successor_id = request_id(&successor);
    let status_path = daemon_source_backed_refresh_job_path(&data_root);
    let mut running = read_daemon_job_status(&status_path).unwrap();
    running["request_state"] = json!("running");
    running["status"] = json!("running");
    write_daemon_job_status(&status_path, &running).unwrap();
    let active_generation = publish_pin_source(
        &source_backed_index_root(&data_root),
        publication_pin_source(),
    );
    drop(first);

    let restarted = test_refresh_engine();
    assert!(restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    let replay = read_daemon_job_status(&status_path).unwrap();
    assert_eq!(replay["request_id"], root_id);
    assert_eq!(replay["request_state"], "admission_pending");
    assert_eq!(replay["previous_generation"], active_generation);
    assert_eq!(replay["reconciliation_demand"], "exhaustive");
    assert_eq!(
        queued_successor_ids(&replay),
        BTreeSet::from([successor_id])
    );
}

#[test]
fn lone_failed_terminal_recovers_without_reenqueue() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let first = test_refresh_engine();
    let request = first.enqueue(None);
    let request_id = request_id(&request);
    let failed = first
        .run_next_with(
            |_, _| Err(anyhow!("exact lone terminal failure")),
            || Ok(None),
            |job| write_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root), job),
            |_| Ok(()),
        )
        .expect("failed terminal");
    assert!(failed.failed);
    assert!(!failed.terminal_persistence_pending);
    let status_path = daemon_source_backed_refresh_job_path(&data_root);
    let mut legacy_failure = read_daemon_job_status(&status_path).unwrap();
    legacy_failure
        .as_object_mut()
        .unwrap()
        .remove("structured_outcome");
    legacy_failure["failure_type"] = json!("source_failures");
    write_daemon_job_status(&status_path, &legacy_failure).unwrap();
    drop(first);

    let restarted = test_refresh_engine();
    assert!(!restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    assert!(!restarted.has_pending_request());
    let recovered = restarted.status(&request_id).unwrap();
    assert_eq!(recovered["request_id"], request_id);
    assert_eq!(recovered["request_state"], "failed");
    assert_eq!(recovered["last_error"], "exact lone terminal failure");
    assert_eq!(
        recovered["structured_outcome"]["detail"],
        "exact lone terminal failure"
    );
    let kind = recovered.kind().unwrap();
    let outcome = kind.terminal_outcome().unwrap();
    assert_eq!(outcome.code(), RefreshOutcomeCode::SourceFailures);
    assert_eq!(outcome.detail(), Some("exact lone terminal failure"));
}

#[test]
fn failed_terminal_persistence_retry_keeps_work_bounded_and_journals_successor() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let first = test_refresh_engine();
    let active = first.enqueue(None);
    let active_id = request_id(&active);
    let failed = first
        .run_next_with(
            |_, _| Err(anyhow!("injected provider failure")),
            || Ok(None),
            |_| Err(anyhow!("injected terminal status write failure")),
            |_| Ok(()),
        )
        .unwrap();
    assert!(failed.failed);
    assert!(failed.terminal_persistence_pending);

    let successor = enqueue_synthetic_manual_all_request(&first, &data_root);
    let successor_id = request_id(&successor);
    let pending = read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root))
        .expect("failed root with durable successor");
    assert_eq!(pending["request_id"], active_id);
    assert_eq!(
        queued_successor_ids(&pending),
        BTreeSet::from([successor_id.clone()])
    );

    let retried = first
        .run_next_with(
            |_, _| panic!("terminal retry must not recapture"),
            || panic!("terminal retry must not reopen Core"),
            |job| write_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root), job),
            |_| panic!("terminal retry must not rerun failure handling"),
        )
        .unwrap();
    assert!(retried.failed);
    assert!(!retried.terminal_persistence_pending);
    drop(first);

    let restarted = test_refresh_engine();
    assert!(restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    assert_eq!(
        restarted.status(&successor_id).unwrap()["request_state"],
        "admission_pending"
    );
}
