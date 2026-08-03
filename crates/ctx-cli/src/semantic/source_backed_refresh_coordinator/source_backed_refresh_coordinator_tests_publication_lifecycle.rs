use super::*;

#[test]
fn failed_refresh_retains_the_previous_published_generation() {
    let coordinator = CoreRefreshEngine::new();
    let request = coordinator.enqueue(Some("generation-1".to_owned()));
    let request_id = request_id(&request);
    let run = coordinator
        .run_next_with(
            |request_id, coordinator| {
                let _ = coordinator.set_progress(
                    request_id,
                    SourceBackedRefreshProgressUpdate {
                        phase: "refreshing".to_owned(),
                        completed_sources: 0,
                        total_sources: 1,
                        current_source: Some("source-a".to_owned()),
                        completed_records: Some(3),
                        completed_bytes: Some(384),
                        current_source_progress: None,
                    },
                );
                Err(anyhow!("injected writer failure before publication"))
            },
            || Ok(Some("generation-1".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("queued refresh");

    assert!(run.failed);
    assert!(!run.did_work);
    let status = coordinator
        .status(&request_id)
        .expect("failed request status");
    assert_eq!(status["request_state"], "failed");
    assert_eq!(status["previous_generation"], "generation-1");
    assert_eq!(status["published_generation"], "generation-1");
    assert!(status.get("generation_changed").is_none());
    assert!(status.get("receipt").is_none());
    assert!(status["last_error"]
        .as_str()
        .is_some_and(|error| error.contains("injected writer failure")));
    assert_eq!(run.job["status"], "failed");
    assert_eq!(run.job["published_generation"], "generation-1");
    assert_eq!(run.job["progress"]["phase"], "failed");
    assert!(run.job["progress"].get("completed_records").is_none());
    assert!(run.job["progress"].get("completed_bytes").is_none());
}

#[test]
fn all_cold_route_failures_keep_their_typed_daemon_classification() {
    let cases = [
        (
            SourceBackedSourceFailureClass::Unavailable,
            "source_unavailable",
        ),
        (
            SourceBackedSourceFailureClass::SourceChanged,
            "source_changed",
        ),
        (
            SourceBackedSourceFailureClass::Unreadable,
            "malformed_source",
        ),
        (
            SourceBackedSourceFailureClass::Incompatible,
            "unsupported_schema",
        ),
    ];
    for (index, (class, expected)) in cases.into_iter().enumerate() {
        let coordinator = CoreRefreshEngine::new();
        let _ = coordinator.enqueue(None);
        let route_identity =
            SourceRouteIdentity::from_sha256(format!("{index:02x}").repeat(32)).unwrap();
        let run = coordinator
            .run_next_with(
                |_, _| {
                    Err(SourceBackedCoordinatorError::NoUsableSourceRoutes {
                        failed_routes: SourceBackedSourceFailures::from_failures([
                            SourceBackedFailedRoute::new(
                                route_identity,
                                "11".repeat(32),
                                CaptureProvider::Codex,
                                class,
                                false,
                                "fixture source",
                                "fixture failure",
                            ),
                        ]),
                    }
                    .into())
                },
                || Ok(None),
                |_| Ok(()),
                |_| Ok(()),
            )
            .unwrap();
        assert!(run.failed);
        assert_eq!(run.job["failure_type"], expected, "{:#?}", run.job);
        let last_error = run.job["last_error"].as_str().unwrap();
        assert!(last_error.contains("codex"), "{last_error}");
        assert!(last_error.contains("fixture failure"), "{last_error}");
        assert!(!last_error.contains(&"11".repeat(32)), "{last_error}");
    }
}

#[test]
fn mixed_cold_route_failures_keep_a_typed_aggregate_classification() {
    let coordinator = CoreRefreshEngine::new();
    let _ = coordinator.enqueue(None);
    let route = |byte: u8, class| {
        SourceBackedFailedRoute::new(
            SourceRouteIdentity::from_sha256(format!("{byte:02x}").repeat(32)).unwrap(),
            format!("{:02x}", byte.saturating_add(1)).repeat(32),
            CaptureProvider::Codex,
            class,
            false,
            "fixture source",
            "fixture failure",
        )
    };
    let run = coordinator
        .run_next_with(
            |_, _| {
                Err(SourceBackedCoordinatorError::NoUsableSourceRoutes {
                    failed_routes: SourceBackedSourceFailures::from_failures([
                        route(1, SourceBackedSourceFailureClass::Unavailable),
                        route(2, SourceBackedSourceFailureClass::SourceChanged),
                    ]),
                }
                .into())
            },
            || Ok(None),
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
    assert!(run.failed);
    assert_eq!(run.job["failure_type"], "source_failures", "{:#?}", run.job);
}

#[test]
fn unverified_returned_generation_is_never_recorded_as_published() {
    let coordinator = CoreRefreshEngine::new();
    let request = coordinator.enqueue(Some("generation-1".to_owned()));
    let request_id = request_id(&request);
    let run = coordinator
        .run_next_with(
            |_, _| Ok(test_publication("generation-2")),
            || Ok(Some("generation-1".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("queued refresh");

    assert!(run.failed);
    assert!(!run.did_work);
    let status = coordinator
        .status(&request_id)
        .expect("failed request status");
    assert_eq!(status["request_state"], "failed");
    assert_eq!(status["previous_generation"], "generation-1");
    assert_eq!(status["published_generation"], "generation-1");
    assert!(status["last_error"]
        .as_str()
        .is_some_and(|error| error.contains("returned generation generation-2")));
}

#[test]
fn verified_publication_atomically_installs_pinned_core_receipt() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let coordinator = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let receipt = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?
            .commit(|_| true)?;
            Ok(empty_test_publication(receipt.generation_id))
        },
    ));
    coordinator.enqueue_periodic(&data_root).unwrap();

    let run = coordinator.run_next(&data_root).expect("queued refresh");
    let pinned = coordinator
        .pinned_core_publication()
        .expect("pinned Core publication");

    assert!(!run.failed);
    assert_eq!(pinned.generation_id(), run.job["published_generation"]);
    assert_eq!(
        pinned.receipt().published_generation,
        pinned
            .verified_index()
            .expect("verified Core index")
            .generation_id()
    );
    assert!(!coordinator.has_pending_request());
}

#[test]
fn terminal_generation_can_be_pinned_after_one_successor_advances_active() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    let terminal_generation = publish_pin_source(&index_root, publication_pin_source());
    let successor_generation =
        publish_pin_source(&index_root, publication_pin_source_with_anchor(0x93));
    assert_ne!(terminal_generation, successor_generation);

    let terminal = pin_retained_generation(&data_root, &terminal_generation).unwrap();
    let active = pin_published_generation(&data_root).unwrap().unwrap();

    assert_eq!(terminal.generation_id(), terminal_generation);
    assert_eq!(active.generation_id(), successor_generation);
}

#[test]
fn cold_dirty_routes_are_published_in_one_all_route_generation() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let routes = BTreeSet::from([route_identity(0x94), route_identity(0x95)]);
    let executor_routes = routes.clone();
    let scopes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let executor_scopes = Arc::clone(&scopes);
    let coordinator = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            executor_scopes
                .lock()
                .unwrap()
                .push(execution.scope.clone());
            let selected = physically_selected_routes(&execution, &executor_routes);
            publish_selected_routes(&execution, &selected, None)
        },
    ));
    coordinator.reconcile_watch_routes(
        routes,
        EventWatermark::new(1, 0),
        ledger_now_ms().saturating_sub(1_000),
    );

    assert!(coordinator
        .enqueue_next_scheduled_refresh(&data_root, ledger_now_ms())
        .unwrap());
    let run = coordinator.run_next(&data_root).unwrap();

    assert!(!run.failed, "{:#}", run.job);
    assert_eq!(*scopes.lock().unwrap(), vec![SourceBackedRefreshScope::All]);
    assert!(!coordinator.has_scheduled_route_work());
}

