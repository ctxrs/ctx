use super::*;

fn publication_pin_test_publication(
    generation_id: impl Into<String>,
) -> SourceBackedRefreshPublication {
    let mut publication = test_publication(generation_id);
    publication.current = SourceBackedRefreshCurrent {
        source_count: 1,
        indexed_documents: 1,
        complete_records: 1,
        retained_records: 1,
        certified_source_bytes: 128,
        ..SourceBackedRefreshCurrent::default()
    };
    publication
}

#[test]
fn exact_no_op_status_reuses_the_exact_durable_receipt_across_restart() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let metadata_factories = Arc::new(AtomicUsize::new(0));
    let first_factories = Arc::clone(&metadata_factories);
    let first = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let request_id = execution.request_id.to_owned();
            let operation = execution.operation;
            let scope = execution.scope.clone();
            let factories = Arc::clone(&first_factories);
            let source = publication_pin_source();
            let mut writer = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?;
            writer.begin_source(source.clone())?;
            writer.add_core_record(publication_pin_record(&source))?;
            writer.certify_source(publication_pin_certificate(&source))?;
            let published = writer.commit_with_publication_metadata(
                |_| true,
                move |context| {
                    factories.fetch_add(1, Ordering::SeqCst);
                    let publication =
                        publication_pin_test_publication(context.generation_id().to_owned());
                    let receipt = SourceBackedRefreshReceipt::from_verified_publication(
                        None,
                        context.generation_id().to_owned(),
                        &publication,
                    )
                    .map_err(|error| IndexError::PublicationMetadata(format!("{error:#}")))?;
                    SourceBackedPublicationMetadata {
                        request_id: request_id.clone(),
                        operation,
                        refresh_scope: scope.clone(),
                        receipt: receipt.to_json(),
                        route_observations: BTreeMap::new(),
                    }
                    .encode()
                },
            )?;
            Ok(publication_pin_test_publication(
                published.receipt().generation_id.clone(),
            ))
        },
    ));
    first.enqueue_periodic(&data_root).unwrap();
    let initial = first.run_next(&data_root).expect("initial publication");
    assert!(!initial.failed, "{:#}", initial.job);
    assert_eq!(metadata_factories.load(Ordering::SeqCst), 1);
    assert!(initial.job.get("request_outcome").is_none());
    let durable_receipt = initial.job["receipt"].clone();
    let generation = initial.job["published_generation"]
        .as_str()
        .unwrap()
        .to_owned();
    drop(first);

    let second_factories = Arc::clone(&metadata_factories);
    let second = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let source = publication_pin_source();
            let mut writer = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?;
            writer.begin_source(source.clone())?;
            writer.add_core_record(publication_pin_record(&source))?;
            writer.certify_source(publication_pin_certificate(&source))?;
            let published = writer.commit_with_publication_metadata(
                |_| true,
                |_| {
                    second_factories.fetch_add(1, Ordering::SeqCst);
                    Err(IndexError::PublicationMetadata(
                        "no-op replay must not rebuild publication metadata".to_owned(),
                    ))
                },
            )?;
            Ok(publication_pin_test_publication(
                published.receipt().generation_id.clone(),
            ))
        },
    ));
    second.enqueue_periodic(&data_root).unwrap();
    let replay = second.run_next(&data_root).expect("exact no-op replay");
    assert!(!replay.failed, "{:#}", replay.job);
    assert!(!replay.did_work);
    assert_eq!(replay.job["published_generation"], generation);
    assert_eq!(replay.job["previous_generation"], generation);
    assert_eq!(replay.job["generation_changed"], false);
    assert_eq!(replay.job["receipt"], durable_receipt);
    assert_eq!(
        replay.job["request_outcome"]["previous_generation"],
        generation
    );
    assert_eq!(replay.job["request_outcome"]["generation_changed"], false);
    assert_eq!(metadata_factories.load(Ordering::SeqCst), 1);
    let pin = second.pinned_core_publication().expect("no-op Core pin");
    assert_eq!(pin.receipt().to_json(), durable_receipt);
    drop(second);

    let restarted = CoreRefreshEngine::new();
    assert!(!restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    assert_eq!(
        restarted
            .pinned_core_publication()
            .expect("restarted no-op Core pin")
            .receipt()
            .to_json(),
        durable_receipt
    );
}

