use super::*;
use std::sync::atomic::AtomicUsize;

fn private_data_root() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().expect("temporary data root");
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root)
        .expect("private data root");
    (temp, data_root)
}

#[derive(Debug, Clone)]
struct ScopedAdmissionRuntime {
    home: PathBuf,
    cwd: PathBuf,
}

impl RefreshRuntime for ScopedAdmissionRuntime {
    fn metadata(&self, _data_root: &Path, operation: RefreshOperation) -> RefreshRuntimeMetadata {
        RefreshRuntimeMetadata {
            operation,
            ..RefreshRuntimeMetadata::default()
        }
    }

    fn discovery_context(&self, _data_root: &Path) -> Result<DiscoveryContext> {
        Ok(DiscoveryContext::new(
            &self.home,
            &self.cwd,
            ctx_history_capture::DiscoveryPlatform::Linux,
            ctx_history_capture::DiscoveryPlatformDirs::default(),
        ))
    }
}

fn scoped_runtime(root: &Path) -> Arc<dyn RefreshRuntime> {
    let home = root.join("home");
    let cwd = root.join("cwd");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    Arc::new(ScopedAdmissionRuntime { home, cwd })
}

#[derive(Debug, Clone)]
struct MutableProviderRootRuntime {
    home: PathBuf,
    cwd: PathBuf,
    roots: Arc<Mutex<Vec<ctx_history_capture_model::ProviderRootDefinition>>>,
}

impl RefreshRuntime for MutableProviderRootRuntime {
    fn metadata(&self, _data_root: &Path, operation: RefreshOperation) -> RefreshRuntimeMetadata {
        RefreshRuntimeMetadata {
            operation,
            ..RefreshRuntimeMetadata::default()
        }
    }

    fn discovery_context(&self, _data_root: &Path) -> Result<DiscoveryContext> {
        Ok(DiscoveryContext::new(
            &self.home,
            &self.cwd,
            ctx_history_capture::DiscoveryPlatform::Linux,
            ctx_history_capture::DiscoveryPlatformDirs::default(),
        )
        .with_configured_provider_roots(self.roots.lock().unwrap().clone()))
    }
}

#[test]
fn queued_complete_catalog_is_readmitted_when_provider_roots_change() {
    let (temp, data_root) = private_data_root();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    let definition = |id: &str| ctx_history_capture_model::ProviderRootDefinition {
        id: id.to_owned(),
        provider: CaptureProvider::Codex,
        path: temp.path().join(format!("codex-{id}")),
        group: Some(id.to_owned()),
        kind: None,
    };
    let roots = Arc::new(Mutex::new(vec![definition("personal")]));
    let runtime = Arc::new(MutableProviderRootRuntime {
        home,
        cwd,
        roots: Arc::clone(&roots),
    });
    let route = SourceRouteIdentity::from_sha256("52".repeat(32)).unwrap();
    let admission_calls = Arc::new(AtomicUsize::new(0));
    let fence_calls = Arc::clone(&admission_calls);
    let fence_route = route.clone();
    let seen_roots = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    let executor_seen_roots = Arc::clone(&seen_roots);
    let executor = Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
        executor_seen_roots.lock().unwrap().push(
            execution
                .admitted_refresh()
                .discovery()
                .configured_provider_roots()
                .unwrap()
                .iter()
                .map(|root| root.id.clone())
                .collect(),
        );
        let selected = execution
            .admitted_refresh()
            .exact_routes()
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let commit = ctx_history_index::GenerationWriter::open(
            execution.index_root,
            ctx_history_index::WriterOptions::default(),
        )?
        .into_writer()
        .map_err(crate::committed_generation_recovery_error)?
        .commit(|_| true)?;
        Ok(SourceBackedRefreshPublication {
            route_results: selected
                .iter()
                .map(|route| {
                    SourceBackedRefreshRouteResult::succeeded(route.as_str().to_owned(), true)
                })
                .collect(),
            zero_source_authority: selected
                .into_iter()
                .map(|route| SourceBackedZeroSourceAuthority {
                    generation_id: commit.generation_id.clone(),
                    route_identity: route,
                    kind: SourceBackedZeroSourceAuthorityKind::CompleteEmptyInventory,
                })
                .collect(),
            catalog_route_bindings: Vec::new(),
            verified_index: None,
            generation_id: commit.generation_id,
            published_explicit_source_catalog: execution.explicit_source_catalog.cloned(),
            unsupported_routes: 0,
            certified_source_count: 0,
            certified_source_bytes: 0,
            current: SourceBackedRefreshCurrent::default(),
            timings: SourceBackedRefreshTimings::default(),
        })
    });
    let coordinator = CoreRefreshEngine::with_runtime_for_test(
        Arc::new(TestRefreshJournal::default()),
        runtime,
        executor,
        Arc::new(move |_, _, _, _| {
            fence_calls.fetch_add(1, Ordering::SeqCst);
            Ok(BTreeMap::from([(
                fence_route.clone(),
                Some("53".repeat(32)),
            )]))
        }),
    );
    let request_id = "019fcaaa-0000-7000-8000-000000000505";
    let admission = coordinator
        .submit(&data_root, test_refresh_submission(request_id))
        .unwrap();
    release_pending_admission(&coordinator, admission);
    assert!(coordinator
        .prepare_next_pending_admission(&data_root)
        .unwrap());
    assert_eq!(admission_calls.load(Ordering::SeqCst), 1);

    *roots.lock().unwrap() = vec![definition("work")];
    let run = coordinator
        .run_next(&data_root)
        .expect("stale complete catalog should be readmitted");

    assert!(!run.failed, "{:#}", run.job);
    assert_eq!(admission_calls.load(Ordering::SeqCst), 2);
    assert_eq!(*seen_roots.lock().unwrap(), vec![vec!["work".to_owned()]]);
}

fn write_codex_session_fixture(sessions: &Path) {
    fs::write(
        sessions.join("session.jsonl"),
        concat!(
            r#"{"timestamp":"2026-07-30T12:00:00Z","type":"session_meta","payload":{"id":"session","cwd":"/repo/refresh-admission","originator":"codex_cli_rs","cli_version":"1.0.0","source":"cli","model_provider":"openai"}}"#,
            "\n",
            r#"{"timestamp":"2026-07-30T12:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"refresh admission fixture"}]}}"#,
            "\n",
        ),
    )
    .unwrap();
}

fn release_pending_admission(
    coordinator: &CoreRefreshEngine,
    admission: RefreshAdmission,
) -> Value {
    let (status, barrier) = admission.into_parts();
    barrier
        .expect("scoped request should retain a pending-admission barrier")
        .release(coordinator);
    status.schema_v1_fields().clone()
}