#[test]
fn publication_remains_running_until_exact_pin_authority_exists() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let publish_nonempty = Arc::new(AtomicBool::new(false));
    let coordinator = Arc::new(CoreRefreshEngine::with_executor(publication_pin_executor(
        Arc::clone(&publish_nonempty),
    )));
    coordinator.enqueue_periodic(&data_root).unwrap();
    assert!(!coordinator.run_next(&data_root).unwrap().failed);
    let prior = coordinator
        .pinned_core_publication()
        .expect("prior retained authority");

    publish_nonempty.store(true, Ordering::SeqCst);
    let queued = coordinator.enqueue_periodic(&data_root).unwrap();
    let request_id = request_id(&queued);
    let (gate, opener_started, opener_release) = RunningRefreshGate::new();
    std::thread::scope(|scope| {
        let runner = Arc::clone(&coordinator);
        let runner_root = data_root.clone();
        let handle = scope.spawn(move || {
            runner
                .run_next_with_verified_index_opener(&runner_root, |index_root| {
                    opener_started.send(()).expect("signal pin opener");
                    let _ = opener_release.recv();
                    Ok(Arc::new(open_verified_index(index_root)?))
                })
                .expect("queued publication")
        });
        gate.wait_until_started();

        let running = coordinator.status(&request_id).expect("running request");
        assert_eq!(running["request_state"], "running");
        assert_eq!(running["published_generation"], prior.generation_id());
        let durable = pin_published_generation(&data_root)
            .unwrap()
            .expect("new durable generation");
        assert_ne!(durable.generation_id(), prior.generation_id());
        let visible = coordinator
            .pinned_core_publication()
            .expect("prior authority remains visible");
        assert!(Arc::ptr_eq(&prior, &visible));

        gate.release();
        let run = handle.join().expect("publication runner");
        assert!(!run.failed);
    });

    let published = coordinator.status(&request_id).expect("published request");
    assert_eq!(published["request_state"], "published");
    let current = coordinator
        .pinned_core_publication()
        .expect("current retained authority");
    assert_ne!(current.generation_id(), prior.generation_id());
    assert_eq!(current.generation_id(), published["published_generation"]);
}