#[test]
fn pointer_crash_recovers_active_receipt_and_preserves_fresh_successor() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let authority = load_explicit_source_catalog_authority(&data_root).unwrap();
    let first = CoreRefreshEngine::new();
    let active = first.enqueue_periodic(&data_root).unwrap();
    let active_request_id = request_id(&active);
    let successor_request_id = Arc::new(Mutex::new(None::<String>));
    let recorded_successor = Arc::clone(&successor_request_id);
    let execution_root = data_root.clone();
    let execution_authority = authority.clone();

    let crash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = first.run_next_with(
            |active_id, coordinator| {
                let successor = coordinator
                    .handle_ipc_request(
                        &execution_root,
                        &json!({
                            "op": SOURCE_REFRESH_REQUEST_OP,
                            "mode": "wait",
                            "operation": "import",
                            "explicit_source_catalog": execution_authority.to_json(),
                            "fresh_after_admitted_snapshot": true,
                        }),
                    )?
                    .expect("fresh manual successor");
                *recorded_successor.lock().unwrap() = Some(request_id(&successor));

                let metadata_request_id = active_id.to_owned();
                let published = ctx_history_index::GenerationWriter::open(
                    source_backed_index_root(&execution_root),
                    WriterOptions::default(),
                )?
                .commit_with_publication_metadata(
                    |_| true,
                    |context| {
                        let publication = empty_test_publication(context.generation_id());
                        let receipt = SourceBackedRefreshReceipt::from_verified_publication(
                            None,
                            context.generation_id().to_owned(),
                            &publication,
                        )
                        .map_err(|error| IndexError::PublicationMetadata(format!("{error:#}")))?;
                        SourceBackedPublicationMetadata {
                            request_id: metadata_request_id.clone(),
                            operation: SourceBackedRefreshOperation::Refresh,
                            refresh_scope: SourceBackedRefreshScope::All,
                            receipt: receipt.to_json(),
                            route_observations: BTreeMap::new(),
                        }
                        .encode()
                    },
                )?;
                panic!(
                    "injected crash after pointer publication {}",
                    published.receipt().generation_id
                );
            },
            || panic!("crash must precede publication probe"),
            |_| panic!("crash must precede terminal persistence"),
            |_| panic!("crash must precede failure persistence"),
        );
    }));
    assert!(crash.is_err());
    let successor_request_id = successor_request_id
        .lock()
        .unwrap()
        .clone()
        .expect("recorded successor request");
    let interrupted = read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root))
        .expect("interrupted durable queue");
    assert_eq!(interrupted["request_id"], active_request_id);
    assert_eq!(
        interrupted["queued_successors"][0]["request_id"],
        successor_request_id
    );
    let committed = pin_published_generation(&data_root)
        .unwrap()
        .expect("committed Core generation")
        .generation_id()
        .to_owned();
    drop(first);

    let executions = Arc::new(AtomicUsize::new(0));
    let observed_executions = Arc::clone(&executions);
    let restarted = CoreRefreshEngine::with_executor(Arc::new(
        move |_execution: SourceBackedRefreshExecution<'_>| {
            observed_executions.fetch_add(1, Ordering::SeqCst);
            Err(anyhow!("recovery must not recapture committed Core work"))
        },
    ));
    assert!(restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert_eq!(
        restarted.status(&active_request_id).unwrap()["request_state"],
        "published"
    );
    assert_eq!(
        restarted.status(&successor_request_id).unwrap()["request_state"],
        "queued"
    );
    assert_eq!(
        restarted
            .pinned_core_publication()
            .expect("recovered exact publication pin")
            .receipt()
            .published_generation,
        committed
    );
    assert!(restarted.has_pending_request());
    let recovered = read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root))
        .expect("predecessor-rooted recovered job");
    assert_eq!(recovered["request_id"], active_request_id);
    assert_eq!(recovered["request_state"], "published");
    assert_eq!(
        recovered["queued_successors"][0]["request_id"],
        successor_request_id
    );
}

