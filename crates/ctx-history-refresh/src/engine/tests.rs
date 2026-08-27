use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc, Barrier,
    },
};

use super::*;
use crate::{
    orchestration::refresh_all_provider_sources,
    publication::observation::install_after_capture_scan_before_metadata_hook_for_test,
};
use ctx_history_capture::{
    provider_source_for_path, DiscoveryPlatform, DiscoveryPlatformDirs, SourceBackedFailedRoute,
};
use ctx_history_capture_model::{
    CoreRecordBatchProgress, ProviderCatalogSupport, ProviderImportSupport, ProviderSource,
    ProviderSourceKind,
};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentScope, CertifiedSource, CoreRecord,
    EventIdentityInput, NativeItemKey, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::{
    CompiledSearchFilter, EventSearchCandidate, EventSearchFilters, LexicalExecution, LexicalMode,
    SourceRouteIdentity,
};
use ctx_history_refresh_execution::{
    source_backed_requested_route_observations, verify_generation_query_readiness,
    GenerationQueryReadiness,
};

fn complete_lexical_candidates(
    index: &VerifiedIndex,
    natural_text: &str,
    limit: usize,
) -> Result<Vec<EventSearchCandidate>> {
    let alternatives = [natural_text];
    let filter = CompiledSearchFilter::compile(EventSearchFilters::default())?;
    let observed = index
        .execute_lexical(LexicalExecution::new(
            LexicalMode::Search(&alternatives),
            &filter,
            limit,
        ))
        .map_err(|failure| failure.error)?;
    assert!(
        observed.batch.complete,
        "lexical test helper requires a complete batch: {:?}",
        observed.batch.exhaustion
    );
    Ok(observed
        .batch
        .candidates
        .into_iter()
        .map(Into::into)
        .collect())
}

#[path = "tests/harness.rs"]
mod harness;
use harness::{
    pin_active_verified_generation, pin_published_generation, CoreRefreshEngine,
    SOURCE_REFRESH_REQUEST_OP,
};

#[test]
fn read_model_source_count_uses_request_routes_not_global_or_diagnostic_counts() {
    let mut attempt = new_refresh_attempt(
        None,
        SourceRefreshRuntimeMetadata::default(),
        RefreshIntent::AutomaticMaintenance,
        SourceBackedRefreshScope::All,
    );
    attempt.state = SourceBackedRefreshState::Published;

    for (
        name,
        scanned_routes,
        unsupported_routes,
        route_inventory,
        request_sources,
        global_sources,
    ) in [
        ("unsupported only", 0, 1, 1, 0, 0),
        ("mixed executable and unsupported", 1, 1, 2, 1, 1),
        ("covered executable route", 0, 3, 3, 1, 1),
        ("failed carried source remains global only", 1, 3, 4, 0, 1),
        (
            "global publication contains unrelated sources",
            38,
            37,
            75,
            1,
            2,
        ),
    ] {
        attempt.scanned_routes = Some(scanned_routes);
        attempt.unsupported_routes = Some(unsupported_routes);
        attempt.request_source_count = Some(request_sources);
        attempt.certified_source_count = Some(global_sources);
        attempt.progress.total_sources = route_inventory;
        attempt.progress_total_sources_known = true;
        let job = attempt.job_json();
        assert_eq!(job["source_count"], request_sources, "{name}");
        assert_eq!(job["scanned_routes"], scanned_routes, "{name}");
        assert_eq!(job["unsupported_routes"], unsupported_routes, "{name}");
        assert_eq!(job["certified_source_count"], global_sources, "{name}");
        assert_eq!(job["progress"]["total_sources"], route_inventory, "{name}");
    }
}