fn provider_submission(request_id: &str, provider: CaptureProvider) -> RefreshRequest {
    RefreshRequest::selected_import(request_id.to_owned(), RefreshSelection::Provider(provider))
}

fn assert_unreadable_admission_failure(retain_generation: bool, request_id: &str) {
    let (_temp, data_root) = private_data_root();
    let retained_generation = retain_generation.then(|| {
        ctx_history_index::GenerationWriter::open(
            source_backed_index_root(&data_root),
            ctx_history_index::WriterOptions::default(),
        )
        .unwrap()
        .into_writer()
        .unwrap()
        .commit(|_| true)
        .unwrap()
        .generation_id
    });
    let route = SourceRouteIdentity::from_sha256("ab".repeat(32)).unwrap();
    let failed_route = route.clone();
    let coordinator = CoreRefreshEngine::with_admission_fence_for_test(
        Arc::new(TestRefreshJournal::default()),
        test_refresh_runtime(),
        Arc::new(move |_, _, _, _| {
            Err(SourceBackedCoordinatorError::NoUsableSourceRoutes {
                failed_routes: SourceBackedSourceFailures::from_failures([
                    SourceBackedFailedRoute::new(
                        failed_route.clone(),
                        "cd".repeat(32),
                        CaptureProvider::Shelley,
                        SourceBackedSourceFailureClass::Unreadable,
                        false,
                        "shelley.db",
                        "file is not a database",
                    ),
                ]),
            }
            .into())
        }),
    );
    let admission = coordinator
        .submit(&data_root, test_refresh_submission(request_id))
        .unwrap();
    release_pending_admission(&coordinator, admission);

    let run = coordinator
        .run_next(&data_root)
        .expect("terminal admission failure");
    assert!(run.failed, "{:#}", run.job);
    assert_eq!(run.job["request_state"], "failed");
    assert_eq!(run.job["failure_type"], "malformed_source");
    assert_eq!(run.job["error_code"], "malformed_source");
    assert_eq!(run.job["reason"], "unreadable");
    assert_eq!(run.job["structured_outcome"]["code"], "malformed_source");
    assert_eq!(run.job["structured_outcome"]["class"], "unreadable");
    assert_eq!(run.job["structured_outcome"]["retryable"], false);
    assert_eq!(
        run.job["structured_outcome"]["affected_routes"],
        json!([route.as_str()])
    );
    assert_eq!(
        run.job["structured_outcome"]["blocked_routes"],
        json!([route.as_str()])
    );
    assert_eq!(run.job["structured_outcome"]["retryable_routes"], json!([]));
    assert_eq!(
        run.job["structured_outcome"]["retry_advice"],
        "inspect_sources"
    );
    assert_eq!(
        run.job["structured_outcome"]["retained_generation"],
        json!(retained_generation)
    );
    assert_eq!(run.job["published_generation"], json!(retained_generation));
    assert_eq!(coordinator.pending_scheduler_retry_root_for_test(), None);
    assert_eq!(
        coordinator.dirty_route_ids_for_test(),
        BTreeSet::from([route.clone()])
    );
    assert!(coordinator.route_is_permanently_blocked_for_test(&route));
}

#[test]
fn cold_admission_preserves_typed_unreadable_route_failure() {
    assert_unreadable_admission_failure(false, "019fcaaa-0000-7000-8000-000000000508");
}

#[test]
fn warm_admission_preserves_typed_unreadable_route_failure_and_retained_generation() {
    assert_unreadable_admission_failure(true, "019fcaaa-0000-7000-8000-000000000509");
}

#[test]
fn retryable_admission_failure_schedules_exact_route_without_restart() {
    let (_temp, data_root) = private_data_root();
    let route = SourceRouteIdentity::from_sha256("de".repeat(32)).unwrap();
    let failed_route = route.clone();
    let retry_route = route.clone();
    let executor: Arc<dyn SourceBackedRefreshExecutor> = Arc::new(
        move |_: ctx_history_refresh_execution::SourceBackedRefreshExecution<'_>| {
            Err(SourceBackedAdmissionRouteFailures::try_from_failures([
                ctx_history_refresh_execution::SourceBackedAdmissionRouteFailure::new(
                    retry_route.clone(),
                    SourceBackedRouteErrorKind::ResourceUnavailable,
                    "Shelley registration resource remains unavailable",
                ),
            ])
            .unwrap()
            .into())
        },
    );
    let coordinator = CoreRefreshEngine::with_runtime_for_test(
        Arc::new(TestRefreshJournal::default()),
        test_refresh_runtime(),
        executor,
        Arc::new(move |_, _, _, _| {
            Err(SourceBackedAdmissionRouteFailures::try_from_failures([
                ctx_history_refresh_execution::SourceBackedAdmissionRouteFailure::new(
                    failed_route.clone(),
                    SourceBackedRouteErrorKind::ResourceUnavailable,
                    "Shelley registration exhausted a bounded resource",
                ),
            ])
            .unwrap()
            .into())
        }),
    );
    let request_id = "019fcaaa-0000-7000-8000-000000000510";
    let admission = coordinator
        .submit(&data_root, test_refresh_submission(request_id))
        .unwrap();
    release_pending_admission(&coordinator, admission);

    let run = coordinator.run_next(&data_root).expect("admission failure");

    assert!(run.failed, "{:#}", run.job);
    assert_eq!(run.job["error_code"], "resource_unavailable");
    assert_eq!(run.job["reason"], "resource_unavailable");
    assert_eq!(run.job["structured_outcome"]["retryable"], true);
    assert_eq!(
        run.job["structured_outcome"]["retryable_routes"],
        json!([route.as_str()])
    );
    assert_eq!(run.job["structured_outcome"]["blocked_routes"], json!([]));
    assert_eq!(
        coordinator.dirty_route_ids_for_test(),
        BTreeSet::from([route.clone()])
    );
    assert!(!coordinator.route_is_permanently_blocked_for_test(&route));
    coordinator.reconcile_watch_routes(
        BTreeSet::from([route.clone()]),
        EventWatermark::new(2, 0),
        source_route_ledger_now_ms().saturating_sub(1_000),
    );
    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, source_route_ledger_now_ms())
        .expect("schedule retryable admission route"));
    let retry = coordinator.run_next(&data_root).expect("exact route retry");
    assert!(retry.failed, "{:#}", retry.job);
    assert_eq!(
        retry.scope,
        SourceBackedRefreshScope::Exact(BTreeSet::from([route.clone()]))
    );
    assert_eq!(
        retry.job["error_code"], "resource_unavailable",
        "{:#}",
        retry.job
    );
    assert_eq!(
        retry.job["structured_outcome"]["retryable_routes"],
        json!([route.as_str()])
    );
    assert_eq!(
        coordinator.dirty_route_ids_for_test(),
        BTreeSet::from([route.clone()])
    );
    assert!(!coordinator.route_is_permanently_blocked_for_test(&route));
}