#[test]
fn pointer_crash_recovers_exact_manual_all_continuation_receipt_without_recapture() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let covered_routes = BTreeSet::from([route_identity(0xa1), route_identity(0xa2)]);
    let routes = covered_routes
        .iter()
        .cloned()
        .chain([route_identity(0xa3)])
        .collect::<BTreeSet<_>>();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let calls = Arc::new(AtomicUsize::new(0));
    let expected_receipt = Arc::new(Mutex::new(None::<Value>));
    let executor_routes = routes.clone();
    let executor_entered = Arc::clone(&entered);
    let executor_release = Arc::clone(&release);
    let executor_calls = Arc::clone(&calls);
    let recorded_receipt = Arc::clone(&expected_receipt);
    let coordinator = Arc::new(CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let selected = physically_selected_routes(&execution, &executor_routes);
            if executor_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                executor_entered.wait();
                executor_release.wait();
                let rejected_route = selected.iter().next().expect("selected exact route");
                let mut publication =
                    publish_selected_routes_with_rejection(&execution, &selected, rejected_route)?;
                publication.current.removed_source_count = 1;
                return Ok(publication);
            }

            assert_eq!(execution.scope, SourceBackedRefreshScope::All);
            assert_eq!(execution.covered_route_ids.len(), 2);
            assert_eq!(selected.len(), 1);
            let previous_generation = open_verified_index(execution.index_root)
                .ok()
                .map(|index| index.generation_id().to_owned());
            let request_id = execution.request_id.to_owned();
            let operation = execution.operation;
            let scope = execution.scope.clone();
            let covered_publication = execution.covered_publication.clone();
            let selected_route_results = selected
                .iter()
                .map(|route| {
                    SourceBackedRefreshRouteResult::succeeded(route.as_str().to_owned(), true)
                })
                .collect::<Vec<_>>();
            let expected = Arc::clone(&recorded_receipt);
            let source = publication_pin_source_with_anchor(0x93);
            let changed_source = publication_pin_source_with_anchor(0x96);
            let mut writer = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?;
            writer.begin_source(source.clone())?;
            writer.add_core_record(publication_pin_record(&source))?;
            writer.certify_source(publication_rejection_certificate(&source))?;
            writer.begin_source(changed_source.clone())?;
            writer.add_core_record(publication_pin_record(&changed_source))?;
            writer.certify_source(publication_pin_certificate(&changed_source))?;
            let published = writer.commit_with_publication_metadata(
                |_| true,
                move |context| {
                    let mut publication =
                        empty_test_publication(context.generation_id().to_owned());
                    publication.current =
                        SourceBackedRefreshCurrent::from_sources(&context.manifest().sources, 0)
                            .map_err(|error| {
                                IndexError::PublicationMetadata(format!("{error:#}"))
                            })?;
                    publication.certified_source_count = publication.current.source_count;
                    publication.certified_source_bytes = publication.current.certified_source_bytes;
                    publication.route_results = selected_route_results.clone();
                    covered_publication.apply_receipt(&mut publication);
                    let receipt = SourceBackedRefreshReceipt::from_verified_publication(
                        previous_generation.clone(),
                        context.generation_id().to_owned(),
                        &publication,
                    )
                    .map_err(|error| IndexError::PublicationMetadata(format!("{error:#}")))?;
                    *expected.lock().unwrap() = Some(receipt.to_json());
                    SourceBackedPublicationMetadata {
                        request_id: request_id.clone(),
                        operation,
                        refresh_scope: scope.clone(),
                        receipt: receipt.to_json(),
                        route_observations: BTreeMap::new(),
                    }
                    .encode()
                },
            )?;
            panic!(
                "injected crash after continuation pointer publication {}",
                published.receipt().generation_id
            );
        },
    )));
    coordinator.reconcile_watch_routes(
        covered_routes,
        EventWatermark::new(2, 0),
        ledger_now_ms().saturating_sub(1_000),
    );
    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());
    let authority = load_explicit_source_catalog_authority(&data_root).unwrap();
    let manual = std::thread::scope(|scope| {
        let runner = Arc::clone(&coordinator);
        let runner_root = data_root.clone();
        scope.spawn(move || {
            let run = runner
                .run_next(&runner_root)
                .expect("running exact predecessor");
            assert!(!run.failed, "{:#}", run.job);
        });
        entered.wait();
        coordinator.initialize_watch_route_authority(routes.iter().cloned());
        let manual = manual_all_request(&coordinator, &data_root, &authority);
        release.wait();
        manual
    });
    let manual_request_id = request_id(&manual);

    let crash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        coordinator.run_next(&data_root)
    }));
    match crash {
        Err(_) => {}
        Ok(run) => panic!(
            "continuation publication returned instead of crashing: {:#?}",
            run.as_ref().map(|run| &run.job)
        ),
    }
    let expected_receipt = expected_receipt
        .lock()
        .unwrap()
        .clone()
        .expect("exact no-crash continuation receipt");
    assert_eq!(expected_receipt["selected_route_total"], routes.len());
    assert_eq!(expected_receipt["rejected_record_total"], 1);
    assert_eq!(expected_receipt["outcome"], "completed_with_rejections");
    drop(coordinator);

    let recaptures = Arc::new(AtomicUsize::new(0));
    let observed_recaptures = Arc::clone(&recaptures);
    let restarted = CoreRefreshEngine::with_executor(Arc::new(
        move |_execution: SourceBackedRefreshExecution<'_>| {
            observed_recaptures.fetch_add(1, Ordering::SeqCst);
            Err(anyhow!(
                "recovery must not recapture committed continuation work"
            ))
        },
    ));
    assert!(!restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    assert_eq!(recaptures.load(Ordering::SeqCst), 0);
    let recovered = restarted
        .status(&manual_request_id)
        .expect("exact acknowledged continuation request");
    assert_eq!(recovered["request_id"], manual_request_id);
    assert_eq!(recovered["request_state"], "published");
    assert_eq!(recovered["receipt"], expected_receipt);
    assert_eq!(recovered["outcome"], "completed_with_rejections");
    assert_eq!(
        recovered["receipt"]["route_results"]
            .as_object()
            .unwrap()
            .values()
            .filter_map(Value::as_array)
            .map(|result| result.get(4).and_then(Value::as_u64).unwrap_or(0))
            .sum::<u64>(),
        1
    );
}