#[test]
fn active_status_overlays_worker_facts_and_snapshots_them_on_failure() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let (published, published_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let executor_release = Arc::clone(&release_rx);
    let executor = Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
        let page = CoreRecordBatchProgress {
            // Duplicate IDs model independently prepared pages from workers.
            session_ids: vec![[9; 32], [9; 32]],
            messages: 4,
            tool_calls: 2,
        };
        execution
            .attempt_history_progress
            .publish_parallel_page(768, &page);
        published.send(()).unwrap();
        executor_release.lock().unwrap().recv().unwrap();
        Err(anyhow!("injected blocked add_prepared failure"))
    });
    let coordinator = Arc::new(CoreRefreshEngine::with_executor(executor));
    let queued = coordinator.enqueue_periodic(&data_root).unwrap();
    let request_id = request_id(&queued);
    std::thread::scope(|scope| {
        let runner = Arc::clone(&coordinator);
        let root = data_root.clone();
        let run = scope.spawn(move || runner.run_next(&root).unwrap());
        published_rx.recv().unwrap();

        let live = coordinator.status(&request_id).unwrap();
        assert_eq!(live["request_state"], "running");
        assert_eq!(live["progress"]["processed_sessions"], 1);
        assert_eq!(live["progress"]["processed_messages"], 4);
        assert_eq!(live["progress"]["processed_tool_calls"], 2);
        assert_eq!(live["progress"]["processed_bytes"], 768);
        assert_eq!(live["progress"]["completed_records"], Value::Null);

        release.send(()).unwrap();
        assert!(run.join().unwrap().failed);
    });
    let terminal = coordinator.status(&request_id).unwrap();
    assert_eq!(terminal["request_state"], "failed");
    assert_eq!(terminal["progress"]["processed_sessions"], 1);
    assert_eq!(terminal["progress"]["processed_messages"], 4);
    assert_eq!(terminal["progress"]["processed_tool_calls"], 2);
    assert_eq!(terminal["progress"]["processed_bytes"], 768);
}

#[path = "tests/observation_fence.rs"]
mod observation_fence;

#[path = "tests/pending_missing.rs"]
mod pending_missing;

#[path = "tests/retained_generation.rs"]
mod retained_generation;

struct TestExecutor {
    calls: Arc<AtomicUsize>,
    generation_id: String,
    failure: Option<String>,
}

impl SourceBackedRefreshExecutor for TestExecutor {
    fn refresh(
        &self,
        execution: SourceBackedRefreshExecution<'_>,
    ) -> Result<SourceBackedRefreshPublication> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            execution.index_root,
            source_backed_index_root(execution.data_root)
        );
        assert!(!execution.request_id.is_empty());
        if let Some(error) = self.failure.as_deref() {
            return Err(anyhow!("{error}"));
        }
        execution.report_progress(
            "refreshing",
            0,
            1,
            Some("provider-neutral".to_owned()),
            Some(7),
            Some(4_096),
        )?;
        execution.report_progress("verifying", 1, 1, None, None, None)?;
        Ok(test_publication(self.generation_id.clone()))
    }
}

fn test_publication(generation_id: impl Into<String>) -> SourceBackedRefreshPublication {
    SourceBackedRefreshPublication {
        route_results: Vec::new(),
        zero_source_authority: Vec::new(),
        catalog_route_bindings: Vec::new(),
        verified_index: None,
        generation_id: generation_id.into(),
        published_explicit_source_catalog: None,
        unsupported_routes: 0,
        certified_source_count: 1,
        certified_source_bytes: 128,
        current: SourceBackedRefreshCurrent {
            source_count: 1,
            indexed_documents: 2,
            complete_records: 3,
            retained_records: 2,
            rejected_records: 1,
            certified_source_bytes: 128,
            sources_with_rejections: 1,
            ..SourceBackedRefreshCurrent::default()
        },
        timings: SourceBackedRefreshTimings {
            discovery_us: 11,
            scan_stage_us: 22,
            commit_us: 33,
        },
    }
}

#[derive(Debug)]
struct RecordingRefreshRuntime {
    events: Arc<Mutex<Vec<&'static str>>>,
}

impl RefreshRuntime for RecordingRefreshRuntime {
    fn metadata(&self, _data_root: &Path, operation: RefreshOperation) -> RefreshRuntimeMetadata {
        RefreshRuntimeMetadata {
            operation,
            ..RefreshRuntimeMetadata::default()
        }
    }

    fn discovery_context(&self, data_root: &Path) -> Result<DiscoveryContext> {
        Ok(DiscoveryContext::from_process(data_root.join("test-home")))
    }

    fn refresh_execution_finished(&self) {
        self.events.lock().unwrap().push("execution-finished");
    }
}

