use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
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
use ctx_history_refresh_execution::source_backed_requested_route_observations;

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
fn diagnostic_text_cannot_override_the_typed_terminal_outcome() {
    let mut attempt = new_refresh_attempt(
        None,
        SourceRefreshRuntimeMetadata::periodic(),
        RefreshIntent::AutomaticMaintenance,
        SourceBackedRefreshScope::All,
    );
    attempt.state = SourceBackedRefreshState::Failed;
    attempt.terminal_outcome = Some(
        RefreshTerminalOutcome::with_uniform_route_disposition(
            RefreshOutcomeCode::SourceRefreshFailed,
            true,
            BTreeSet::new(),
            uuid::Uuid::nil().to_string(),
            None,
            None,
            Some(RefreshRetryAdvice::RetryRequest),
            None,
        )
        .unwrap(),
    );
    attempt.last_error = Some(format!(
        "diagnostic mentions {} but is not classification authority",
        RefreshOutcomeCode::AllProviderTerminalCoverageUnavailable.as_str()
    ));

    let job = attempt.job_json();

    assert_eq!(job["error_code"], "source_refresh_failed");
    assert_eq!(job["reason"], "internal");
    assert_eq!(job["structured_outcome"]["code"], "source_refresh_failed");
    assert_eq!(job["structured_outcome"]["class"], "internal");
}

#[test]
fn observed_low_space_is_a_retryable_resource_failure() {
    let error = anyhow::Error::new(IndexError::CandidateFailureWithLowSpace {
        available: 0,
        cause: Box::new(IndexError::WriterInvariant("original worker failure")),
    });
    let outcome = source_backed_refresh_failure_outcome(
        &error,
        &BTreeSet::new(),
        &uuid::Uuid::nil().to_string(),
    )
    .unwrap();
    assert_eq!(outcome.code(), RefreshOutcomeCode::ResourceUnavailable);
    assert!(outcome.retryable());
    assert_eq!(
        outcome.retry_advice(),
        Some(RefreshRetryAdvice::RetryRequest)
    );
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
fn verified_publication_is_terminal_despite_synchronous_watch_uncertainty() {
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
                move |_, receipt| {
                    runner_preterminal.wait();
                    runner_release.wait();
                    Ok((
                        CoreRefreshTerminalSuccess::state_only(receipt),
                        PostPublicationRouteCoverageFence::fail_closed(),
                    ))
                },
                |_| Ok(()),
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

    let published = run.join().unwrap();
    assert_eq!(published.job["request_state"], "published");
    assert_eq!(published.job["progress"]["phase"], "published");
    let catalog_revision = coordinator.lock_state().watch_catalog_revision;
    assert!(coordinator
        .complete_watch_uncertainty_recovery(
            &data_root,
            SourceBackedWatchCatalog::default(),
            boundary,
            ledger_now_ms(),
        )
        .unwrap());
    assert_eq!(
        coordinator.lock_state().watch_catalog_revision,
        catalog_revision.saturating_add(1),
        "watch recovery must invalidate out-of-lock catalog observations"
    );
    let terminal = coordinator.status(&request_id).unwrap();
    assert_eq!(terminal["request_state"], "published");
    assert_eq!(terminal["published_generation"], "stale");
}

#[test]
fn newer_exhaustive_marker_survives_post_publication_coverage_fence() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let route = route_identity(0x61);
    let coordinator_slot = Arc::new(Mutex::new(None::<Arc<CoreRefreshEngine>>));
    let executor_slot = Arc::clone(&coordinator_slot);
    let executor_route = route.clone();
    let observation = "ab".repeat(32);
    let executor_observation = observation.clone();
    let executions = Arc::new(AtomicUsize::new(0));
    let executor_executions = Arc::clone(&executions);
    let executor: Arc<dyn SourceBackedRefreshExecutor> =
        Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
            let coordinator = executor_slot
                .lock()
                .unwrap()
                .as_ref()
                .expect("coordinator installed before execution")
                .clone();
            let first_execution = executor_executions.fetch_add(1, Ordering::SeqCst) == 0;
            if first_execution {
                coordinator.record_watch_routes_requiring_exhaustive_reconciliation(
                    [(executor_route.clone(), EventWatermark::new(1, 2))],
                    ledger_now_ms().saturating_sub(1_000),
                );
                coordinator.set_route_observations_for_test(
                    execution.request_id,
                    BTreeMap::from([(executor_route.clone(), executor_observation.clone())]),
                );
            }
            publish_pin_fixture_with_observations(
                &execution,
                false,
                if first_execution {
                    BTreeMap::from([(executor_route.clone(), executor_observation.clone())])
                } else {
                    BTreeMap::new()
                },
            )
        });
    let coordinator = Arc::new(CoreRefreshEngine::with_executor_and_admitted_routes(
        executor,
        [route.clone()],
    ));
    *coordinator_slot.lock().unwrap() = Some(Arc::clone(&coordinator));
    coordinator.initialize_watch_route_authority([route.clone()]);
    coordinator.record_watch_routes_requiring_exhaustive_reconciliation(
        [(route.clone(), EventWatermark::new(1, 1))],
        ledger_now_ms().saturating_sub(1_000),
    );
    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());
    assert!(coordinator
        .prepare_next_pending_admission(&data_root)
        .unwrap());
    let run = coordinator
        .run_next_with_coverage_fence_for_test(&data_root, |_, routes| {
            assert_eq!(routes, &BTreeSet::from([route.clone()]));
            Ok(BTreeMap::from([(route.clone(), Some(observation.clone()))]))
        })
        .expect("original exhaustive maintenance run");
    let request_id = request_id(&run.job);
    assert!(Uuid::parse_str(&request_id).is_ok());
    assert_eq!(run.job["request_state"], "published");
    assert_eq!(
        run.coverage_certificate()
            .expect("coverage certificate")
            .exact_route_boundaries()
            .collect::<Vec<_>>(),
        vec![(&route, EventWatermark::new(1, 1), observation.as_str())]
    );
    assert_eq!(
        coordinator.status(&request_id).unwrap()["request_state"],
        "published"
    );
    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());
    let successor_id = coordinator.lock_state().active_request_id.clone().unwrap();
    assert!(Uuid::parse_str(&successor_id).is_ok());
    assert_ne!(successor_id, request_id);
    assert_eq!(
        coordinator.active_reconciliation_demand_for_test(),
        Some(SourceBackedReconciliationDemand::Exhaustive)
    );
}