#[test]
fn mismatched_pin_fails_without_rebinding_stale_prior_authority() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let publish_nonempty = Arc::new(AtomicBool::new(false));
    let coordinator =
        CoreRefreshEngine::with_executor(publication_pin_executor(Arc::clone(&publish_nonempty)));
    coordinator.enqueue_periodic(&data_root).unwrap();
    assert!(!coordinator.run_next(&data_root).unwrap().failed);
    let prior = coordinator
        .pinned_core_publication()
        .expect("prior retained authority");
    let stale_index = prior.verified_index().expect("prior verified index");

    publish_nonempty.store(true, Ordering::SeqCst);
    let queued = coordinator.enqueue_periodic(&data_root).unwrap();
    let request_id = request_id(&queued);
    let run = coordinator
        .run_next_with_verified_index_opener(&data_root, |_| Ok(stale_index))
        .expect("mismatched publication attempt");

    assert!(run.failed);
    assert_eq!(run.job["request_state"], "failed");
    assert!(run.job.get("post_publication_error").is_none());
    assert!(run.job["last_error"]
        .as_str()
        .is_some_and(|error| error.contains("verified pin carries")));
    let retained = coordinator
        .pinned_core_publication()
        .expect("prior authority remains retained");
    assert!(Arc::ptr_eq(&prior, &retained));
    let durable = pin_published_generation(&data_root)
        .unwrap()
        .expect("new durable generation exists");
    assert_ne!(durable.generation_id(), retained.generation_id());
    assert_eq!(
        coordinator.status(&request_id).unwrap()["request_state"],
        "failed"
    );
}

#[test]
fn missing_pin_retries_exact_route_and_reopens_without_stale_authority() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let publish_nonempty = Arc::new(AtomicBool::new(false));
    let coordinator =
        CoreRefreshEngine::with_executor(publication_pin_executor(Arc::clone(&publish_nonempty)));
    coordinator.enqueue_periodic(&data_root).unwrap();
    assert!(!coordinator.run_next(&data_root).unwrap().failed);
    let prior = coordinator
        .pinned_core_publication()
        .expect("prior retained authority");

    let route = route_identity(0xa1);
    coordinator.reconcile_watch_routes(
        [route.clone()],
        EventWatermark::new(7, 0),
        ledger_now_ms().saturating_sub(1_000),
    );
    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());
    publish_nonempty.store(true, Ordering::SeqCst);
    let injected_opens = AtomicUsize::new(0);
    let failed = coordinator
        .run_next_with_verified_index_opener(&data_root, |_| {
            injected_opens.fetch_add(1, Ordering::SeqCst);
            coordinator.record_watch_routes(
                [(route.clone(), EventWatermark::new(7, 1))],
                ledger_now_ms().saturating_sub(1_000),
            );
            Err(anyhow!("injected missing exact generation pin"))
        })
        .expect("missing-pin publication attempt");

    assert_eq!(injected_opens.load(Ordering::SeqCst), 1);
    assert!(failed.failed);
    assert_eq!(failed.job["request_state"], "failed");
    assert!(failed.job.get("post_publication_error").is_none());
    assert!(failed.job["last_error"]
        .as_str()
        .is_some_and(|error| error.contains("injected missing exact generation pin")));
    let retained = coordinator
        .pinned_core_publication()
        .expect("prior authority remains retained");
    assert!(Arc::ptr_eq(&prior, &retained));
    let durable = pin_published_generation(&data_root)
        .unwrap()
        .expect("committed generation survives missing pin");
    assert_ne!(durable.generation_id(), retained.generation_id());
    assert!(coordinator.has_scheduled_route_work());

    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());
    let retried = coordinator
        .run_next(&data_root)
        .expect("retry reopens durable generation");
    assert!(!retried.failed, "{:#}", retried.job);
    let reopened = coordinator
        .pinned_core_publication()
        .expect("retried authority");
    assert_ne!(reopened.generation_id(), prior.generation_id());
    assert_eq!(
        reopened.generation_id(),
        retried.job["published_generation"]
    );
    assert!(!coordinator.has_scheduled_route_work());
}