struct RecordingExecutionDrop(Arc<Mutex<Vec<&'static str>>>);

impl Drop for RecordingExecutionDrop {
    fn drop(&mut self) {
        self.0.lock().unwrap().push("execution-locals-dropped");
    }
}

#[test]
fn runtime_hook_follows_execution_drop_and_precedes_terminal_status() {
    fn run(success: bool) -> Vec<&'static str> {
        let events = Arc::new(Mutex::new(Vec::new()));
        let runtime = Arc::new(RecordingRefreshRuntime {
            events: Arc::clone(&events),
        });
        let coordinator = CoreRefreshEngine(super::CoreRefreshEngine::with_journal_for_test(
            Arc::new(TestRefreshJournal::default()),
            runtime,
            Arc::new(TestExecutor {
                calls: Arc::new(AtomicUsize::new(0)),
                generation_id: "unused".to_owned(),
                failure: None,
            }),
        ));
        coordinator.enqueue(Some("previous".to_owned()));

        let execute_events = Arc::clone(&events);
        let probe_events = Arc::clone(&events);
        let terminal_events = Arc::clone(&events);
        let failure_events = Arc::clone(&events);
        let run = coordinator
            .run_next_with(
                move |_, _| {
                    execute_events.lock().unwrap().push("execute");
                    let _drop = RecordingExecutionDrop(Arc::clone(&execute_events));
                    if success {
                        Ok(test_publication("published"))
                    } else {
                        Err(anyhow!("injected execution failure"))
                    }
                },
                move || {
                    probe_events.lock().unwrap().push("probe");
                    Ok(Some(
                        if success { "published" } else { "previous" }.to_owned(),
                    ))
                },
                move |_| {
                    terminal_events.lock().unwrap().push("terminal-status");
                    Ok(())
                },
                move |_| {
                    failure_events.lock().unwrap().push("record-failure");
                    Ok(())
                },
            )
            .expect("queued refresh");
        assert_eq!(run.failed, !success);
        drop(coordinator);

        Arc::try_unwrap(events).unwrap().into_inner().unwrap()
    }

    assert_eq!(
        run(true),
        [
            "execute",
            "execution-locals-dropped",
            "execution-finished",
            "probe",
            "terminal-status",
        ]
    );
    assert_eq!(
        run(false),
        [
            "execute",
            "execution-locals-dropped",
            "execution-finished",
            "probe",
            "record-failure",
            "terminal-status",
        ]
    );
}

#[test]
fn pressure_fence_only_advances_global_uncertainty_authority() {
    let coordinator = CoreRefreshEngine::new();
    let routes = (0x20..0x40).map(route_identity).collect::<BTreeSet<_>>();
    let retained = routes.iter().next().unwrap().clone();
    coordinator.initialize_watch_route_authority(routes);
    coordinator.record_watch_routes(
        [(retained.clone(), EventWatermark::new(4, 1))],
        ledger_now_ms(),
    );

    coordinator.fence_watch_uncertainty(EventWatermark::new(4, 7));
    coordinator.fence_watch_uncertainty(EventWatermark::new(4, 5));

    assert_eq!(
        coordinator.watch_uncertainty_watermark(),
        Some(EventWatermark::new(4, 7))
    );
    assert_eq!(
        coordinator.scheduled_route_ids_for_test(),
        BTreeSet::from([retained]),
        "callback fencing must not enumerate or seed the catalog"
    );
}