#[test]
fn internal_registration_failure_uses_admission_retry_handoff() {
    let (_temp, data_root) = private_data_root();
    let route = SourceRouteIdentity::from_sha256("fa".repeat(32)).unwrap();
    let failed_route = route.clone();
    let coordinator = CoreRefreshEngine::with_admission_fence_for_test(
        Arc::new(TestRefreshJournal::default()),
        test_refresh_runtime(),
        Arc::new(move |_, _, _, _| {
            Err(SourceBackedAdmissionRouteFailures::try_from_failures([
                ctx_history_refresh_execution::SourceBackedAdmissionRouteFailure::new(
                    failed_route.clone(),
                    SourceBackedRouteErrorKind::Internal,
                    "injected registration invariant failure",
                ),
            ])
            .unwrap()
            .into())
        }),
    );
    let request_id = "019fcaaa-0000-7000-8000-000000000511";
    let admission = coordinator
        .submit(&data_root, test_refresh_submission(request_id))
        .unwrap();
    release_pending_admission(&coordinator, admission);

    let run = coordinator.run_next(&data_root).expect("admission failure");

    assert!(run.failed, "{:#}", run.job);
    assert_eq!(run.job["error_code"], "source_refresh_admission_failed");
    assert_eq!(run.job["reason"], "control_plane");
    assert_eq!(run.job["structured_outcome"]["retryable"], true);
    assert_eq!(
        run.job["structured_outcome"]["retry_advice"],
        "retry_admission"
    );
    assert_eq!(run.job["structured_outcome"]["affected_routes"], json!([]));
    assert_eq!(
        coordinator.pending_scheduler_retry_root_for_test(),
        Some(request_id.to_owned())
    );
    assert!(!coordinator.route_is_permanently_blocked_for_test(&route));
}

#[test]
fn automatic_provider_admission_is_exact_on_a_fresh_root_without_global_discovery() {
    let (temp, data_root) = private_data_root();
    let runtime = scoped_runtime(temp.path());
    let sessions = temp.path().join("home/.codex/sessions");
    fs::create_dir_all(&sessions).unwrap();
    write_codex_session_fixture(&sessions);
    let unrelated = temp.path().join("home/.claude/projects/unrelated");
    fs::create_dir_all(&unrelated).unwrap();
    fs::write(unrelated.join("broken.jsonl"), "not-json\n").unwrap();
    // If provider selection widens to the all-provider fence, this test fails.
    let coordinator = CoreRefreshEngine::with_admission_fence_for_test(
        Arc::new(TestRefreshJournal::default()),
        runtime,
        Arc::new(|_, _, _, _| panic!("provider selection invoked global discovery")),
    );
    let request_id = "019fcaaa-0000-7000-8000-000000000501";
    let admission = coordinator
        .submit(
            &data_root,
            provider_submission(request_id, CaptureProvider::Codex),
        )
        .unwrap();
    assert_eq!(
        release_pending_admission(&coordinator, admission)["request_state"],
        "admission_pending"
    );

    assert!(coordinator
        .prepare_next_pending_admission(&data_root)
        .unwrap());
    let status = status_value(&coordinator, request_id);
    assert_eq!(status["request_state"], "queued");
    let scope = refresh_scope_from_json(status.get("refresh_scope")).unwrap();
    let SourceBackedRefreshScope::Exact(routes) = scope else {
        panic!("provider selection did not resolve to an exact scope");
    };
    assert!(!routes.is_empty());
    // Fresh-root exact execution is seeded from request-local authority, not
    // from global known routes or a previously installed watch catalog.
    let run = coordinator
        .run_next(&data_root)
        .expect("admitted provider request should execute");
    assert!(!run.failed, "{}", run.job);
    assert_eq!(run.scope, SourceBackedRefreshScope::Exact(routes.clone()));
    let receipt_routes = run.job["receipt"]["route_results"]
        .as_object()
        .expect("scoped publication receipt routes");
    assert_eq!(receipt_routes.len(), routes.len());
    assert!(routes
        .iter()
        .all(|route| receipt_routes.contains_key(route.as_str())));
}

#[test]
fn unavailable_provider_admission_fails_without_widening() {
    let (temp, data_root) = private_data_root();
    let runtime = scoped_runtime(temp.path());
    let coordinator = CoreRefreshEngine::with_admission_fence_for_test(
        Arc::new(TestRefreshJournal::default()),
        runtime,
        Arc::new(|_, _, _, _| panic!("empty provider selection invoked global discovery")),
    );
    let request_id = "019fcaaa-0000-7000-8000-000000000502";
    let admission = coordinator
        .submit(
            &data_root,
            provider_submission(request_id, CaptureProvider::Claude),
        )
        .unwrap();
    release_pending_admission(&coordinator, admission);

    assert!(coordinator
        .prepare_next_pending_admission(&data_root)
        .unwrap());
    let status = status_value(&coordinator, request_id);
    assert_eq!(status["request_state"], "failed");
    assert!(
        status["last_error"]
            .as_str()
            .is_some_and(|error| error.contains("automatic provider `claude`")
                && error.contains("no executable source routes")),
        "{status:#}"
    );
    assert_eq!(status["refresh_scope"], json!({ "kind": "all" }));
    assert_eq!(status["error_code"], "source_refresh_admission_failed");
    assert_eq!(status["reason"], "control_plane");
    assert_eq!(status["structured_outcome"]["retryable"], true);
    assert_eq!(
        status["structured_outcome"]["retry_advice"],
        "retry_admission"
    );
}

#[test]
fn explicit_catalog_admission_uses_only_its_exact_path_authority() {
    let (temp, data_root) = private_data_root();
    let runtime = scoped_runtime(temp.path());
    let source_path = temp.path().join("requested-history.jsonl");
    fs::write(&source_path, "{}\n").unwrap();
    let source = crate::explicit_source_for_path(&data_root, &source_path, None, true).unwrap();
    let authority = crate::upsert_explicit_source(&data_root, &source)
        .unwrap()
        .authority;
    let coordinator = CoreRefreshEngine::with_admission_fence_for_test(
        Arc::new(TestRefreshJournal::default()),
        runtime,
        Arc::new(|_, _, _, _| panic!("explicit path invoked all-provider discovery")),
    );
    let request_id = "019fcaaa-0000-7000-8000-000000000503";
    let admission = coordinator
        .submit(
            &data_root,
            RefreshRequest::selected_import(
                request_id.to_owned(),
                RefreshSelection::ExactSource(authority),
            ),
        )
        .unwrap();
    release_pending_admission(&coordinator, admission);

    assert!(coordinator
        .prepare_next_pending_admission(&data_root)
        .unwrap());
    let status = status_value(&coordinator, request_id);
    assert_eq!(status["request_state"], "queued");
    let scope = refresh_scope_from_json(status.get("refresh_scope")).unwrap();
    assert!(matches!(scope, SourceBackedRefreshScope::Exact(ref routes) if routes.len() == 1));
}

