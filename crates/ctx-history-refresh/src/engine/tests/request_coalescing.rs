//! Request-coalescing coverage owned by the refresh engine.

use super::*;

fn command_refresh_submission(
    trigger: RefreshRequestTrigger,
    fresh_after_admitted_snapshot: bool,
) -> RefreshRequest {
    let request_id = Uuid::now_v7().to_string();
    if fresh_after_admitted_snapshot {
        RefreshRequest::new(
            request_id,
            RefreshIntent::SelectedImport(RefreshSelection::All),
            trigger,
        )
    } else {
        RefreshRequest::automatic(request_id, trigger)
    }
}

fn enqueue_ordinary_attempt(
    coordinator: &CoreRefreshEngine,
    data_root: &Path,
    trigger: &str,
) -> Value {
    match trigger {
        "periodic" => coordinator.enqueue_periodic(data_root).unwrap(),
        "search" => coordinator.enqueue_for_test(None),
        _ => panic!("unknown ordinary test trigger"),
    }
}

#[test]
fn setup_refresh_claims_periodic_and_search_attempts_without_ordinary_overwrite() {
    for ordinary in ["periodic", "search"] {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
        let coordinator = CoreRefreshEngine::new();
        let initial = enqueue_ordinary_attempt(&coordinator, &data_root, ordinary);
        let physical_request_id = request_id(&initial);

        let claimed = coordinator
            .submit(
                &data_root,
                command_refresh_submission(RefreshRequestTrigger::Setup, false),
            )
            .unwrap();
        assert_eq!(claimed.status()["request_id"], physical_request_id);
        let claimed = status_value(&coordinator, &physical_request_id);
        assert_eq!(claimed["operation"], "refresh");
        assert_eq!(claimed["trigger"], "setup");
        assert_eq!(claimed["trigger_provenance"], "setup_command");

        coordinator.enqueue_periodic(&data_root).unwrap();
        coordinator
            .submit(
                &data_root,
                command_refresh_submission(RefreshRequestTrigger::Search, false),
            )
            .unwrap();
        let retained = status_value(&coordinator, &physical_request_id);
        assert_eq!(retained["trigger"], "setup");
        assert_eq!(retained["trigger_provenance"], "setup_command");
    }
}

#[test]
fn explicit_import_operation_upgrades_automatic_import_without_losing_ownership() {
    let mut attempt = new_refresh_attempt(
        None,
        SourceRefreshRuntimeMetadata::periodic(),
        RefreshIntent::AutomaticMaintenance,
        SourceBackedRefreshScope::All,
    );
    let setup = SourceRefreshRuntimeMetadata {
        operation: RefreshOperation::Refresh,
        daemon_mode: "full".to_owned(),
        trigger: "setup",
        trigger_provenance: "setup_command",
    };
    let automatic_import = SourceRefreshRuntimeMetadata {
        operation: RefreshOperation::Refresh,
        daemon_mode: "full".to_owned(),
        trigger: "import",
        trigger_provenance: "import_command",
    };
    let explicit_import = SourceRefreshRuntimeMetadata {
        operation: RefreshOperation::Import,
        daemon_mode: "full".to_owned(),
        trigger: "import",
        trigger_provenance: "explicit_source_catalog",
    };

    coalesce_attempt(&mut attempt, setup.clone());
    coalesce_attempt(&mut attempt, automatic_import.clone());
    coalesce_attempt(&mut attempt, setup);
    assert_eq!(attempt.operation(), RefreshOperation::Refresh);
    assert_eq!(attempt.trigger, "import");
    assert_eq!(attempt.trigger_provenance, "import_command");

    coalesce_attempt(&mut attempt, explicit_import);
    coalesce_attempt(&mut attempt, automatic_import);
    assert_eq!(attempt.operation(), RefreshOperation::Refresh);
    assert_eq!(attempt.trigger, "import");
    assert_eq!(attempt.trigger_provenance, "explicit_source_catalog");
}