#[test]
fn uncertainty_between_preterminal_binding_and_state_transition_keeps_wait_active() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let coordinator = Arc::new(CoreRefreshEngine::new());
    let queued = coordinator.enqueue(Some("previous".to_owned()));
    let request_id = request_id(&queued);
    let preterminal = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let runner_preterminal = Arc::clone(&preterminal);
    let runner_release = Arc::clone(&release);
    let runner = Arc::clone(&coordinator);
    let run = std::thread::spawn(move || {
        runner
            .run_next_with_terminal_success(
                |_, _| Ok(test_publication("stale")),
                || Ok(Some("stale".to_owned())),
                move |receipt| {
                    runner_preterminal.wait();
                    runner_release.wait();
                    Ok(CoreRefreshTerminalSuccess::state_only(receipt))
                },
                |_| panic!("fenced refresh must not persist terminal success"),
                |_| Ok(()),
            )
            .expect("active wait run")
    });

    preterminal.wait();
    let boundary = EventWatermark::new(8, 13);
    coordinator.fence_watch_uncertainty(boundary);
    assert_eq!(
        coordinator.status(&request_id).unwrap()["request_state"],
        "running"
    );
    release.wait();

    let fenced = run.join().unwrap();
    assert_eq!(fenced.job["request_state"], "running");
    assert_eq!(fenced.job["progress"]["phase"], "watch_recovery");
    assert!(coordinator
        .complete_watch_uncertainty_recovery(
            &data_root,
            SourceBackedWatchCatalog::default(),
            boundary,
            ledger_now_ms(),
        )
        .unwrap());
    let pending = coordinator.status(&request_id).unwrap();
    assert_eq!(pending["request_state"], "admission_pending");
    assert_eq!(pending["reconciliation_demand"], "exhaustive");
    assert!(coordinator
        .prepare_next_pending_admission(&data_root)
        .unwrap());

    let recovered = coordinator
        .run_next_with(
            |_, _| Ok(test_publication("recovered")),
            || Ok(Some("recovered".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("exhaustive successor");
    assert_eq!(recovered.job["request_id"], request_id);
    assert_eq!(recovered.job["request_state"], "published");
}

fn empty_test_publication(generation_id: impl Into<String>) -> SourceBackedRefreshPublication {
    let mut publication = test_publication(generation_id);
    publication.certified_source_count = 0;
    publication.certified_source_bytes = 0;
    publication.current = SourceBackedRefreshCurrent::default();
    publication
}

fn add_complete_empty_authority(
    publication: &mut SourceBackedRefreshPublication,
    route: SourceRouteIdentity,
) {
    publication.route_results = vec![SourceBackedRefreshRouteResult::succeeded(
        route.as_str().to_owned(),
        true,
    )];
    publication.zero_source_authority = vec![SourceBackedZeroSourceAuthority {
        generation_id: publication.generation_id.clone(),
        route_identity: route,
        kind: SourceBackedZeroSourceAuthorityKind::CompleteEmptyInventory,
    }];
}

fn request_id(response: &Value) -> String {
    response
        .get("request_id")
        .and_then(Value::as_str)
        .expect("request ID")
        .to_owned()
}

fn route_identity(byte: u8) -> SourceRouteIdentity {
    SourceRouteIdentity::from_sha256(format!("{byte:02x}").repeat(32)).unwrap()
}

fn ledger_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

struct RunningRefreshGate {
    started: mpsc::Receiver<()>,
    release: Option<mpsc::SyncSender<()>>,
}

impl RunningRefreshGate {
    fn new() -> (Self, mpsc::SyncSender<()>, mpsc::Receiver<()>) {
        let (started_tx, started_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        (
            Self {
                started: started_rx,
                release: Some(release_tx),
            },
            started_tx,
            release_rx,
        )
    }

    fn wait_until_started(&self) {
        self.started
            .recv_timeout(StdDuration::from_secs(5))
            .expect("refresh runner entered executor");
    }

    fn release(mut self) {
        self.release
            .take()
            .expect("refresh release sender")
            .send(())
            .expect("release refresh runner");
    }
}

fn test_catalog_authority(revision: u64, digest_byte: u8) -> ExplicitSourceCatalogAuthority {
    let _ = digest_byte;
    crate::explicit_source_catalog_authority_for_test(revision)
}

fn test_exact_catalog_authority(
    data_root: &Path,
    source_root: &Path,
) -> ExplicitSourceCatalogAuthority {
    fs::create_dir_all(source_root).expect("create exact-source fixture root");
    crate::upsert_explicit_source(
        data_root,
        &provider_source_for_path(CaptureProvider::Codex, source_root.to_path_buf()),
    )
    .expect("register exact-source fixture")
    .authority
}

fn physically_selected_routes(
    execution: &SourceBackedRefreshExecution<'_>,
    current_routes: &BTreeSet<SourceRouteIdentity>,
) -> BTreeSet<SourceRouteIdentity> {
    match &execution.admitted_refresh().publication_scope() {
        SourceBackedRefreshScope::All => current_routes.clone(),
        SourceBackedRefreshScope::Exact(routes) => routes.clone(),
    }
}

fn publish_selected_routes(
    execution: &SourceBackedRefreshExecution<'_>,
    selected: &BTreeSet<SourceRouteIdentity>,
    failed_route: Option<(&SourceRouteIdentity, &'static str)>,
) -> Result<SourceBackedRefreshPublication> {
    let retain_rejection_fixture = open_verified_index(execution.index_root)
        .is_ok_and(|index| !index.manifest().sources.is_empty());
    let mut writer =
        ctx_history_index::GenerationWriter::open(execution.index_root, WriterOptions::default())?
            .into_writer()
            .map_err(crate::committed_generation_recovery_error)?;
    if retain_rejection_fixture {
        let source = publication_pin_source_with_anchor(0x93);
        writer.begin_source(source.clone())?;
        writer.add_core_record(publication_pin_record(&source))?;
        writer.certify_source(publication_rejection_certificate(&source))?;
    }
    let commit = writer.commit(|_| true)?;
    let mut publication = empty_test_publication(commit.generation_id.clone());
    publication.current = SourceBackedRefreshCurrent::from_sources(&commit.manifest().sources, 0)?;
    publication.certified_source_count = publication.current.source_count;
    publication.certified_source_bytes = publication.current.certified_source_bytes;
    publication.published_explicit_source_catalog = execution.explicit_source_catalog.cloned();
    publication.route_results = selected
        .iter()
        .map(|route| SourceBackedRefreshRouteResult::succeeded(route.as_str().to_owned(), true))
        .collect();
    if let Some((route, class)) = failed_route {
        let result = publication
            .route_results
            .iter_mut()
            .find(|result| result.route_identity == route.as_str())
            .expect("failed selected route");
        *result = SourceBackedRefreshRouteResult::failed(
            route.as_str().to_owned(),
            class.to_owned(),
            true,
        );
        result.source_failures = vec![SourceBackedRefreshSourceFailure {
            route_identity: route.as_str().to_owned(),
            source_identity: "cd".repeat(32),
            provider: "fixture".to_owned(),
            class: class.to_owned(),
            carried_forward: true,
            source_selector: "fixture source".to_owned(),
            detail: "fixture failure".to_owned(),
        }];
    }
    Ok(publication)
}

fn publication_rejection_certificate(source: &SourceKey) -> CertifiedSource {
    let observation = SourceObservation::new(source.clone(), "regular-file-v1", vec![1]).unwrap();
    CertifiedSource::certify(
        observation.clone(),
        observation,
        "publication-pin-test-v1",
        [0x94; 32],
        ScannedSourceCounts {
            complete_records: 2,
            retained_records: 1,
            rejected_records: 1,
            indexed_documents: 1,
            certified_bytes: 128,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap()
}

fn publication_pin_source() -> SourceKey {
    publication_pin_source_with_anchor(0x91)
}

fn publication_pin_source_with_anchor(anchor: u8) -> SourceKey {
    SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        SourceAnchor::CatalogLineage([anchor; 32]),
    )
    .unwrap()
}

fn publish_pin_source(index_root: &Path, source: SourceKey) -> String {
    let mut writer =
        ctx_history_index::GenerationWriter::open(index_root, WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(publication_pin_record(&source))
        .unwrap();
    writer
        .certify_source(publication_pin_certificate(&source))
        .unwrap();
    writer.commit(|_| true).unwrap().generation_id
}

fn publication_pin_record(source: &SourceKey) -> CoreRecord {
    let native_session = TypedKey::utf8("publication-pin-session").unwrap();
    let session_key = NativeSessionKey::native_id("session", native_session).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let native_item =
        NativeItemKey::native_id("message", TypedKey::utf8("publication-pin-event").unwrap())
            .unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item,
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        0,
        "message",
        "publication-pin-test-v1",
        "exact publication pin fixture",
    )
    .unwrap();
    record.provider_session_id = Some("publication-pin-session".to_owned());
    record.native_event_id = Some(TypedKey::U64(0));
    record.role = Some("user".to_owned());
    record.agent_scope = Some(AgentScope::Primary);
    record.validate_contract().unwrap();
    record
}

fn publication_pin_certificate(source: &SourceKey) -> CertifiedSource {
    let observation = SourceObservation::new(source.clone(), "regular-file-v1", vec![1]).unwrap();
    CertifiedSource::certify(
        observation.clone(),
        observation,
        "publication-pin-test-v1",
        [0x92; 32],
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 1,
            indexed_documents: 1,
            certified_bytes: 128,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap()
}

fn publish_pin_fixture(
    execution: &SourceBackedRefreshExecution<'_>,
    alternate_source: bool,
) -> Result<SourceBackedRefreshPublication> {
    let mut writer =
        ctx_history_index::GenerationWriter::open(execution.index_root, WriterOptions::default())?
            .into_writer()
            .map_err(crate::committed_generation_recovery_error)?;
    let source = publication_pin_source();
    writer.begin_source(source.clone())?;
    let mut record = publication_pin_record(&source);
    if alternate_source {
        record.content.structured_content = Some(json!({"fixture_revision": 2}));
        record.validate_contract()?;
    }
    writer.add_core_record(record)?;
    writer.certify_source(publication_pin_certificate(&source))?;
    let commit = writer.commit(|_| true)?;
    let mut publication = test_publication(commit.generation_id);
    publication.certified_source_count = 1;
    publication.certified_source_bytes = 128;
    publication.current = SourceBackedRefreshCurrent {
        source_count: 1,
        indexed_documents: 1,
        complete_records: 1,
        retained_records: 1,
        certified_source_bytes: 128,
        ..SourceBackedRefreshCurrent::default()
    };
    publication.published_explicit_source_catalog = execution.explicit_source_catalog.cloned();
    let selected = match &execution.admitted_refresh().publication_scope() {
        SourceBackedRefreshScope::All => BTreeSet::new(),
        SourceBackedRefreshScope::Exact(routes) => routes.iter().cloned().collect(),
    };
    publication.route_results = selected
        .iter()
        .map(|route| SourceBackedRefreshRouteResult::succeeded(route.as_str().to_owned(), true))
        .collect();
    Ok(publication)
}

fn publication_pin_executor(
    publish_nonempty: Arc<AtomicBool>,
) -> Arc<dyn SourceBackedRefreshExecutor> {
    Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
        publish_pin_fixture(&execution, publish_nonempty.load(Ordering::SeqCst))
    })
}

fn manual_all_request_without_catalog(coordinator: &CoreRefreshEngine, data_root: &Path) -> Value {
    let observations = coordinator
        .scheduled_route_ids_for_test()
        .into_iter()
        .map(|route| (route, Some("ab".repeat(32))))
        .collect();
    coordinator
        .handle_ipc_request_with_admission_fence_for_test(
            data_root,
            &json!({
                "schema_version": 1,
                "op": SOURCE_REFRESH_REQUEST_OP,
                "request_id": Uuid::now_v7().to_string(),
                "mode": "wait",
                "operation": "import",
                "refresh_selector": {"kind": "all_automatic"},
                "fresh_after_admitted_snapshot": true,
            }),
            observations,
        )
        .unwrap()
        .expect("manual all-route refresh response")
}

#[test]
fn warm_dirty_route_burst_uses_one_bounded_refresh_and_publication() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    ctx_history_index::GenerationWriter::open(
        source_backed_index_root(&data_root),
        WriterOptions::default(),
    )
    .unwrap()
    .into_writer()
    .unwrap()
    .commit(|_| true)
    .unwrap();
    let routes = BTreeSet::from([
        route_identity(0x17),
        route_identity(0x18),
        route_identity(0x19),
    ]);
    let calls = Arc::new(AtomicUsize::new(0));
    let scans = Arc::new(Mutex::new(BTreeMap::<SourceRouteIdentity, usize>::new()));
    let expected_routes = routes.clone();
    let executor_calls = Arc::clone(&calls);
    let executor_scans = Arc::clone(&scans);
    let coordinator = CoreRefreshEngine::with_executor_and_admitted_routes(
        Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
            executor_calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                execution.admitted_refresh().publication_scope(),
                SourceBackedRefreshScope::Exact(expected_routes.clone())
            );
            for route in &expected_routes {
                *executor_scans
                    .lock()
                    .unwrap()
                    .entry(route.clone())
                    .or_default() += 1;
            }
            publish_selected_routes(&execution, &expected_routes, None)
        }),
        routes.clone(),
    );
    coordinator.reconcile_watch_routes(
        routes.clone(),
        EventWatermark::new(1, 0),
        ledger_now_ms().saturating_sub(1_000),
    );

    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());
    let run = coordinator.run_next(&data_root).expect("batched dirty run");
    assert!(!run.failed, "{:#}", run.job);
    assert_eq!(run.scope, SourceBackedRefreshScope::Exact(routes.clone()));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        *scans.lock().unwrap(),
        routes
            .iter()
            .cloned()
            .map(|route| (route, 1))
            .collect::<BTreeMap<_, _>>()
    );
    assert!(!coordinator.has_scheduled_route_work());
    assert!(!coordinator
        .enqueue_next_dirty_route(&data_root, u64::MAX)
        .unwrap());
}