#[test]
fn newer_missing_member_uncertainty_and_failure_rearm_exhaustive_obligations() {
    for fail in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let route = route_identity(if fail { 0x63 } else { 0x62 });
        let coordinator = CoreRefreshEngine::new();
        coordinator.initialize_watch_route_authority([route.clone()]);
        coordinator.record_watch_routes_requiring_exhaustive_reconciliation(
            [(route.clone(), EventWatermark::new(1, 1))],
            ledger_now_ms().saturating_sub(1_000),
        );
        assert!(coordinator
            .enqueue_next_dirty_route(&data_root, ledger_now_ms())
            .unwrap());
        assert!(coordinator
            .prepare_next_pending_admission(&data_root)
            .unwrap());
        let route_for_run = route.clone();
        let _ = coordinator.run_next_with(
            move |active, engine| {
                engine.admit_refresh_scope_for_test(
                    active,
                    &SourceBackedRefreshScope::Exact(BTreeSet::from([route_for_run.clone()])),
                )?;
                if fail {
                    Err(anyhow!("rearm exhaustive obligation"))
                } else {
                    engine.record_watch_routes_requiring_exhaustive_reconciliation(
                        [(route_for_run.clone(), EventWatermark::new(1, 2))],
                        ledger_now_ms().saturating_sub(1_000),
                    );
                    let mut publication = test_publication("generation-62");
                    publication.route_results = vec![SourceBackedRefreshRouteResult::succeeded(
                        route_for_run.as_str().to_owned(),
                        true,
                    )];
                    Ok(publication)
                }
            },
            || {
                Ok(if fail {
                    None
                } else {
                    Some("generation-62".to_owned())
                })
            },
            |_| Ok(()),
            |_| Ok(()),
        );
        assert!(coordinator
            .enqueue_next_dirty_route(&data_root, u64::MAX)
            .unwrap());
        assert_eq!(
            coordinator.active_reconciliation_demand_for_test(),
            Some(SourceBackedReconciliationDemand::Exhaustive),
            "fail={fail}"
        );
    }
}

fn empty_test_publication(generation_id: impl Into<String>) -> SourceBackedRefreshPublication {
    let mut publication = test_publication(generation_id);
    publication.certified_source_count = 0;
    publication.certified_source_bytes = 0;
    publication.current = SourceBackedRefreshCurrent::default();
    publication
}

fn commit_source_backed_test_generation(
    writer: ctx_history_index::GenerationWriter,
) -> ctx_history_index::Result<ctx_history_index::CommitReceipt> {
    commit_source_backed_test_generation_with_facts(
        writer,
        SourceBackedTestGenerationFacts::default(),
    )
}