#[test]
fn duplicate_concurrent_requests_launch_one_writer() {
    const REQUESTS: usize = 16;

    let coordinator = Arc::new(CoreRefreshEngine::new());
    let barrier = Arc::new(Barrier::new(REQUESTS));
    let mut threads = Vec::new();
    for _ in 0..REQUESTS {
        let coordinator = coordinator.clone();
        let barrier = barrier.clone();
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            coordinator.enqueue(Some("generation-1".to_owned()))
        }));
    }
    let responses = threads
        .into_iter()
        .map(|thread| thread.join().expect("request thread"))
        .collect::<Vec<_>>();
    let expected_request_id = request_id(&responses[0]);
    assert!(responses
        .iter()
        .all(|response| request_id(response) == expected_request_id));

    let writer_launches = AtomicUsize::new(0);
    let run = coordinator
        .run_next_with(
            |request_id, coordinator| {
                writer_launches.fetch_add(1, Ordering::SeqCst);
                let _ = coordinator.set_progress(
                    request_id,
                    SourceBackedRefreshProgressUpdate {
                        phase: "refreshing".to_owned(),
                        completed_sources: 0,
                        total_sources: 1,
                        total_sources_known: true,
                        current_source: Some("source-a".to_owned()),
                        completed_records: Some(1),
                        completed_bytes: Some(128),
                        current_source_progress: Some(SourceBackedCurrentSourceProgress {
                            stage: SourceBackedCurrentSourceProgressStage::LogicalScan,
                            snapshot_pages_completed: None,
                            snapshot_pages_total: None,
                            snapshot_bytes_completed: None,
                            snapshot_bytes_total: None,
                            logical_rows_scanned: Some(1),
                            logical_certified_bytes: Some(128),
                        }),
                        ..Default::default()
                    },
                );
                Ok(test_publication("generation-2"))
            },
            || Ok(Some("generation-2".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("queued refresh");

    assert_eq!(writer_launches.load(Ordering::SeqCst), 1);
    assert!(run.did_work);
    assert!(!run.failed);
    let status = coordinator
        .status(&expected_request_id)
        .expect("published request status");
    assert_eq!(status["request_state"], "published");
    assert!(status["progress"].get("current_source").is_none());
    assert!(status["progress"].get("completed_records").is_none());
    assert!(status["progress"].get("completed_bytes").is_none());
    assert!(status["progress"].get("current_source_progress").is_none());
    assert_eq!(status["published_generation"], "generation-2");
    assert_eq!(status["generation_changed"], true);
    assert_eq!(status["receipt"]["previous_generation"], "generation-1");
    assert_eq!(status["receipt"]["published_generation"], "generation-2");
    assert_eq!(status["receipt"]["generation_changed"], true);
    assert!(status.get("published_explicit_source_catalog").is_none());
    assert!(status["receipt"]
        .get("published_explicit_source_catalog")
        .is_none());
    assert_eq!(status["receipt"]["current"]["current_source_count"], 1);
    assert_eq!(status["receipt"]["current"]["current_indexed_documents"], 2);
    assert_eq!(status["receipt"]["current"]["current_rejected_records"], 1);
    assert_eq!(
        status["coalesced_requests"].as_u64(),
        Some((REQUESTS - 1) as u64)
    );
    assert_eq!(status["certified_source_count"], 1);
    assert_eq!(status["certified_source_bytes"], 128);
    assert_eq!(status["timings_us"]["discovery"], 11);
    assert_eq!(status["timings_us"]["scan_stage"], 22);
    assert_eq!(status["timings_us"]["commit"], 33);
    assert!(coordinator
        .run_next_with(
            |_, _| panic!("duplicate writer launched"),
            || Ok(Some("generation-2".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .is_none());
}

#[test]
fn unchanged_nonempty_publication_is_no_op_by_generation_identity() {
    let coordinator = CoreRefreshEngine::new();
    let request = coordinator.enqueue(Some("generation-1".to_owned()));
    let request_id = request_id(&request);
    let run = coordinator
        .run_next_with(
            |_, _| Ok(test_publication("generation-1")),
            || Ok(Some("generation-1".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("queued refresh");

    assert!(!run.failed);
    assert!(!run.did_work);
    let status = coordinator.status(&request_id).expect("published request");
    assert_eq!(status["generation_changed"], false);
    assert_eq!(status["receipt"]["generation_changed"], false);
    assert_eq!(status["receipt"]["current"]["current_source_count"], 1);
    assert_eq!(status["receipt"]["current"]["current_indexed_documents"], 2);
    assert_eq!(
        status["structured_outcome"]["retained_generation"],
        "generation-1"
    );
    assert_eq!(
        status["structured_outcome"]["published_generation"],
        "generation-1"
    );
}

#[test]
fn concurrent_refresh_request_uses_active_generation_without_reopening_inflight_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let coordinator = CoreRefreshEngine::new();
    let request = coordinator.enqueue(None);
    assert_eq!(request["request_state"], "queued");

    let index_root = source_backed_index_root(temp.path());
    let inactive = index_root.join("index-generations/in-flight");
    std::fs::create_dir_all(&inactive).unwrap();
    std::fs::write(inactive.join("meta.json"), b"in-flight metadata").unwrap();

    let coalesced = coordinator
        .handle_ipc_request(
            temp.path(),
            &json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "refresh_intent": {"kind": "automatic_maintenance"},
            }),
        )
        .unwrap()
        .expect("coalesced refresh response");
    assert_eq!(coalesced["request_id"], request["request_id"]);
    assert_eq!(coalesced["coalesced_requests"], 1);
}

#[test]
fn selected_import_requests_remain_distinct_before_execution() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let coordinator = CoreRefreshEngine::new();
    let authority = test_exact_catalog_authority(&data_root, &temp.path().join("exact-source"));

    let submit = |request_id: String| {
        coordinator
            .handle_ipc_request(
                &data_root,
                &json!({
                    "op": SOURCE_REFRESH_REQUEST_OP,
                    "request_id": request_id,
                    "mode": "wait",
                    "refresh_intent": {
                        "kind": "selected_import",
                        "selection": {
                            "kind": "exact_source",
                            "authority": authority.to_json(),
                        },
                    },
                }),
            )
            .unwrap()
            .expect("selected import response")
    };

    let first = submit(Uuid::now_v7().to_string());
    let second = submit(Uuid::now_v7().to_string());

    assert_ne!(request_id(&first), request_id(&second));
    assert_eq!(first["refresh_intent"]["selection"]["kind"], "exact_source");
    assert_eq!(
        second["refresh_intent"]["selection"]["kind"],
        "exact_source"
    );
    assert!(coordinator.has_pending_request());
}