#[test]
fn restart_requeues_durable_watch_recovery_after_pointer_advance() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let committed = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let executor_committed = Arc::clone(&committed);
    let executor_release = Arc::clone(&release);
    let first = Arc::new(CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let publication =
                publication_lifecycle_tests::publish_empty_generation_with_request_metadata(
                    &execution, 0x99,
                )?;
            executor_committed.wait();
            executor_release.wait();
            Ok(publication)
        },
    )));
    let request_id = "019fcaaa-0000-7000-8000-000000000691".to_owned();
    let admitted = first
        .handle_ipc_request(
            &data_root,
            &json!({
                "schema_version": 1,
                "op": SOURCE_REFRESH_REQUEST_OP,
                "request_id": request_id,
                "mode": "wait",
                "operation": "refresh",
                "fresh_after_admitted_snapshot": true,
            }),
        )
        .unwrap()
        .expect("active wait admission");
    assert_eq!(admitted["request_state"], "admission_pending");
    let runner = Arc::clone(&first);
    let run_root = data_root.clone();
    let run = std::thread::spawn(move || runner.run_next(&run_root).expect("active wait"));

    committed.wait();
    first.fence_watch_uncertainty(EventWatermark::new(19, 1));
    release.wait();
    let interrupted = run.join().unwrap();
    assert_eq!(interrupted.job["request_state"], "running");
    assert_eq!(interrupted.job["progress"]["phase"], "watch_recovery");
    let generation = pin_published_generation(&data_root)
        .unwrap()
        .expect("physical generation advanced")
        .generation_id()
        .to_owned();
    assert_ne!(
        interrupted.job["previous_generation"].as_str(),
        Some(generation.as_str())
    );
    drop(first);

    let executions = Arc::new(AtomicUsize::new(0));
    let observed_executions = Arc::clone(&executions);
    let restarted =
        CoreRefreshEngine::with_executor(Arc::new(move |_: SourceBackedRefreshExecution<'_>| {
            observed_executions.fetch_add(1, Ordering::SeqCst);
            Err(anyhow!("restart recovery must remain nonterminal"))
        }));
    assert!(restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    let recovered = restarted
        .status(&request_id)
        .expect("recovered active wait");
    assert_eq!(recovered["request_state"], "admission_pending");
    assert_eq!(recovered["reconciliation_demand"], "exhaustive");
    assert_eq!(executions.load(Ordering::SeqCst), 0);

    let reconnect = restarted
        .handle_ipc_request(
            &data_root,
            &json!({
                "schema_version": 1,
                "op": SOURCE_REFRESH_REQUEST_OP,
                "request_id": request_id,
                "mode": "wait",
                "operation": "refresh",
                "fresh_after_admitted_snapshot": true,
            }),
        )
        .unwrap()
        .expect("reconnected active wait");
    assert_eq!(reconnect["request_id"], request_id);
    assert_eq!(reconnect["request_state"], "admission_pending");
}

mod additional;

#[path = "tests/receipt.rs"]
mod receipt_tests;

#[path = "tests/unsupported_refresh.rs"]
mod unsupported_refresh;

#[path = "tests/codex_union.rs"]
mod codex_union_tests;

#[path = "tests/request_coalescing.rs"]
mod request_coalescing_tests;

#[path = "tests/publication_lifecycle.rs"]
mod publication_lifecycle_tests;

#[path = "tests/durable_receipt.rs"]
mod durable_receipt_tests;