#[test]
fn explicit_catalog_path_disappearance_has_a_typed_terminal_outcome() {
    let (temp, data_root) = private_data_root();
    let runtime = scoped_runtime(temp.path());
    let source_path = temp.path().join("requested-history.jsonl");
    fs::write(&source_path, "{}\n").unwrap();
    let source = crate::explicit_source_for_path(&data_root, &source_path, None, true).unwrap();
    let authority = crate::upsert_explicit_source(&data_root, &source)
        .unwrap()
        .authority;
    let coordinator = CoreRefreshEngine::with_admission_fence_for_test(
        Arc::new(TestRefreshJournal::default()),
        runtime,
        Arc::new(|_, _, _, _| panic!("explicit path invoked all-provider discovery")),
    );
    let request_id = "019fcaaa-0000-7000-8000-000000000506";
    let admission = coordinator
        .submit(
            &data_root,
            RefreshRequest::selected_import(
                request_id.to_owned(),
                RefreshSelection::ExactSource(authority),
            ),
        )
        .unwrap();
    fs::remove_file(&source_path).unwrap();
    release_pending_admission(&coordinator, admission);

    assert!(coordinator
        .prepare_next_pending_admission(&data_root)
        .unwrap());
    let status = status_value(&coordinator, request_id);
    assert_eq!(status["request_state"], "failed");
    assert_eq!(status["error_code"], "explicit_source_path_missing");
    assert_eq!(status["reason"], "unavailable");
    assert_eq!(status["failure_type"], "source_unavailable");
    assert_eq!(status["structured_outcome"]["retryable"], true);
    assert_eq!(
        status["structured_outcome"]["retry_advice"],
        "inspect_sources"
    );
    assert!(status["last_error"]
        .as_str()
        .is_some_and(|detail| detail.contains(source_path.to_string_lossy().as_ref())));
}

#[test]
fn explicit_catalog_admission_does_not_inherit_running_all_route_work() {
    let (temp, data_root) = private_data_root();
    let runtime = scoped_runtime(temp.path());
    let source_path = temp.path().join("requested-history.jsonl");
    fs::write(&source_path, "{}\n").unwrap();
    let source = crate::explicit_source_for_path(&data_root, &source_path, None, true).unwrap();
    let authority = crate::upsert_explicit_source(&data_root, &source)
        .unwrap()
        .authority;
    let coordinator = CoreRefreshEngine::with_admission_fence_for_test(
        Arc::new(TestRefreshJournal::default()),
        runtime,
        Arc::new(|_, _, _, _| panic!("explicit path invoked all-provider discovery")),
    );
    let predecessor = coordinator.enqueue_periodic(&data_root).unwrap();
    let predecessor_id = predecessor["request_id"].as_str().unwrap().to_owned();
    {
        let mut state = coordinator.lock_state();
        let predecessor = find_attempt_mut(&mut state, &predecessor_id).unwrap();
        predecessor.state = SourceBackedRefreshState::Running;
        predecessor.started_at_ms = Some(utc_now().timestamp_millis());
        predecessor.progress.phase = "refreshing".to_owned();
    }
    let request_id = "019fcaaa-0000-7000-8000-000000000505";
    let admission = coordinator
        .submit(
            &data_root,
            RefreshRequest::selected_import(
                request_id.to_owned(),
                RefreshSelection::ExactSource(authority),
            ),
        )
        .unwrap();
    release_pending_admission(&coordinator, admission);

    let pending = status_value(&coordinator, request_id);
    assert_eq!(pending["request_state"], "admission_pending");
    assert_eq!(pending["physical_attempt_id"], request_id);
    assert!(pending["coalesced_into_request_id"].is_null());
}

#[test]
fn recovered_provider_scope_is_rehydrated_and_cannot_widen() {
    let (temp, data_root) = private_data_root();
    let runtime = scoped_runtime(temp.path());
    let sessions = temp.path().join("home/.codex/sessions");
    fs::create_dir_all(&sessions).unwrap();
    write_codex_session_fixture(&sessions);
    let journal = Arc::new(TestRefreshJournal::default());
    let request_id = "019fcaaa-0000-7000-8000-000000000504";
    let first = CoreRefreshEngine::with_admission_fence_for_test(
        Arc::clone(&journal) as Arc<dyn RefreshJournal>,
        Arc::clone(&runtime),
        Arc::new(|_, _, _, _| panic!("provider selection invoked global discovery")),
    );
    let admission = first
        .submit(
            &data_root,
            provider_submission(request_id, CaptureProvider::Codex),
        )
        .unwrap();
    release_pending_admission(&first, admission);
    assert!(first.prepare_next_pending_admission(&data_root).unwrap());
    let admitted_scope = status_value(&first, request_id)["refresh_scope"].clone();
    let persisted_scope = refresh_scope_from_json(Some(&admitted_scope)).unwrap();
    assert_eq!(admitted_scope["kind"], "exact");
    drop(first);

    // A newly available route for the same provider must not widen the exact
    // request that was already persisted before the restart.
    fs::write(
        temp.path().join("home/.codex/history.jsonl"),
        r#"{"session_id":"new-route","ts":1,"text":"later"}"#,
    )
    .unwrap();
    let recovered = CoreRefreshEngine::with_admission_fence_for_test(
        journal as Arc<dyn RefreshJournal>,
        runtime,
        Arc::new(|_, _, _, _| panic!("recovery invoked global discovery")),
    );
    assert!(recovered
        .recover_interrupted_publication(&data_root)
        .unwrap());
    let pending = status_value(&recovered, request_id);
    assert_eq!(pending["request_state"], "admission_pending");
    assert_eq!(pending["refresh_scope"], admitted_scope);
    assert!(recovered
        .prepare_next_pending_admission(&data_root)
        .unwrap());
    let admitted = status_value(&recovered, request_id);
    assert_eq!(admitted["request_state"], "queued");
    assert_eq!(admitted["refresh_scope"], admitted_scope);
    let run = recovered
        .run_next(&data_root)
        .expect("recovered exact provider request should execute");
    assert!(!run.failed, "{}", run.job);
    assert_eq!(run.scope, persisted_scope);
    assert_eq!(run.job["request_state"], "published");
}