#[test]
fn failed_post_commit_probe_is_not_reopened_in_the_same_cycle() {
    let temp = tempfile::tempdir().unwrap();
    let coordinator = CoreRefreshEngine::with_executor(Arc::new(TestExecutor {
        calls: Arc::new(AtomicUsize::new(0)),
        generation_id: "claimed-generation".to_owned(),
        failure: None,
    }));
    coordinator.enqueue(None);

    let run = coordinator
        .run_next(temp.path())
        .expect("queued refresh must run");
    assert!(run.failed);
    assert!(run.job["last_error"]
        .as_str()
        .is_some_and(|error| error.contains("already failed in this refresh cycle")));
}

#[test]
fn terminal_persist_failure_retries_exact_receipt_without_reexecuting_refresh() {
    let coordinator = CoreRefreshEngine::new();
    let failed_callbacks = AtomicUsize::new(0);
    let request = coordinator.enqueue(Some("generation-a".to_owned()));
    let request_id = request_id(&request);

    let run = coordinator
        .run_next_with(
            |_, _| Ok(test_publication("generation-b")),
            || Ok(Some("generation-b".to_owned())),
            |_| Err(anyhow!("injected terminal persistence failure")),
            |_| {
                failed_callbacks.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .expect("committed refresh with failed terminal persistence");

    assert!(!run.failed);
    assert!(!run.did_work);
    assert!(run.terminal_persistence_pending);
    assert_eq!(run.job["request_state"], "running");
    assert_eq!(run.job["progress"]["phase"], "persisting_terminal");
    assert!(run.job["last_error"]
        .as_str()
        .is_some_and(|error| error.contains("injected terminal persistence failure")));
    assert_eq!(failed_callbacks.load(Ordering::SeqCst), 0);
    assert_eq!(
        coordinator.status(&request_id).unwrap()["request_state"],
        "running"
    );

    let retry = coordinator
        .run_next_with(
            |_, _| panic!("terminal persistence retry must not execute capture"),
            || panic!("terminal persistence retry must not reopen Core"),
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("terminal persistence retry");
    assert!(!retry.failed, "{:#}", retry.job);
    assert!(!retry.terminal_persistence_pending);
    assert!(retry.did_work);
    assert_eq!(retry.job["request_id"], request_id);
    assert_eq!(retry.job["request_state"], "published");
    assert!(retry.job.get("failure_type").is_none());
    assert!(retry.job.get("last_error").is_none());
}

#[test]
fn terminal_persist_retry_retains_admissions_without_readmitting_routes() {
    let coordinator = CoreRefreshEngine::new();
    let route = route_identity(0x5a);
    coordinator.initialize_watch_route_authority([route.clone()]);
    coordinator.record_watch_routes(
        [(route.clone(), EventWatermark::new(1, 1))],
        ledger_now_ms(),
    );
    let request = coordinator.enqueue(Some("generation-a".to_owned()));
    let request_id = request_id(&request);
    let scope = SourceBackedRefreshScope::All;
    assert!(coordinator
        .admit_refresh_scope_for_test(&request_id, &scope)
        .unwrap()
        .is_empty());

    let mut publication = test_publication("generation-b");
    publication.route_results = vec![SourceBackedRefreshRouteResult::succeeded(
        route.as_str().to_owned(),
        true,
    )];
    let run = coordinator
        .run_next_with(
            |_, _| Ok(publication),
            || Ok(Some("generation-b".to_owned())),
            |_| Err(anyhow!("injected terminal persistence failure")),
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(run.job["request_state"], "running");
    assert!(run.terminal_persistence_pending);

    let retry_error = coordinator
        .admit_refresh_scope_for_test(&request_id, &scope)
        .unwrap_err();
    assert!(
        format!("{retry_error:#}").contains("already has retained route admissions"),
        "{retry_error:#}"
    );
}

#[test]
fn recovered_wait_after_restart_attaches_to_equivalent_running_attempt() {
    let temp = tempfile::tempdir().unwrap();
    let authority = load_explicit_source_catalog_authority(temp.path()).unwrap();
    let first = CoreRefreshEngine::new();
    let interrupted = first.enqueue_periodic(temp.path()).unwrap();
    let interrupted_request_id = request_id(&interrupted);
    let crash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = first.run_next_with(
            |_, _| panic!("injected daemon process interruption"),
            || Ok(None),
            |_| Ok(()),
            |_| Ok(()),
        );
    }));
    assert!(crash.is_err());
    let running_job = first
        .set_progress(
            &interrupted_request_id,
            SourceBackedRefreshProgressUpdate {
                phase: "refreshing".to_owned(),
                completed_sources: 0,
                total_sources: 1,
                current_source: Some("interrupted-source".to_owned()),
                completed_records: Some(5),
                completed_bytes: Some(640),
                current_source_progress: None,
            },
        )
        .expect("interrupted running job");
    assert_eq!(running_job["progress"]["completed_records"], 5);
    assert_eq!(running_job["progress"]["completed_bytes"], 640);
    write_daemon_job_status(
        &daemon_source_backed_refresh_job_path(temp.path()),
        &running_job,
    )
    .unwrap();
    drop(first);

    let restarted = Arc::new(CoreRefreshEngine::new());
    let active = restarted
        .handle_ipc_request(
            temp.path(),
            &json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "operation": "import",
                "explicit_source_catalog": authority.to_json(),
            }),
        )
        .unwrap()
        .expect("restarted explicit request");
    let active_request_id = request_id(&active);
    assert_ne!(active_request_id, interrupted_request_id);
    let (gate, runner_started, runner_release) = RunningRefreshGate::new();

    let recovered = std::thread::scope(|scope| {
        let runner = Arc::clone(&restarted);
        let runner_authority = authority.clone();
        scope.spawn(move || {
            let run = runner
                .run_next_with(
                    |_, _| {
                        runner_started.send(()).expect("signal running refresh");
                        let _ = runner_release.recv();
                        let mut publication = test_publication("restart-generation");
                        publication.published_explicit_source_catalog = Some(runner_authority);
                        Ok(publication)
                    },
                    || Ok(Some("restart-generation".to_owned())),
                    |_| Ok(()),
                    |_| Ok(()),
                )
                .expect("restarted running refresh");
            assert!(!run.failed, "{:#}", run.job);
        });
        gate.wait_until_started();

        let recovered = restarted
            .handle_ipc_request(
                temp.path(),
                &json!({
                    "op": SOURCE_REFRESH_REQUEST_OP,
                    "mode": "wait",
                    "operation": "import",
                    "explicit_source_catalog": authority.to_json(),
                }),
            )
            .unwrap()
            .expect("recovered wait refresh response");
        gate.release();
        recovered
    });

    assert_eq!(request_id(&recovered), active_request_id);
    let terminal = restarted.status(&active_request_id).unwrap();
    assert_eq!(terminal["request_state"], "published");
    assert_eq!(
        terminal["receipt"]["published_generation"],
        "restart-generation"
    );
    assert!(!restarted.has_pending_request());
}

