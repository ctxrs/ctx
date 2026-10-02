use super::*;

#[test]
fn stable_import_request_id_conflicts_on_changed_reconciliation_demand() {
    let (_temp, data_root) = private_data_root();
    let coordinator = test_refresh_engine();
    let request_id = "019fcaaa-0000-7000-8000-000000000520";
    let request = |demand| {
        RefreshRequest::selected_import(request_id.to_owned(), RefreshSelection::All, demand)
    };
    let first = coordinator
        .submit(
            &data_root,
            request(SourceBackedReconciliationDemand::Incremental),
        )
        .unwrap();
    let replay = coordinator
        .submit(
            &data_root,
            request(SourceBackedReconciliationDemand::Incremental),
        )
        .unwrap();
    assert_eq!(replay.status(), first.status());
    let conflict = coordinator
        .submit(
            &data_root,
            request(SourceBackedReconciliationDemand::Exhaustive),
        )
        .unwrap();
    assert_eq!(conflict.status()["request_state"], "request_conflict");
    assert_eq!(conflict.status()["error_code"], "request_id_conflict");
    assert_eq!(
        first.status()["refresh_intent"]["reconciliation_demand"],
        "incremental"
    );
}

#[test]
fn restart_preserves_incremental_intent_and_strengthens_only_interrupted_work() {
    for running in [false, true] {
        let (_temp, data_root) = private_data_root();
        let journal = Arc::new(TestRefreshJournal::default());
        let first = CoreRefreshEngine::new(
            Arc::clone(&journal) as Arc<dyn RefreshJournal>,
            test_refresh_runtime(),
        );
        let request_id = "019fcaaa-0000-7000-8000-000000000521";
        let request = RefreshRequest::selected_import(
            request_id.to_owned(),
            RefreshSelection::All,
            SourceBackedReconciliationDemand::Incremental,
        );
        let original_intent = request.intent().clone();
        let admission = first.submit(&data_root, request.clone()).unwrap();
        release_pending_admission(&first, admission);
        let mut job = journal.load(&data_root).unwrap().unwrap();
        let fingerprint = job["request_fingerprint"].clone();
        if running {
            job["request_state"] = json!("running");
            journal.store(&data_root, &job).unwrap();
        }
        drop(first);
        let recovered =
            CoreRefreshEngine::new(journal as Arc<dyn RefreshJournal>, test_refresh_runtime());
        assert!(recovered
            .recover_interrupted_publication(&data_root)
            .unwrap());
        let status = status_value(&recovered, request_id);
        assert_eq!(status["refresh_intent"], original_intent.to_json());
        assert_eq!(
            status["reconciliation_demand"],
            if running { "exhaustive" } else { "incremental" }
        );
        assert_eq!(status["request_fingerprint"], fingerprint);
        let replay = recovered.submit(&data_root, request).unwrap();
        assert_ne!(replay.status()["request_state"], "request_conflict");
    }
}

#[test]
fn queued_import_recovery_defaults_from_intent_and_never_weakens_demand() {
    for requested in [
        SourceBackedReconciliationDemand::Incremental,
        SourceBackedReconciliationDemand::Exhaustive,
    ] {
        let attempt = new_refresh_attempt(
            None,
            SourceRefreshRuntimeMetadata::default(),
            RefreshIntent::SelectedImport {
                selection: RefreshSelection::All,
                reconciliation_demand: requested,
            },
            SourceBackedRefreshScope::All,
        );
        let mut job = attempt.job_json();
        job.as_object_mut().unwrap().remove("reconciliation_demand");
        assert_eq!(
            recover_queued_root(&job, None)
                .unwrap()
                .reconciliation_demand,
            requested
        );
        job["reconciliation_demand"] = json!("exhaustive");
        assert_eq!(
            recover_queued_root(&job, None)
                .unwrap()
                .reconciliation_demand,
            SourceBackedReconciliationDemand::Exhaustive
        );
        job["reconciliation_demand"] = json!("incremental");
        assert_eq!(
            recover_queued_root(&job, None).is_ok(),
            requested == SourceBackedReconciliationDemand::Incremental
        );
    }
}