#[derive(Clone, Default)]
pub(super) struct SourceBackedTestGenerationFacts {
    pub(super) explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    pub(super) catalog_route_bindings: Vec<ExplicitSourceCatalogRouteBinding>,
    pub(super) route_observations: BTreeMap<SourceRouteIdentity, String>,
    pub(super) route_controls: BTreeMap<SourceRouteIdentity, Vec<u8>>,
}

pub(super) fn commit_source_backed_test_generation_with_facts(
    writer: ctx_history_index::GenerationWriter,
    facts: SourceBackedTestGenerationFacts,
) -> ctx_history_index::Result<ctx_history_index::CommitReceipt> {
    let state = SourceBackedGenerationState::new(
        facts.explicit_source_catalog.clone(),
        facts.catalog_route_bindings.clone(),
        facts.route_observations.clone(),
        facts.route_controls.clone(),
        Vec::new(),
    )?
    .envelope()?;
    let published =
        writer.commit_with_generation_state(|_| true, |_| false, |_| Ok(state), |_| Ok(()))?;
    Ok(published.into_parts().0)
}

fn empty_source_backed_test_state(
) -> ctx_history_index::Result<ctx_history_index::GenerationStateEnvelope> {
    SourceBackedGenerationState::new(
        None,
        Vec::new(),
        BTreeMap::new(),
        BTreeMap::new(),
        Vec::new(),
    )?
    .envelope()
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

fn synthetic_catalog_route_bindings(
    catalog: Option<&ExplicitSourceCatalogAuthority>,
    routes: &BTreeSet<SourceRouteIdentity>,
) -> Vec<ExplicitSourceCatalogRouteBinding> {
    let Some(route) = routes.iter().next() else {
        return Vec::new();
    };
    catalog
        .into_iter()
        .flat_map(ExplicitSourceCatalogAuthority::route_lineages)
        .map(|catalog_lineage| ExplicitSourceCatalogRouteBinding {
            catalog_lineage,
            route_identity: route.as_str().to_owned(),
        })
        .collect()
}

fn publish_selected_routes(
    execution: &SourceBackedRefreshExecution<'_>,
    selected: &BTreeSet<SourceRouteIdentity>,
    failed_route: Option<(&SourceRouteIdentity, &'static str)>,
) -> Result<SourceBackedRefreshPublication> {
    let retained = open_verified_index(execution.index_root).ok();
    let retain_rejection_fixture = retained
        .as_ref()
        .is_some_and(|index| !index.manifest().sources.is_empty());
    let mut writer =
        ctx_history_index::GenerationWriter::open(execution.index_root, WriterOptions::default())?
            .into_writer()
            .map_err(crate::committed_generation_recovery_error)?;
    let staged_source = if retain_rejection_fixture {
        let source = publication_pin_source_with_anchor(0x93);
        writer.begin_source(source.clone())?;
        writer.add_core_record(publication_pin_record(&source))?;
        writer.certify_source(publication_rejection_certificate(&source))?;
        Some(source)
    } else {
        None
    };
    let mut source_routes = retained
        .as_ref()
        .map(|index| index.manifest().source_routes().to_vec())
        .unwrap_or_default();
    for route in selected {
        if source_routes
            .iter()
            .all(|existing| existing.route_identity() != route)
        {
            source_routes.push(ctx_history_index::SourceRouteSnapshot::present(
                route.clone(),
                Vec::new(),
            )?);
        }
    }
    let mut all_sources = retained
        .as_ref()
        .into_iter()
        .flat_map(|index| index.manifest().sources.iter())
        .map(|source| source.observation().source().clone())
        .collect::<Vec<_>>();
    if let Some(source) = staged_source {
        if !all_sources.contains(&source) {
            all_sources.push(source);
        }
    }
    let unowned_sources = all_sources
        .into_iter()
        .filter(|source| {
            source_routes
                .iter()
                .all(|route| !route.sources().contains(source))
        })
        .collect::<Vec<_>>();
    if !unowned_sources.is_empty() {
        if let Some(route) = source_routes
            .iter_mut()
            .find(|route| route.missing_state().is_none())
        {
            let mut sources = route.sources().to_vec();
            sources.extend(unowned_sources);
            *route = ctx_history_index::SourceRouteSnapshot::present(
                route.route_identity().clone(),
                sources,
            )?;
        } else {
            let route = selected
                .iter()
                .next()
                .cloned()
                .unwrap_or_else(|| SourceRouteIdentity::from_sha256("ed".repeat(32)).unwrap());
            source_routes.push(ctx_history_index::SourceRouteSnapshot::present(
                route,
                unowned_sources,
            )?);
        }
    }
    let retained_route = source_routes
        .iter()
        .find(|route| !route.sources().is_empty())
        .map(|route| route.route_identity().clone());
    writer.set_present_source_routes(source_routes)?;
    let mut route_results = selected
        .iter()
        .map(|route| SourceBackedRefreshRouteResult::succeeded(route.as_str().to_owned(), true))
        .collect::<Vec<_>>();
    if let Some((route, class)) = failed_route {
        let result = route_results
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
    let route_observations = source_backed_requested_route_observations(
        execution.admitted_refresh().discovery().watch_catalog(),
        selected,
    )
    .into_iter()
    .filter_map(|(route, observation)| observation.map(|observation| (route, observation)))
    .collect::<BTreeMap<_, _>>();
    let catalog_route_bindings =
        synthetic_catalog_route_bindings(execution.explicit_source_catalog, selected);
    let zero_source_authority = if retained_route.is_none() {
        route_results
            .iter()
            .filter(|result| result.outcome.is_success())
            .map(|result| {
                (
                    SourceRouteIdentity::from_sha256(result.route_identity.clone()).unwrap(),
                    SourceBackedZeroSourceAuthorityKind::CompleteEmptyInventory,
                )
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let commit = commit_source_backed_test_generation_with_facts(
        writer,
        SourceBackedTestGenerationFacts {
            explicit_source_catalog: execution.explicit_source_catalog.cloned(),
            catalog_route_bindings: catalog_route_bindings.clone(),
            route_observations,
            ..SourceBackedTestGenerationFacts::default()
        },
    )?;
    let mut publication = empty_test_publication(commit.generation_id.clone());
    publication.current = SourceBackedRefreshCurrent::from_sources(&commit.manifest().sources, 0)?;
    publication.certified_source_count = publication.current.source_count;
    publication.certified_source_bytes = publication.current.certified_source_bytes;
    publication.published_explicit_source_catalog = execution.explicit_source_catalog.cloned();
    publication.catalog_route_bindings = catalog_route_bindings;
    publication.route_results = route_results;
    publication.zero_source_authority = zero_source_authority
        .into_iter()
        .map(|(route_identity, kind)| SourceBackedZeroSourceAuthority {
            generation_id: commit.generation_id.clone(),
            route_identity,
            kind,
        })
        .collect();
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
    commit_source_backed_test_generation(writer)
        .unwrap()
        .generation_id
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
    publish_pin_fixture_with_observations(execution, alternate_source, BTreeMap::new())
}

fn publish_pin_fixture_with_observations(
    execution: &SourceBackedRefreshExecution<'_>,
    alternate_source: bool,
    route_observations: BTreeMap<SourceRouteIdentity, String>,
) -> Result<SourceBackedRefreshPublication> {
    let selected = execution
        .admitted_refresh()
        .exact_routes()
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
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
    if !selected.is_empty() {
        let owning_route = selected.iter().next().cloned();
        writer.set_present_source_routes(
            selected
                .iter()
                .cloned()
                .map(|route| {
                    let sources = if Some(&route) == owning_route.as_ref() {
                        vec![source.clone()]
                    } else {
                        Vec::new()
                    };
                    ctx_history_index::SourceRouteSnapshot::present(route, sources)
                })
                .collect::<ctx_history_index::Result<Vec<_>>>()?,
        )?;
    }
    let route_results = selected
        .iter()
        .map(|route| SourceBackedRefreshRouteResult::succeeded(route.as_str().to_owned(), true))
        .collect::<Vec<_>>();
    let catalog_route_bindings =
        synthetic_catalog_route_bindings(execution.explicit_source_catalog, &selected);
    let zero_source_authority = Vec::new();
    let commit = commit_source_backed_test_generation_with_facts(
        writer,
        SourceBackedTestGenerationFacts {
            explicit_source_catalog: execution.explicit_source_catalog.cloned(),
            catalog_route_bindings: catalog_route_bindings.clone(),
            route_observations,
            ..SourceBackedTestGenerationFacts::default()
        },
    )?;
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
    publication.catalog_route_bindings = catalog_route_bindings;
    publication.route_results = route_results;
    publication.zero_source_authority = zero_source_authority
        .into_iter()
        .map(|(route_identity, kind)| SourceBackedZeroSourceAuthority {
            generation_id: publication.generation_id.clone(),
            route_identity,
            kind,
        })
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
                "refresh_intent": {
                    "kind": "selected_import",
                    "selection": {"kind": "all"},
                },
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
    commit_source_backed_test_generation(
        ctx_history_index::GenerationWriter::open(
            source_backed_index_root(&data_root),
            WriterOptions::default(),
        )
        .unwrap()
        .into_writer()
        .unwrap(),
    )
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

mod additional;

#[path = "tests/execution_persistence.rs"]
mod execution_persistence;

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