#[test]
fn restart_discards_incomplete_candidate_and_publishes_from_last_good() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let first = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let _writer = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?;
            Err(anyhow!("injected cancellation before commit"))
        },
    ));
    first.enqueue_periodic(&data_root).unwrap();
    let failed = first.run_next(&data_root).expect("cancelled refresh");
    assert!(failed.failed);
    assert!(pin_published_generation(&data_root).unwrap().is_none());
    drop(first);

    let restarted = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let receipt = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?
            .commit(|_| true)?;
            Ok(empty_test_publication(receipt.generation_id))
        },
    ));
    restarted.enqueue_periodic(&data_root).unwrap();
    let published = restarted.run_next(&data_root).expect("restart refresh");

    assert!(!published.failed);
    let pinned = restarted
        .pinned_core_publication()
        .expect("restart publication pin");
    assert_eq!(
        pinned.generation_id(),
        published.job["published_generation"]
    );
}

#[test]
fn restart_after_pointer_publication_recovers_exact_receipt_without_recapture() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let first = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let request_id = execution.request_id.to_owned();
            let operation = execution.operation;
            let scope = execution.scope.clone();
            let published = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?
            .commit_with_publication_metadata(
                |_| true,
                |context| {
                    let publication = SourceBackedRefreshPublication {
                        generation_id: context.generation_id().to_owned(),
                        published_explicit_source_catalog: None,
                        unsupported_routes: 0,
                        certified_source_count: 0,
                        certified_source_bytes: 0,
                        current: SourceBackedRefreshCurrent::default(),
                        timings: SourceBackedRefreshTimings::default(),
                        route_results: Vec::new(),
                        catalog_route_bindings: Vec::new(),
                        verified_index: None,
                    };
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
            Err(anyhow!(
                "injected cancellation after commit {}",
                published.receipt().generation_id
            ))
        },
    ));
    first.enqueue_periodic(&data_root).unwrap();
    let failed = first.run_next(&data_root).expect("cancelled refresh");
    assert!(failed.failed);
    let committed = pin_published_generation(&data_root)
        .unwrap()
        .expect("atomic commit survives cancellation")
        .generation_id()
        .to_owned();
    drop(first);

    let executions = Arc::new(AtomicUsize::new(0));
    let observed_executions = Arc::clone(&executions);
    let restarted = CoreRefreshEngine::with_executor(Arc::new(
        move |_execution: SourceBackedRefreshExecution<'_>| {
            observed_executions.fetch_add(1, Ordering::SeqCst);
            Err(anyhow!("recovery must not reexecute committed Core work"))
        },
    ));
    assert!(!restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    assert!(!restarted.has_pending_request());
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    let recovered = read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root))
        .expect("typed interrupted-publication recovery status");
    assert_eq!(recovered["request_state"], "published");
    assert_eq!(recovered["published_generation"], committed);
    assert_eq!(recovered["outcome"], "completed");
    assert!(recovered.get("receipt").is_some());
    assert_eq!(
        pin_published_generation(&data_root)
            .unwrap()
            .expect("committed Core remains readable")
            .generation_id(),
        committed
    );
}