#[test]
fn incremental_selected_membership_ignores_stale_watcher_members_without_widening() {
    use ctx_history_capture::provider_source_for_path;
    use ctx_history_index::{
        CompiledSearchFilter, EventSearchFilters, LexicalExecution, LexicalMode,
    };

    let contains = |index: &ctx_history_index::VerifiedIndex, marker: &str| {
        let terms = [marker];
        !index
            .execute_lexical(LexicalExecution::new(
                LexicalMode::Search(&terms),
                &CompiledSearchFilter::compile(EventSearchFilters::default()).unwrap(),
                10,
            ))
            .unwrap()
            .batch
            .candidates
            .is_empty()
    };
    for exact in [false, true] {
        let (temp, data_root) = private_data_root();
        let runtime = scoped_runtime(temp.path());
        let sessions = temp.path().join("home/.codex/sessions");
        let peer = temp.path().join("unselected-projects/project");
        fs::create_dir_all(&sessions).unwrap();
        fs::create_dir_all(&peer).unwrap();
        write_codex_session_fixture(&sessions);
        let template = fs::read_to_string(sessions.join("session.jsonl")).unwrap();
        // Authored membership cases reuse the established native record shape.
        let write_member = |root: &Path, name: &str, marker: &str| {
            fs::write(
                root.join(format!("{name}.jsonl")),
                template
                    .replace("\"id\":\"session\"", &format!("\"id\":\"{name}\""))
                    .replace("refresh admission fixture", marker),
            )
            .unwrap();
        };
        // Another provider avoids the existing same-format exact replacement policy.
        let write_peer_member = |name: &str, marker: &str| {
            let record = json!({
                "type": "user",
                "uuid": format!("fixture-{name}"),
                "sessionId": name,
                "message": {"role": "user", "content": marker},
            });
            fs::write(peer.join(format!("{name}.jsonl")), format!("{record}\n")).unwrap();
        };
        write_member(&sessions, "deleted", "deletedmemberoracle");
        write_peer_member("peer", "retainedpeeroracle");
        let peer_authority = crate::upsert_explicit_source(
            &data_root,
            &provider_source_for_path(CaptureProvider::Claude, peer.clone()),
        )
        .unwrap()
        .authority;
        let selection = if exact {
            RefreshSelection::ExactSource(
                crate::upsert_explicit_source(
                    &data_root,
                    &provider_source_for_path(CaptureProvider::Codex, sessions.clone()),
                )
                .unwrap()
                .authority,
            )
        } else {
            RefreshSelection::Provider(CaptureProvider::Codex)
        };
        let executions = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&executions);
        let coordinator = CoreRefreshEngine::with_runtime_for_test(
            Arc::new(TestRefreshJournal::default()),
            runtime,
            Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
                observed.lock().unwrap().push((
                    execution.reconciliation_demand,
                    execution.admitted_refresh().exact_routes().clone(),
                    execution.admitted_refresh().route_worksets().clone(),
                ));
                ctx_history_refresh_execution::execute_refresh(execution)
            }),
            Arc::new(|_, _, _, _| panic!("selected request invoked global discovery")),
        );
        let import = |selection, demand| {
            let admission = coordinator
                .submit(
                    &data_root,
                    RefreshRequest::selected_import(Uuid::now_v7().to_string(), selection, demand),
                )
                .unwrap();
            release_pending_admission(&coordinator, admission);
            assert!(coordinator
                .prepare_next_pending_admission(&data_root)
                .unwrap());
            let run = coordinator.run_next(&data_root).unwrap();
            assert!(!run.failed, "exact={exact}: {}", run.job);
            run
        };
        import(
            RefreshSelection::ExactSource(peer_authority),
            SourceBackedReconciliationDemand::Exhaustive,
        );
        {
            let index = crate::open_verified_index(&source_backed_index_root(&data_root)).unwrap();
            assert!(
                contains(&index, "retainedpeeroracle"),
                "exact={exact}: peer baseline must contain the preservation oracle"
            );
        }
        let baseline = import(
            selection.clone(),
            SourceBackedReconciliationDemand::Exhaustive,
        );
        {
            let index = crate::open_verified_index(&source_backed_index_root(&data_root)).unwrap();
            assert!(
                contains(&index, "retainedpeeroracle"),
                "exact={exact}: selected baseline must retain the peer"
            );
        }
        let SourceBackedRefreshScope::Exact(routes) =
            refresh_scope_from_json(Some(&baseline.job["refresh_scope"])).unwrap()
        else {
            panic!("selected exact physical scope")
        };
        coordinator.initialize_watch_route_authority(routes.iter().cloned());
        coordinator.record_watch_routes_with_members(
            routes
                .iter()
                .cloned()
                .map(|route| (route, EventWatermark::new(8, 1))),
            routes
                .iter()
                .cloned()
                .map(|route| (route, BTreeSet::from([sessions.join("session.jsonl")])))
                .collect(),
            source_route_ledger_now_ms().saturating_sub(1_000),
        );
        fs::remove_file(sessions.join("deleted.jsonl")).unwrap();
        write_member(&sessions, "added", "addedmemberoracle");
        write_peer_member("laterpeer", "unselectedlateroracle");
        let refreshed = import(selection, SourceBackedReconciliationDemand::Incremental);
        assert_eq!(refreshed.job["reconciliation_demand"], "incremental");
        let executions = executions.lock().unwrap();
        let (demand, selected_routes, worksets) = executions.last().unwrap();
        assert_eq!(*demand, SourceBackedReconciliationDemand::Incremental);
        assert_eq!(selected_routes, &routes);
        assert!(
            worksets.is_empty(),
            "selected import must discover complete route membership"
        );
        let index = crate::open_verified_index(&source_backed_index_root(&data_root)).unwrap();
        assert!(contains(&index, "addedmemberoracle"));
        assert!(!contains(&index, "deletedmemberoracle"));
        assert!(
            contains(&index, "retainedpeeroracle"),
            "exact={exact}: incremental import must retain the peer"
        );
        assert!(!contains(&index, "unselectedlateroracle"));
        assert_eq!(index.document_count(), 3);
    }
}