#[test]
fn recovered_provider_scope_fails_when_a_persisted_route_disappears() {
    let (temp, data_root) = private_data_root();
    let runtime = scoped_runtime(temp.path());
    let sessions = temp.path().join("home/.codex/sessions");
    fs::create_dir_all(&sessions).unwrap();
    write_codex_session_fixture(&sessions);
    let journal = Arc::new(TestRefreshJournal::default());
    let request_id = "019fcaaa-0000-7000-8000-000000000507";
    let first = CoreRefreshEngine::with_admission_fence_for_test(
        Arc::clone(&journal) as Arc<dyn RefreshJournal>,
        Arc::clone(&runtime),
        Arc::new(|_, _, _, _| panic!("provider selection invoked global discovery")),
    );
    let admission = first
        .submit(
            &data_root,
            provider_submission(request_id, CaptureProvider::Codex),
        )
        .unwrap();
    release_pending_admission(&first, admission);
    assert!(first.prepare_next_pending_admission(&data_root).unwrap());
    let admitted_scope = status_value(&first, request_id)["refresh_scope"].clone();
    assert_eq!(admitted_scope["kind"], "exact");
    drop(first);

    // Keep the provider available under a different route so recovery must
    // compare exact authority, rather than merely fail because it found no
    // executable provider routes.
    fs::remove_dir_all(&sessions).unwrap();
    fs::write(
        temp.path().join("home/.codex/history.jsonl"),
        r#"{"session_id":"replacement-route","ts":1,"text":"later"}"#,
    )
    .unwrap();
    let recovered = CoreRefreshEngine::with_admission_fence_for_test(
        journal as Arc<dyn RefreshJournal>,
        runtime,
        Arc::new(|_, _, _, _| panic!("recovery invoked global discovery")),
    );
    assert!(recovered
        .recover_interrupted_publication(&data_root)
        .unwrap());
    assert!(recovered
        .prepare_next_pending_admission(&data_root)
        .unwrap());
    let failed = status_value(&recovered, request_id);
    assert_eq!(failed["request_state"], "failed");
    assert_eq!(failed["refresh_scope"], admitted_scope);
    assert!(failed["last_error"]
        .as_str()
        .is_some_and(|error| error.contains("missing persisted exact routes")));
    assert!(recovered.run_next(&data_root).is_none());
}

fn assert_retained_acknowledgement(response: &Value, request_id: &str) {
    assert_eq!(response["ok"], true);
    assert_eq!(response["request_id"], request_id);
    assert_eq!(response["request_state"], "admission_pending");
    assert_eq!(
        response["disconnect_policy"],
        "retain_after_durable_admission"
    );
    assert_retained_durability_fields(response);
}

fn assert_retained_durability_fields(response: &Value) {
    assert_eq!(
        response["admission_acknowledgement"],
        "retained_after_durability_error"
    );
    assert_eq!(
        response["admission_durability"],
        "replacement_visible_or_indeterminate"
    );
}

fn assert_reconfirmed_acknowledgement(response: &Value, request_id: &str) {
    assert_eq!(response["ok"], true);
    assert_eq!(response["request_id"], request_id);
    assert!(response.get("admission_acknowledgement").is_none());
    assert!(response.get("admission_durability").is_none());
}

#[derive(Debug)]
struct RetainingAdmissionJournal {
    job: Mutex<Option<Value>>,
    store_before_ack_calls: std::sync::atomic::AtomicUsize,
    message: &'static str,
}

impl RetainingAdmissionJournal {
    fn once(message: &'static str) -> Self {
        Self {
            job: Mutex::new(None),
            store_before_ack_calls: std::sync::atomic::AtomicUsize::new(0),
            message,
        }
    }
}

impl RefreshJournal for RetainingAdmissionJournal {
    fn load(&self, _data_root: &Path) -> Result<Option<Value>> {
        Ok(self.job.lock().unwrap().clone())
    }

    fn store(&self, _data_root: &Path, value: &Value) -> Result<()> {
        *self.job.lock().unwrap() = Some(value.clone());
        Ok(())
    }

    fn store_before_ack(&self, data_root: &Path, value: &Value) -> DurableAdmissionPersistence {
        if let Err(error) = self.store(data_root, value) {
            return DurableAdmissionPersistence::Failed(error);
        }
        let call = self
            .store_before_ack_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call == 0 {
            DurableAdmissionPersistence::Retained(anyhow!(self.message))
        } else {
            DurableAdmissionPersistence::Confirmed
        }
    }
}