#[test]
fn active_generation_pin_fails_closed_when_core_state_is_missing() {
    let temp = tempfile::tempdir().unwrap();
    let error = match pin_active_verified_generation(temp.path()) {
        Ok(_) => panic!("missing Core state must not fall back to a helper receipt"),
        Err(error) => error,
    };
    assert!(format!("{error:#}").starts_with("source_unavailable:"));
}

#[test]
fn activated_generation_missing_commit_payload_remains_typed_corruption() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let coordinator = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let writer = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?;
            let receipt = writer.commit(|_| true)?;
            Ok(empty_test_publication(receipt.generation_id))
        },
    ));
    coordinator.enqueue_periodic(&data_root).unwrap();
    let run = coordinator.run_next(&data_root).expect("initial refresh");
    assert!(!run.failed);
    write_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root), &run.job).unwrap();

    let index_root = source_backed_index_root(&data_root);
    let pointer: Value =
        serde_json::from_slice(&std::fs::read(index_root.join("active-generation.json")).unwrap())
            .unwrap();
    let directory = pointer["active"]["directory"].as_str().unwrap();
    let meta_path = index_root
        .join("index-generations")
        .join(directory)
        .join("meta.json");
    let mut meta: Value = serde_json::from_slice(&std::fs::read(&meta_path).unwrap()).unwrap();
    assert!(meta.as_object_mut().unwrap().remove("payload").is_some());
    std::fs::write(&meta_path, serde_json::to_vec(&meta).unwrap()).unwrap();

    drop(coordinator);
    let restarted = CoreRefreshEngine::new();
    let error = restarted
        .enqueue_periodic(&data_root)
        .expect_err("activated generation corruption must fail closed");
    assert!(matches!(
        error.downcast_ref::<IndexError>(),
        Some(IndexError::MissingCommitPayload)
    ));
    assert!(!restarted.has_pending_request());

    let error = match pin_active_verified_generation(&data_root) {
        Ok(_) => panic!("corrupt active Core state must fail closed before blame"),
        Err(error) => error,
    };
    assert!(format!("{error:#}").starts_with("source_unavailable:"));
}