#[test]
fn failed_terminal_restart_preserves_fresh_successor() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let authority = load_explicit_source_catalog_authority(&data_root).unwrap();
    let first = CoreRefreshEngine::new();
    first.enqueue_periodic(&data_root).unwrap();
    let successor_id = Arc::new(Mutex::new(None::<String>));
    let recorded_successor = Arc::clone(&successor_id);
    let run = first
        .run_next_with(
            |_, coordinator| {
                let successor = coordinator
                    .handle_ipc_request(
                        &data_root,
                        &json!({
                            "op": SOURCE_REFRESH_REQUEST_OP,
                            "mode": "wait",
                            "operation": "import",
                            "explicit_source_catalog": authority.to_json(),
                            "fresh_after_admitted_snapshot": true,
                        }),
                    )?
                    .expect("fresh manual successor");
                *recorded_successor.lock().unwrap() = Some(request_id(&successor));
                Err(anyhow!("injected terminal provider failure"))
            },
            || Ok(None),
            |job| write_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root), job),
            |_| Ok(()),
        )
        .unwrap();
    assert!(run.failed);
    let successor_id = successor_id.lock().unwrap().clone().unwrap();
    drop(first);

    let restarted = CoreRefreshEngine::new();
    assert!(restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    assert_eq!(
        restarted.status(&successor_id).unwrap()["request_state"],
        "queued"
    );
    assert!(restarted.has_pending_request());
}

#[test]
fn failed_terminal_retry_journals_successor_before_restart() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let authority = load_explicit_source_catalog_authority(&data_root).unwrap();
    let first = CoreRefreshEngine::new();
    let active = first.enqueue_periodic(&data_root).unwrap();
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

    let successor = first
        .handle_ipc_request(
            &data_root,
            &json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "operation": "import",
                "explicit_source_catalog": authority.to_json(),
                "fresh_after_admitted_snapshot": true,
            }),
        )
        .unwrap()
        .expect("fresh successor during terminal retry");
    let successor_id = request_id(&successor);
    let pending = read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root))
        .expect("failed root with durable successor");
    assert_eq!(pending["request_id"], active_id);
    assert_eq!(pending["queued_successors"][0]["request_id"], successor_id);

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

    let restarted = CoreRefreshEngine::new();
    assert!(restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    assert_eq!(
        restarted.status(&successor_id).unwrap()["request_state"],
        "queued"
    );
    assert!(restarted.has_pending_request());
}