#[test]
fn listener_ack_is_durable_before_admission_discovery_can_start() {
    let (_temp, data_root) = private_data_root();
    let journal = Arc::new(TestRefreshJournal::default());
    let fence_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_calls = Arc::clone(&fence_calls);
    let coordinator = CoreRefreshEngine::with_admission_fence_for_test(
        Arc::clone(&journal) as Arc<dyn RefreshJournal>,
        test_refresh_runtime(),
        Arc::new(move |_discovery, _journal, _data_root, _catalog| {
            observed_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(BTreeMap::new())
        }),
    );
    let request_id = "019fcaaa-0000-7000-8000-000000000294";

    let admission = coordinator
        .submit(&data_root, test_refresh_submission(request_id))
        .unwrap();
    let (response, response_barrier) = admission.into_parts();
    let response = response.schema_v1_fields();

    assert_eq!(response["request_id"], request_id);
    assert_eq!(response["request_state"], "admission_pending");
    assert_eq!(
        response["disconnect_policy"],
        "retain_after_durable_admission"
    );
    assert_eq!(fence_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    let durable = journal
        .load(&data_root)
        .unwrap()
        .expect("durable admission before acknowledgement");
    assert_eq!(durable["request_id"], request_id);
    assert_eq!(durable["request_state"], "admission_pending");

    assert!(!coordinator
        .prepare_next_pending_admission(&data_root)
        .unwrap());
    assert_eq!(fence_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    response_barrier
        .expect("pending admission response barrier")
        .release(&coordinator);
    assert!(coordinator
        .prepare_next_pending_admission(&data_root)
        .unwrap());
    assert_eq!(fence_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        status_value(&coordinator, request_id)["request_state"],
        "queued"
    );
}

#[test]
fn failed_durable_admission_rolls_back_the_reserved_request() {
    let (_temp, data_root) = private_data_root();
    let coordinator = CoreRefreshEngine::new(
        Arc::new(TestRefreshJournal::failing_before_ack()),
        test_refresh_runtime(),
    );
    let request_id = "019fcaaa-0000-7000-8000-000000000295";

    let error = coordinator
        .submit(&data_root, test_refresh_submission(request_id))
        .unwrap_err();

    assert!(format!("{error:#}").contains("persist durable source refresh admission"));
    assert!(coordinator.status(request_id).is_none());
    assert!(!coordinator.has_pending_request());
    assert!(!coordinator.has_pending_admission());
}

#[test]
fn post_replacement_chmod_error_retains_and_acknowledges_the_request() {
    let (_temp, data_root) = private_data_root();
    let journal = Arc::new(RetainingAdmissionJournal::once(
        "injected post-replacement chmod failure",
    ));
    let coordinator = CoreRefreshEngine::with_admission_fence_for_test(
        Arc::clone(&journal) as Arc<dyn RefreshJournal>,
        test_refresh_runtime(),
        Arc::new(|_discovery, _journal, _data_root, _catalog| Ok(BTreeMap::new())),
    );
    let request_id = "019fcaaa-0000-7000-8000-000000000298";

    let admission = coordinator
        .submit(&data_root, test_refresh_submission(request_id))
        .unwrap();
    let (response, response_barrier) = admission.into_parts();

    assert_retained_acknowledgement(response.schema_v1_fields(), request_id);
    assert!(coordinator.has_pending_admission());
    let retained = journal
        .load(&data_root)
        .unwrap()
        .expect("replacement-visible admission retains its durability marker");
    assert_eq!(retained["request_id"], request_id);
    assert_retained_durability_fields(&retained);
    let replay = coordinator
        .submit(&data_root, test_refresh_submission(request_id))
        .unwrap();
    let (replay, replay_barrier) = replay.into_parts();
    assert_reconfirmed_acknowledgement(replay.schema_v1_fields(), request_id);
    response_barrier
        .expect("retained admission response barrier")
        .release(&coordinator);
    replay_barrier
        .expect("replayed admission response barrier")
        .release(&coordinator);
    assert!(coordinator
        .prepare_next_pending_admission(&data_root)
        .unwrap());
    assert_eq!(
        status_value(&coordinator, request_id)["request_state"],
        "queued"
    );
}

#[test]
fn post_replacement_parent_sync_error_retains_and_acknowledges_the_request() {
    let (_temp, data_root) = private_data_root();
    let journal = Arc::new(RetainingAdmissionJournal::once(
        "injected durable admission parent sync failure",
    ));
    let coordinator = CoreRefreshEngine::with_admission_fence_for_test(
        Arc::clone(&journal) as Arc<dyn RefreshJournal>,
        test_refresh_runtime(),
        Arc::new(|_discovery, _journal, _data_root, _catalog| Ok(BTreeMap::new())),
    );
    let request_id = "019fcaaa-0000-7000-8000-000000000299";

    let admission = coordinator
        .submit(&data_root, test_refresh_submission(request_id))
        .unwrap();
    let (response, response_barrier) = admission.into_parts();

    assert_retained_acknowledgement(response.schema_v1_fields(), request_id);
    assert!(coordinator.has_pending_admission());
    let retained = journal
        .load(&data_root)
        .unwrap()
        .expect("parent-sync failure retains its durability marker");
    assert_eq!(retained["request_id"], request_id);
    assert_retained_durability_fields(&retained);
    let replay = coordinator
        .submit(&data_root, test_refresh_submission(request_id))
        .unwrap();
    let (replay, replay_barrier) = replay.into_parts();
    assert_reconfirmed_acknowledgement(replay.schema_v1_fields(), request_id);
    response_barrier
        .expect("retained admission response barrier")
        .release(&coordinator);
    replay_barrier
        .expect("replayed admission response barrier")
        .release(&coordinator);
    assert!(coordinator
        .prepare_next_pending_admission(&data_root)
        .unwrap());
    assert_eq!(
        status_value(&coordinator, request_id)["request_state"],
        "queued"
    );
}

#[test]
fn same_id_replay_preserves_persistently_indeterminate_admission_durability() {
    let (_temp, data_root) = private_data_root();
    let journal = Arc::new(TestRefreshJournal::retaining_after_ack_write(
        "injected persistent post-replacement durability failure",
    ));
    let coordinator = CoreRefreshEngine::with_admission_fence_for_test(
        Arc::clone(&journal) as Arc<dyn RefreshJournal>,
        test_refresh_runtime(),
        Arc::new(|_discovery, _journal, _data_root, _catalog| Ok(BTreeMap::new())),
    );
    let request_id = "019fcaaa-0000-7000-8000-0000000002a2";

    let first = coordinator
        .submit(&data_root, test_refresh_submission(request_id))
        .unwrap();
    let replay = coordinator
        .submit(&data_root, test_refresh_submission(request_id))
        .unwrap();

    assert_retained_acknowledgement(first.status().schema_v1_fields(), request_id);
    assert_retained_acknowledgement(replay.status().schema_v1_fields(), request_id);
    let durable = journal
        .load(&data_root)
        .unwrap()
        .expect("replacement-visible durable admission");
    assert_eq!(durable["request_id"], request_id);
    assert_eq!(
        durable["admission_acknowledgement"],
        "retained_after_durability_error"
    );
    assert_eq!(
        durable["admission_durability"],
        "replacement_visible_or_indeterminate"
    );
}

#[test]
fn stable_request_id_replay_requires_the_exact_same_payload() {
    let (_temp, data_root) = private_data_root();
    let coordinator = test_refresh_engine();
    let request_id = "019fcaaa-0000-7000-8000-000000000296";

    let first = coordinator
        .submit(&data_root, test_refresh_submission(request_id))
        .unwrap();
    let replay = coordinator
        .submit(&data_root, test_refresh_submission(request_id))
        .unwrap();
    assert_eq!(replay.status(), first.status());

    let changed = RefreshRequest::automatic(request_id.to_owned(), RefreshRequestTrigger::Search);
    let conflict = coordinator.submit(&data_root, changed).unwrap();
    let conflict = conflict.status().schema_v1_fields();
    assert_eq!(conflict["ok"], false);
    assert_eq!(conflict["request_id"], request_id);
    assert_eq!(conflict["request_state"], "request_conflict");
    assert_eq!(conflict["error_code"], "request_id_conflict");
    assert_eq!(conflict["retryable"], false);
}

#[test]
fn stable_request_id_distinguishes_exact_source_from_selected_all() {
    let (_temp, data_root) = private_data_root();
    let coordinator = test_refresh_engine();
    let request_id = "019fcaaa-0000-7000-8000-000000000506";
    let authority = crate::explicit_source_catalog_authority_for_test(1);
    let exact = || {
        RefreshRequest::selected_import(
            request_id.to_owned(),
            RefreshSelection::ExactSource(authority.clone()),
        )
    };

    let legacy = coordinator
        .submit(
            &data_root,
            RefreshRequest::selected_import(request_id.to_owned(), RefreshSelection::All),
        )
        .unwrap();
    release_pending_admission(&coordinator, legacy);
    let conflict = coordinator.submit(&data_root, exact()).unwrap();
    let conflict = conflict.status().schema_v1_fields();
    assert_eq!(conflict["request_state"], "request_conflict");
    assert_eq!(conflict["error_code"], "request_id_conflict");
}

#[test]
fn stable_request_id_replays_the_same_provider_and_conflicts_on_provider_change() {
    let (_temp, data_root) = private_data_root();
    let coordinator = test_refresh_engine();
    let request_id = "019fcaaa-0000-7000-8000-000000000414";
    let submission = |provider| {
        RefreshRequest::selected_import(request_id.to_owned(), RefreshSelection::Provider(provider))
    };

    let first = coordinator
        .submit(&data_root, submission(CaptureProvider::Codex))
        .unwrap();
    let replay = coordinator
        .submit(&data_root, submission(CaptureProvider::Codex))
        .unwrap();
    assert_eq!(replay.status(), first.status());

    let conflict = coordinator
        .submit(&data_root, submission(CaptureProvider::Claude))
        .unwrap();
    let conflict = conflict.status().schema_v1_fields();
    assert_eq!(conflict["request_state"], "request_conflict");
    assert_eq!(conflict["error_code"], "request_id_conflict");
}

#[test]
fn provider_selection_does_not_coalesce_with_all_automatic() {
    let (_temp, data_root) = private_data_root();
    let coordinator = test_refresh_engine();
    let all_request_id = "019fcaaa-0000-7000-8000-000000000415";
    let provider_request_id = "019fcaaa-0000-7000-8000-000000000416";
    let all = coordinator
        .submit(
            &data_root,
            RefreshRequest::automatic(all_request_id.to_owned(), RefreshRequestTrigger::Search),
        )
        .unwrap();
    let provider = coordinator
        .submit(
            &data_root,
            RefreshRequest::selected_import(
                provider_request_id.to_owned(),
                RefreshSelection::Provider(CaptureProvider::Codex),
            ),
        )
        .unwrap();

    assert_eq!(all.status()["request_id"], all_request_id);
    assert_eq!(provider.status()["request_id"], provider_request_id);
    assert_eq!(
        status_value(&coordinator, all_request_id)["refresh_intent"],
        json!({ "kind": "automatic_maintenance" })
    );
    assert_eq!(
        status_value(&coordinator, provider_request_id)["refresh_intent"],
        json!({
            "kind": "selected_import",
            "selection": { "kind": "provider", "provider": "codex" },
        })
    );
}

#[test]
fn durable_recovery_preserves_intent_separately_from_physical_scope() {
    let routes = BTreeSet::from([
        SourceRouteIdentity::from_sha256("41".repeat(32)).unwrap(),
        SourceRouteIdentity::from_sha256("42".repeat(32)).unwrap(),
    ]);
    let scope = SourceBackedRefreshScope::Exact(routes);
    let attempt = new_refresh_attempt(
        None,
        SourceRefreshRuntimeMetadata::default(),
        RefreshIntent::SelectedImport(RefreshSelection::Provider(CaptureProvider::Codex)),
        scope.clone(),
    );
    let job = attempt.job_json();

    assert_eq!(
        job["refresh_intent"],
        json!({
            "kind": "selected_import",
            "selection": { "kind": "provider", "provider": "codex" }
        })
    );
    assert!(job.get("refresh_selector").is_none());
    assert!(job.get("fresh_after_admitted_snapshot").is_none());
    assert!(job.get("requested_explicit_source_catalog").is_none());
    let recovered = recover_queued_root(&job, None).unwrap();
    assert_eq!(recovered.intent, attempt.intent);
    assert_eq!(recovered.refresh_scope, scope);

    let mut legacy = job.clone();
    legacy.as_object_mut().unwrap().remove("refresh_intent");
    legacy["refresh_selector"] = json!({ "kind": "automatic_provider", "provider": "codex" });
    let recovered_legacy = recover_queued_root(&legacy, None).unwrap();
    assert_eq!(
        recovered_legacy.intent,
        RefreshIntent::SelectedImport(RefreshSelection::Provider(CaptureProvider::Codex))
    );
    assert_eq!(recovered_legacy.refresh_scope, attempt.refresh_scope);

    let legacy_authority = crate::explicit_source_catalog_authority_for_test(1);
    let mut legacy_explicit = new_refresh_attempt(
        None,
        SourceRefreshRuntimeMetadata {
            operation: SourceBackedRefreshOperation::Import,
            daemon_mode: "full".to_owned(),
            trigger: "import",
            trigger_provenance: "explicit_source_catalog",
        },
        RefreshIntent::SelectedImport(RefreshSelection::ExactSource(legacy_authority.clone())),
        SourceBackedRefreshScope::All,
    )
    .job_json();
    legacy_explicit
        .as_object_mut()
        .unwrap()
        .remove("refresh_intent");
    legacy_explicit["requested_explicit_source_catalog"] = legacy_authority.to_json();
    let recovered_explicit = recover_queued_root(&legacy_explicit, None).unwrap();
    assert_eq!(
        recovered_explicit.intent,
        RefreshIntent::SelectedImport(RefreshSelection::ExactSource(
            crate::explicit_source_catalog_authority_for_test(1),
        ))
    );

    let mut malformed = job.clone();
    malformed["refresh_intent"] = json!({
        "kind": "selected_import",
        "selection": {
            "kind": "provider",
            "provider": "codex",
            "unexpected": true
        },
        "unexpected": true,
    });
    assert!(recover_queued_root(&malformed, None).is_err());

    let mut unknown_provider = job.clone();
    unknown_provider["refresh_intent"] = json!({
        "kind": "selected_import",
        "selection": { "kind": "provider", "provider": "unknown" },
    });
    assert!(recover_queued_root(&unknown_provider, None).is_err());

    let mut missing_authority = job;
    missing_authority["refresh_intent"] = json!({
        "kind": "selected_import",
        "selection": { "kind": "exact_source" },
    });
    assert!(recover_queued_root(&missing_authority, None).is_err());
}

#[test]
fn stable_request_id_replay_treats_trigger_as_request_identity() {
    let (_temp, data_root) = private_data_root();
    let coordinator = test_refresh_engine();
    let request_id = "019fcaaa-0000-7000-8000-000000000411";

    let search = test_refresh_submission(request_id).with_trigger(RefreshRequestTrigger::Search);
    coordinator.submit(&data_root, search).unwrap();
    let setup = test_refresh_submission(request_id).with_trigger(RefreshRequestTrigger::Setup);
    let conflict = coordinator.submit(&data_root, setup).unwrap();
    let conflict = conflict.status().schema_v1_fields();

    assert_eq!(conflict["request_state"], "request_conflict");
    assert_eq!(conflict["error_code"], "request_id_conflict");
}

#[test]
fn restart_recovers_durable_setup_admission_metadata() {
    let (_temp, data_root) = private_data_root();
    let request_id = "019fcaaa-0000-7000-8000-000000000412";
    let journal = Arc::new(TestRefreshJournal::default());
    let first = CoreRefreshEngine::new(
        Arc::clone(&journal) as Arc<dyn RefreshJournal>,
        test_refresh_runtime(),
    );
    let admission = first
        .submit(
            &data_root,
            test_refresh_submission(request_id).with_trigger(RefreshRequestTrigger::Setup),
        )
        .unwrap();
    assert_eq!(
        admission.status().schema_v1_fields()["request_state"],
        "admission_pending"
    );
    drop(first);

    let recovered =
        CoreRefreshEngine::new(journal as Arc<dyn RefreshJournal>, test_refresh_runtime());
    assert!(recovered
        .recover_interrupted_publication(&data_root)
        .unwrap());
    let status = status_value(&recovered, request_id);
    assert_eq!(status["trigger"], "setup");
    assert_eq!(status["trigger_provenance"], "setup_command");
}

#[test]
fn restart_recovers_terminal_setup_metadata() {
    let (_temp, data_root) = private_data_root();
    let request_id = "019fcaaa-0000-7000-8000-000000000413";
    let journal = Arc::new(TestRefreshJournal::default());
    let route = SourceRouteIdentity::from_sha256("41".repeat(32)).unwrap();
    let first = CoreRefreshEngine::with_admission_fence_for_test(
        Arc::clone(&journal) as Arc<dyn RefreshJournal>,
        test_refresh_runtime(),
        Arc::new(move |_, _, _, _| Ok(BTreeMap::from([(route.clone(), Some("42".repeat(32)))]))),
    );
    let submission = RefreshRequest::automatic(request_id.to_owned(), RefreshRequestTrigger::Setup);
    let admission = first.submit(&data_root, submission).unwrap();
    release_pending_admission(&first, admission);
    assert!(first.prepare_next_pending_admission(&data_root).unwrap());
    let failed = first
        .run_next_with(
            |_, _| {
                Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::InvalidSource,
                    "bounded setup refresh failure",
                )
                .into())
            },
            || Ok(None),
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("failed terminal setup run");
    assert!(failed.failed);
    journal.store(&data_root, &failed.job).unwrap();
    let durable = journal
        .load(&data_root)
        .unwrap()
        .expect("durable failed setup");
    assert_eq!(durable["request_state"], "failed");
    assert_eq!(durable["trigger"], "setup");
    assert_eq!(durable["trigger_provenance"], "setup_command");
    drop(first);

    let recovered =
        CoreRefreshEngine::new(journal as Arc<dyn RefreshJournal>, test_refresh_runtime());
    let _recovered_work = recovered
        .recover_interrupted_publication(&data_root)
        .unwrap();
    let status = status_value(&recovered, request_id);
    assert_eq!(status["request_state"], "failed");
    assert_eq!(status["trigger"], "setup");
    assert_eq!(status["trigger_provenance"], "setup_command");
}

#[test]
fn restart_recovers_and_resumes_a_durable_pending_admission() {
    let (_temp, data_root) = private_data_root();
    let request_id = "019fcaaa-0000-7000-8000-000000000297";
    let journal = Arc::new(TestRefreshJournal::default());
    let first = CoreRefreshEngine::new(
        Arc::clone(&journal) as Arc<dyn RefreshJournal>,
        test_refresh_runtime(),
    );
    let admission = first
        .submit(&data_root, test_refresh_submission(request_id))
        .unwrap();
    assert_eq!(
        admission.status().schema_v1_fields()["request_state"],
        "admission_pending"
    );
    drop(first);

    let recovered = CoreRefreshEngine::with_admission_fence_for_test(
        journal as Arc<dyn RefreshJournal>,
        test_refresh_runtime(),
        Arc::new(|_discovery, _journal, _data_root, _catalog| Ok(BTreeMap::new())),
    );
    assert!(recovered
        .recover_interrupted_publication(&data_root)
        .unwrap());
    assert_eq!(
        status_value(&recovered, request_id)["request_state"],
        "admission_pending"
    );
    assert!(recovered
        .prepare_next_pending_admission(&data_root)
        .unwrap());
    assert_eq!(
        status_value(&recovered, request_id)["request_state"],
        "queued"
    );
}

#[test]
fn crash_restart_preserves_a_logical_successors_non_null_generation_baseline() {
    let generation_zero = "10".repeat(32);
    let generation_one = "21".repeat(32);
    let mut predecessor = new_refresh_attempt(
        Some(generation_zero.clone()),
        SourceRefreshRuntimeMetadata::periodic(),
        RefreshIntent::AutomaticMaintenance,
        SourceBackedRefreshScope::All,
    );
    predecessor.request_id = "019fcaaa-0000-7000-8000-0000000002a0".to_owned();
    predecessor.state = SourceBackedRefreshState::Published;
    predecessor.published_generation = Some(generation_one.clone());
    let mut successor = new_refresh_attempt(
        Some(generation_zero.clone()),
        SourceRefreshRuntimeMetadata::default(),
        RefreshIntent::SelectedImport(RefreshSelection::All),
        SourceBackedRefreshScope::All,
    );
    successor.request_id = "019fcaaa-0000-7000-8000-0000000002a1".to_owned();
    let mut interrupted = predecessor.job_json();
    interrupted["queued_successors"] = Value::Array(vec![successor.job_json()]);

    let mut recovered = recover_queued_successors(&interrupted)
        .expect("recover successor after predecessor pointer publication")
        .pop()
        .expect("persisted logical successor");
    recovered.state = SourceBackedRefreshState::Published;
    recovered.published_generation = Some(generation_one.clone());
    recovered.receipt = Some(SourceBackedRefreshReceipt {
        zero_source_authority: Vec::new(),
        previous_generation: recovered.previous_generation.clone(),
        published_generation: generation_one,
        generation_changed: true,
        published_explicit_source_catalog: None,
        current: SourceBackedRefreshCurrent::default(),
        route_results: Vec::new(),
        catalog_route_bindings: Vec::new(),
    });

    let terminal = recovered.to_json();
    assert_eq!(terminal["previous_generation"], generation_zero);
    assert_eq!(terminal["generation_changed"], true);
    assert_eq!(
        terminal["receipt"]["previous_generation"],
        terminal["previous_generation"]
    );
}
