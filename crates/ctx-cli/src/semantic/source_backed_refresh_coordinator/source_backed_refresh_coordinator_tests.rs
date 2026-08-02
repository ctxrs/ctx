use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc, Barrier,
    },
};

use crate::semantic::dirty_source_routes::EventWatermark;
use ctx_history_capture::{
    provider_source_for_path, DiscoveryPlatform, DiscoveryPlatformDirs, ProviderCatalogSupport,
    ProviderImportSupport, ProviderSource, ProviderSourceKind, SourceBackedFailedRoute,
};
use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CoreRecord, EventIdentityInput,
    NativeItemKey, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput, SourceAnchor,
    SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::SourceRouteIdentity;
use rusqlite::Connection;

use super::*;

#[path = "source_backed_refresh_coordinator_tests/registry_policy.rs"]
mod registry_policy;

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
        selected_route_ids: Vec::new(),
        successful_route_ids: Vec::new(),
        successful_route_changes: BTreeMap::new(),
        failed_route_outcomes: Vec::new(),
        catalog_route_outcomes: Vec::new(),
        source_failures: Vec::new(),
        generation_id: generation_id.into(),
        published_explicit_source_catalog: load_explicit_source_catalog_authority(
            tempfile::tempdir().unwrap().path(),
        )
        .unwrap(),
        scanned_routes: 1,
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

fn empty_test_publication(generation_id: impl Into<String>) -> SourceBackedRefreshPublication {
    let mut publication = test_publication(generation_id);
    publication.certified_source_count = 0;
    publication.certified_source_bytes = 0;
    publication.current = SourceBackedRefreshCurrent::default();
    publication
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
    ExplicitSourceCatalogAuthority::from_json(&json!({
        "schema_version": 1,
        "revision": revision,
        "integrity": {
            "algorithm": "sha256",
            "digest": format!("{digest_byte:02x}").repeat(32),
        },
    }))
    .unwrap()
}

fn physically_selected_routes(
    execution: &SourceBackedRefreshExecution<'_>,
    current_routes: &BTreeSet<SourceRouteIdentity>,
) -> BTreeSet<SourceRouteIdentity> {
    match &execution.scope {
        SourceBackedRefreshScope::All => current_routes
            .difference(&execution.covered_route_ids)
            .cloned()
            .collect(),
        SourceBackedRefreshScope::Exact(routes) => routes.clone(),
    }
}

fn publish_selected_routes(
    execution: &SourceBackedRefreshExecution<'_>,
    selected: &BTreeSet<SourceRouteIdentity>,
    failed_route: Option<(&SourceRouteIdentity, &'static str)>,
) -> Result<SourceBackedRefreshPublication> {
    let commit =
        ctx_history_index::GenerationWriter::open(execution.index_root, WriterOptions::default())?
            .commit(|_| true)?;
    let mut publication = empty_test_publication(commit.generation_id);
    publication.published_explicit_source_catalog = execution
        .explicit_source_catalog
        .cloned()
        .expect("refresh catalog authority");
    publication.scanned_routes = selected.len();
    publication.selected_route_ids = selected
        .iter()
        .map(|route| route.as_str().to_owned())
        .collect();
    publication.successful_route_ids = publication.selected_route_ids.clone();
    if let Some((route, class)) = failed_route {
        publication
            .successful_route_ids
            .retain(|selected| selected != route.as_str());
        publication.source_failures = vec![SourceBackedRefreshSourceFailure {
            route_identity: route.as_str().to_owned(),
            source_identity: "content-free-source".to_owned(),
            provider: "fixture".to_owned(),
            class: class.to_owned(),
            carried_forward: true,
            source_selector: "fixture source".to_owned(),
            detail: "fixture failure".to_owned(),
        }];
    }
    publication.successful_route_changes = publication
        .successful_route_ids
        .iter()
        .cloned()
        .map(|route| (route, true))
        .collect();
    Ok(publication)
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
        ctx_history_index::GenerationWriter::open(index_root, WriterOptions::default()).unwrap();
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
        session_id,
        source.clone(),
        0,
        "message",
        "primary",
        true,
        "publication-pin-test-v1",
        "exact publication pin fixture",
    )
    .unwrap();
    record.provider_session_id = Some("publication-pin-session".to_owned());
    record.native_event_id = Some(TypedKey::U64(0));
    record.role = Some("user".to_owned());
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
    nonempty: bool,
) -> Result<SourceBackedRefreshPublication> {
    let mut writer =
        ctx_history_index::GenerationWriter::open(execution.index_root, WriterOptions::default())?;
    if nonempty {
        let source = publication_pin_source();
        writer.begin_source(source.clone())?;
        writer.add_core_record(publication_pin_record(&source))?;
        writer.certify_source(publication_pin_certificate(&source))?;
    }
    let commit = writer.commit(|_| true)?;
    let mut publication = if nonempty {
        test_publication(commit.generation_id)
    } else {
        empty_test_publication(commit.generation_id)
    };
    if nonempty {
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
    }
    publication.published_explicit_source_catalog = execution
        .explicit_source_catalog
        .cloned()
        .expect("publication pin catalog authority");
    publication.selected_route_ids = match &execution.scope {
        SourceBackedRefreshScope::All => Vec::new(),
        SourceBackedRefreshScope::Exact(routes) => routes
            .iter()
            .map(|route| route.as_str().to_owned())
            .collect(),
    };
    publication.successful_route_ids = publication.selected_route_ids.clone();
    publication.scanned_routes = publication.selected_route_ids.len();
    Ok(publication)
}

fn publication_pin_executor(
    publish_nonempty: Arc<AtomicBool>,
) -> Arc<dyn SourceBackedRefreshExecutor> {
    Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
        publish_pin_fixture(&execution, publish_nonempty.load(Ordering::SeqCst))
    })
}

fn manual_all_request(
    coordinator: &CoreRefreshEngine,
    data_root: &Path,
    authority: &ExplicitSourceCatalogAuthority,
) -> Value {
    coordinator
        .handle_ipc_request(
            data_root,
            &json!({
                "schema_version": 1,
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "operation": "import",
                "explicit_source_catalog": authority.to_json(),
            }),
        )
        .unwrap()
        .expect("manual all-route refresh response")
}

#[test]
fn queued_startup_exact_is_upgraded_to_one_manual_all_scan() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let routes = BTreeSet::from([
        route_identity(0x11),
        route_identity(0x12),
        route_identity(0x13),
    ]);
    let scans = Arc::new(Mutex::new(BTreeMap::<SourceRouteIdentity, usize>::new()));
    let executor_routes = routes.clone();
    let executor_scans = Arc::clone(&scans);
    let coordinator = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            assert_eq!(execution.scope, SourceBackedRefreshScope::All);
            assert!(execution.covered_route_ids.is_empty());
            let selected = physically_selected_routes(&execution, &executor_routes);
            for route in &selected {
                *executor_scans
                    .lock()
                    .unwrap()
                    .entry(route.clone())
                    .or_default() += 1;
            }
            publish_selected_routes(&execution, &selected, None)
        },
    ));
    coordinator.reconcile_watch_routes(
        routes.clone(),
        EventWatermark::new(1, 0),
        ledger_now_ms().saturating_sub(1_000),
    );
    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());
    let automatic_request_id =
        read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root))
            .and_then(|status| status["request_id"].as_str().map(str::to_owned))
            .expect("queued startup exact request ID");
    let authority = load_explicit_source_catalog_authority(&data_root).unwrap();
    let manual = manual_all_request(&coordinator, &data_root, &authority);

    assert_eq!(request_id(&manual), automatic_request_id);
    assert_eq!(manual["trigger"], "import");
    let run = coordinator.run_next(&data_root).expect("upgraded all run");
    assert!(!run.failed);
    assert_eq!(run.scope, SourceBackedRefreshScope::All);
    assert_eq!(
        *scans.lock().unwrap(),
        routes
            .iter()
            .cloned()
            .map(|route| (route, 1))
            .collect::<BTreeMap<_, _>>()
    );
    assert!(!coordinator.has_scheduled_route_work());
}

#[test]
fn running_startup_exact_continues_manual_all_without_rescanning() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let routes = BTreeSet::from([
        route_identity(0x21),
        route_identity(0x22),
        route_identity(0x23),
    ]);
    let scans = Arc::new(Mutex::new(BTreeMap::<SourceRouteIdentity, usize>::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let executor_routes = routes.clone();
    let executor_scans = Arc::clone(&scans);
    let executor_calls = Arc::clone(&calls);
    let executor_entered = Arc::clone(&entered);
    let executor_release = Arc::clone(&release);
    let coordinator = Arc::new(CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let selected = physically_selected_routes(&execution, &executor_routes);
            for route in &selected {
                *executor_scans
                    .lock()
                    .unwrap()
                    .entry(route.clone())
                    .or_default() += 1;
            }
            let first = executor_calls.fetch_add(1, Ordering::SeqCst) == 0;
            if first {
                assert!(matches!(
                    execution.scope,
                    SourceBackedRefreshScope::Exact(_)
                ));
                assert!(execution.covered_route_ids.is_empty());
                executor_entered.wait();
                executor_release.wait();
            } else {
                assert_eq!(execution.scope, SourceBackedRefreshScope::All);
                assert_eq!(execution.covered_route_ids.len(), 1);
            }
            let mut publication = publish_selected_routes(&execution, &selected, None)?;
            if first {
                publication.current.removed_source_count = 1;
            }
            Ok(publication)
        },
    )));
    coordinator.reconcile_watch_routes(
        routes.clone(),
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
                .expect("running startup exact");
            assert!(!run.failed);
        });
        entered.wait();
        let manual = manual_all_request(&coordinator, &data_root, &authority);
        release.wait();
        manual
    });

    let manual_request_id = request_id(&manual);
    let successor = coordinator
        .run_next(&data_root)
        .expect("manual all continuation");
    assert!(!successor.failed);
    assert_eq!(request_id(&successor.job), manual_request_id);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        *scans.lock().unwrap(),
        routes
            .iter()
            .cloned()
            .map(|route| (route, 1))
            .collect::<BTreeMap<_, _>>()
    );
    let terminal = coordinator.status(&manual_request_id).unwrap();
    assert_eq!(terminal["request_state"], "published");
    assert_eq!(terminal["generation_changed"], true);
    assert_eq!(terminal["scanned_routes"], routes.len());
    assert_eq!(terminal["receipt"]["current"]["removed_source_count"], 1);
    assert_eq!(
        terminal["receipt"]["successful_route_total"]
            .as_u64()
            .unwrap() as usize,
        routes.len()
    );
    assert!(!coordinator.has_scheduled_route_work());
}

#[test]
fn failed_running_exact_remains_in_manual_all_successor_work() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let routes = BTreeSet::from([route_identity(0x31), route_identity(0x32)]);
    let first_route = routes.iter().next().unwrap().clone();
    let scans = Arc::new(Mutex::new(BTreeMap::<SourceRouteIdentity, usize>::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let executor_routes = routes.clone();
    let executor_scans = Arc::clone(&scans);
    let executor_calls = Arc::clone(&calls);
    let executor_entered = Arc::clone(&entered);
    let executor_release = Arc::clone(&release);
    let executor_first_route = first_route.clone();
    let coordinator = Arc::new(CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let selected = physically_selected_routes(&execution, &executor_routes);
            for route in &selected {
                *executor_scans
                    .lock()
                    .unwrap()
                    .entry(route.clone())
                    .or_default() += 1;
            }
            let first = executor_calls.fetch_add(1, Ordering::SeqCst) == 0;
            if first {
                executor_entered.wait();
                executor_release.wait();
            } else {
                assert!(execution.covered_route_ids.is_empty());
            }
            publish_selected_routes(
                &execution,
                &selected,
                first.then_some((&executor_first_route, "unavailable")),
            )
        },
    )));
    coordinator.reconcile_watch_routes(
        routes.clone(),
        EventWatermark::new(3, 0),
        ledger_now_ms().saturating_sub(1_000),
    );
    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());
    let authority = load_explicit_source_catalog_authority(&data_root).unwrap();

    std::thread::scope(|scope| {
        let runner = Arc::clone(&coordinator);
        let runner_root = data_root.clone();
        scope.spawn(move || {
            assert!(!runner.run_next(&runner_root).unwrap().failed);
        });
        entered.wait();
        let _manual = manual_all_request(&coordinator, &data_root, &authority);
        release.wait();
    });
    assert!(!coordinator.run_next(&data_root).unwrap().failed);

    let observed = scans.lock().unwrap();
    assert_eq!(observed.get(&first_route), Some(&2));
    for route in routes.iter().filter(|route| *route != &first_route) {
        assert_eq!(observed.get(route), Some(&1));
    }
    assert!(!coordinator.has_scheduled_route_work());
}

#[test]
fn event_during_running_exact_invalidates_manual_all_coverage() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let routes = BTreeSet::from([route_identity(0x41), route_identity(0x42)]);
    let first_route = routes.iter().next().unwrap().clone();
    let scans = Arc::new(Mutex::new(BTreeMap::<SourceRouteIdentity, usize>::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let executor_routes = routes.clone();
    let executor_scans = Arc::clone(&scans);
    let executor_calls = Arc::clone(&calls);
    let executor_entered = Arc::clone(&entered);
    let executor_release = Arc::clone(&release);
    let coordinator = Arc::new(CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let selected = physically_selected_routes(&execution, &executor_routes);
            for route in &selected {
                *executor_scans
                    .lock()
                    .unwrap()
                    .entry(route.clone())
                    .or_default() += 1;
            }
            if executor_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                executor_entered.wait();
                executor_release.wait();
            } else {
                assert!(execution.covered_route_ids.is_empty());
            }
            publish_selected_routes(&execution, &selected, None)
        },
    )));
    let observed_at_ms = ledger_now_ms().saturating_sub(1_000);
    coordinator.reconcile_watch_routes(routes.clone(), EventWatermark::new(4, 0), observed_at_ms);
    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());
    let authority = load_explicit_source_catalog_authority(&data_root).unwrap();

    std::thread::scope(|scope| {
        let runner = Arc::clone(&coordinator);
        let runner_root = data_root.clone();
        scope.spawn(move || {
            assert!(!runner.run_next(&runner_root).unwrap().failed);
        });
        entered.wait();
        let _manual = manual_all_request(&coordinator, &data_root, &authority);
        coordinator.record_watch_routes(
            [(first_route.clone(), EventWatermark::new(4, 1))],
            observed_at_ms,
        );
        release.wait();
    });
    assert!(!coordinator.run_next(&data_root).unwrap().failed);

    let observed = scans.lock().unwrap();
    assert_eq!(observed.get(&first_route), Some(&2));
    for route in routes.iter().filter(|route| *route != &first_route) {
        assert_eq!(observed.get(route), Some(&1));
    }
    assert!(!coordinator.has_scheduled_route_work());
}

#[test]
fn ordinary_manual_all_still_scans_every_current_route() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let routes = BTreeSet::from([route_identity(0x45), route_identity(0x46)]);
    let scans = Arc::new(Mutex::new(BTreeMap::<SourceRouteIdentity, usize>::new()));
    let executor_routes = routes.clone();
    let executor_scans = Arc::clone(&scans);
    let coordinator = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            assert_eq!(execution.scope, SourceBackedRefreshScope::All);
            assert!(execution.covered_route_ids.is_empty());
            let selected = physically_selected_routes(&execution, &executor_routes);
            for route in &selected {
                *executor_scans
                    .lock()
                    .unwrap()
                    .entry(route.clone())
                    .or_default() += 1;
            }
            publish_selected_routes(&execution, &selected, None)
        },
    ));
    coordinator.reconcile_watch_routes(
        routes.clone(),
        EventWatermark::new(5, 0),
        ledger_now_ms().saturating_sub(1_000),
    );
    let authority = load_explicit_source_catalog_authority(&data_root).unwrap();
    let manual = manual_all_request(&coordinator, &data_root, &authority);

    let run = coordinator
        .run_next(&data_root)
        .expect("ordinary manual all");
    assert!(!run.failed);
    assert_eq!(request_id(&run.job), request_id(&manual));
    assert_eq!(
        *scans.lock().unwrap(),
        routes
            .iter()
            .cloned()
            .map(|route| (route, 1))
            .collect::<BTreeMap<_, _>>()
    );
    assert!(!coordinator.has_scheduled_route_work());
}

#[test]
fn exact_route_event_during_execution_creates_one_successor_and_noop_ack_cleans_it() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let route = route_identity(0x51);
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let executor_calls = Arc::clone(&calls);
    let executor_entered = Arc::clone(&entered);
    let executor_release = Arc::clone(&release);
    let executor_route = route.clone();
    let executor: Arc<dyn SourceBackedRefreshExecutor> =
        Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
            assert_eq!(
                execution.scope,
                SourceBackedRefreshScope::exact([executor_route.clone()])
            );
            let call = executor_calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                executor_entered.wait();
                executor_release.wait();
            }
            let commit = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?
            .commit(|_| true)?;
            let mut publication = empty_test_publication(commit.generation_id);
            publication.published_explicit_source_catalog = execution
                .explicit_source_catalog
                .cloned()
                .expect("exact refresh catalog authority");
            publication.scanned_routes = 1;
            publication.selected_route_ids = vec![executor_route.as_str().to_owned()];
            publication.successful_route_ids = vec![executor_route.as_str().to_owned()];
            Ok(publication)
        });
    let coordinator = Arc::new(CoreRefreshEngine::with_executor(executor));
    let observed_at_ms = ledger_now_ms().saturating_sub(1_000);
    coordinator.reconcile_watch_routes([route.clone()], EventWatermark::new(1, 0), observed_at_ms);
    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());

    std::thread::scope(|scope| {
        let runner = Arc::clone(&coordinator);
        let runner_data_root = data_root.clone();
        scope.spawn(move || {
            let run = runner
                .run_next(&runner_data_root)
                .expect("first exact route run");
            assert!(!run.failed);
            assert!(matches!(run.scope, SourceBackedRefreshScope::Exact(_)));
        });
        entered.wait();
        coordinator
            .record_watch_routes([(route.clone(), EventWatermark::new(1, 1))], observed_at_ms);
        release.wait();
    });

    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());
    let successor = coordinator
        .run_next(&data_root)
        .expect("successor exact route run");
    assert!(!successor.failed);
    assert!(!successor.did_work, "unchanged exact route must be a no-op");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(!coordinator.has_scheduled_route_work());
    assert!(!coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());
}

fn route_failure_executor(
    route: SourceRouteIdentity,
    class: &'static str,
) -> Arc<dyn SourceBackedRefreshExecutor> {
    Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
        let commit = ctx_history_index::GenerationWriter::open(
            execution.index_root,
            WriterOptions::default(),
        )?
        .commit(|_| true)?;
        let mut publication = empty_test_publication(commit.generation_id);
        publication.published_explicit_source_catalog = execution
            .explicit_source_catalog
            .cloned()
            .expect("exact refresh catalog authority");
        publication.scanned_routes = 1;
        publication.selected_route_ids = vec![route.as_str().to_owned()];
        publication.failed_route_outcomes = vec![SourceBackedRefreshRouteFailure {
            route_identity: route.as_str().to_owned(),
            source_identity: "content-free-source".to_owned(),
            provider: "fixture".to_owned(),
            class: class.to_owned(),
            carried_forward: true,
        }];
        publication.source_failures = vec![SourceBackedRefreshSourceFailure {
            route_identity: route.as_str().to_owned(),
            source_identity: "content-free-source".to_owned(),
            provider: "fixture".to_owned(),
            class: class.to_owned(),
            carried_forward: true,
            source_selector: "fixture source".to_owned(),
            detail: "fixture failure".to_owned(),
        }];
        Ok(publication)
    })
}

#[test]
fn exact_route_receipt_failures_back_off_or_block_until_a_new_event() {
    let route = route_identity(0x61);
    let observed_at_ms = ledger_now_ms().saturating_sub(1_000);

    let retry_temp = tempfile::tempdir().unwrap();
    let retry_root = retry_temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&retry_root).unwrap();
    let retry =
        CoreRefreshEngine::with_executor(route_failure_executor(route.clone(), "unavailable"));
    retry.reconcile_watch_routes([route.clone()], EventWatermark::new(1, 0), observed_at_ms);
    assert!(retry
        .enqueue_next_dirty_route(&retry_root, ledger_now_ms())
        .unwrap());
    assert!(!retry.run_next(&retry_root).unwrap().failed);
    let retry_after = retry
        .next_dirty_route_due_in_ms(ledger_now_ms())
        .expect("retryable route remains scheduled");
    assert!(retry_after <= 10_000 && retry_after > 0);
    assert!(!retry
        .enqueue_next_dirty_route(&retry_root, ledger_now_ms())
        .unwrap());

    let blocked_temp = tempfile::tempdir().unwrap();
    let blocked_root = blocked_temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&blocked_root).unwrap();
    let blocked =
        CoreRefreshEngine::with_executor(route_failure_executor(route.clone(), "incompatible"));
    blocked.reconcile_watch_routes([route.clone()], EventWatermark::new(2, 0), observed_at_ms);
    assert!(blocked
        .enqueue_next_dirty_route(&blocked_root, ledger_now_ms())
        .unwrap());
    assert!(!blocked.run_next(&blocked_root).unwrap().failed);
    assert!(!blocked.has_scheduled_route_work());
    blocked.record_watch_routes([(route.clone(), EventWatermark::new(2, 0))], observed_at_ms);
    assert!(!blocked.has_scheduled_route_work());
    blocked.record_watch_routes([(route, EventWatermark::new(2, 1))], observed_at_ms);
    assert!(blocked.has_scheduled_route_work());
}

#[test]
fn systemic_exact_publication_failure_leaves_the_route_dirty_with_backoff() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let route = route_identity(0x71);
    let executor: Arc<dyn SourceBackedRefreshExecutor> =
        Arc::new(|_: SourceBackedRefreshExecution<'_>| Err(anyhow!("systemic fixture failure")));
    let coordinator = CoreRefreshEngine::with_executor(executor);
    coordinator.reconcile_watch_routes(
        [route],
        EventWatermark::new(3, 0),
        ledger_now_ms().saturating_sub(1_000),
    );
    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());
    assert!(coordinator.run_next(&data_root).unwrap().failed);
    let retry_after = coordinator
        .next_dirty_route_due_in_ms(ledger_now_ms())
        .expect("systemic failure remains dirty");
    assert!(retry_after <= 10_000 && retry_after > 0);
}

#[path = "source_backed_refresh_coordinator_tests_receipt.rs"]
mod receipt_tests;

#[test]
fn differing_catalog_authority_queues_one_successor_behind_a_running_refresh() {
    let temp = tempfile::tempdir().unwrap();
    let coordinator = Arc::new(CoreRefreshEngine::new());
    let first_authority = test_catalog_authority(1, 0x11);
    let second_authority = test_catalog_authority(2, 0x22);
    let request = |authority: &ExplicitSourceCatalogAuthority| {
        coordinator
            .handle_ipc_request(
                temp.path(),
                &json!({
                    "schema_version": 1,
                    "op": SOURCE_REFRESH_REQUEST_OP,
                    "mode": "wait",
                    "operation": "import",
                    "explicit_source_catalog": authority.to_json(),
                }),
            )
            .unwrap()
            .expect("source refresh response")
    };

    let first = request(&first_authority);
    let first_request_id = request_id(&first);
    let (gate, runner_started, runner_release) = RunningRefreshGate::new();

    let (second, second_replay) = std::thread::scope(|scope| {
        let runner = Arc::clone(&coordinator);
        let runner_request_id = first_request_id.clone();
        let runner_authority = first_authority.clone();
        scope.spawn(move || {
            let first_run = runner
                .run_next_with(
                    |request_id, _| {
                        assert_eq!(request_id, runner_request_id);
                        runner_started.send(()).expect("signal running refresh");
                        let _ = runner_release.recv();
                        let mut publication = test_publication("catalog-generation-1");
                        publication.published_explicit_source_catalog = runner_authority;
                        Ok(publication)
                    },
                    || Ok(Some("catalog-generation-1".to_owned())),
                    |_| Ok(()),
                    |_| Ok(()),
                )
                .expect("running first catalog refresh");
            assert!(!first_run.failed);
        });
        gate.wait_until_started();

        let second = request(&second_authority);
        let second_replay = request(&second_authority);
        gate.release();
        (second, second_replay)
    });

    let second_request_id = request_id(&second);
    assert_ne!(first_request_id, second_request_id);
    assert_eq!(request_id(&second_replay), second_request_id);
    assert_eq!(second_replay["coalesced_requests"], 1);
    assert_eq!(second["request_state"], "queued");
    assert_eq!(
        coordinator.status(&first_request_id).unwrap()["request_state"],
        "published"
    );
    let queued_second = coordinator.status(&second_request_id).unwrap();
    assert_eq!(queued_second["request_state"], "queued");
    assert_eq!(queued_second["previous_generation"], "catalog-generation-1");

    let second_run = coordinator
        .run_next_with(
            |request_id, _| {
                assert_eq!(request_id, second_request_id);
                let mut publication = test_publication("catalog-generation-2");
                publication.published_explicit_source_catalog = second_authority.clone();
                Ok(publication)
            },
            || Ok(Some("catalog-generation-2".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
    assert!(!second_run.failed);
    assert!(!coordinator.has_pending_request());
    let published_second = coordinator.status(&second_request_id).unwrap();
    assert_eq!(published_second["request_state"], "published");
    assert_eq!(
        ExplicitSourceCatalogAuthority::from_json(
            &published_second["published_explicit_source_catalog"]
        )
        .unwrap(),
        second_authority
    );
}

#[test]
fn active_and_pending_refreshes_are_bounded_with_a_typed_busy_response() {
    let temp = tempfile::tempdir().unwrap();
    let coordinator = CoreRefreshEngine::new();
    let request = |revision: u64| {
        let digest_byte = u8::try_from(revision).unwrap();
        let authority = test_catalog_authority(revision, digest_byte);
        coordinator
            .handle_ipc_request(
                temp.path(),
                &json!({
                    "schema_version": 1,
                    "op": SOURCE_REFRESH_REQUEST_OP,
                    "mode": "wait",
                    "operation": "import",
                    "explicit_source_catalog": authority.to_json(),
                }),
            )
            .unwrap()
            .expect("source refresh response")
    };

    let accepted = (1..=SOURCE_REFRESH_ACTIVE_PENDING_LIMIT)
        .map(|revision| request(u64::try_from(revision).unwrap()))
        .collect::<Vec<_>>();
    assert!(accepted.iter().all(|response| response["ok"] == true));
    assert_eq!(
        accepted
            .iter()
            .map(request_id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        SOURCE_REFRESH_ACTIVE_PENDING_LIMIT
    );

    let busy = request(99);
    assert_eq!(busy["ok"], false);
    assert_eq!(busy["status"], "busy");
    assert_eq!(busy["error_code"], "source_refresh_queue_full");
    assert_eq!(busy["reason"], "queue_full");
    assert_eq!(busy["retryable"], true);
    assert_eq!(
        busy["active_pending_requests"],
        SOURCE_REFRESH_ACTIVE_PENDING_LIMIT
    );
    assert_eq!(
        busy["max_active_pending_requests"],
        SOURCE_REFRESH_ACTIVE_PENDING_LIMIT
    );
    assert!(busy.get("request_id").is_none());
}

#[test]
fn terminal_history_is_trimmed_independently_from_inflight_capacity() {
    let coordinator = CoreRefreshEngine::new();
    let total = SOURCE_REFRESH_ATTEMPT_HISTORY + 3;
    let mut request_ids = Vec::with_capacity(total);

    for generation in 0..total {
        let previous = format!("generation-{generation}");
        let published = format!("generation-{}", generation.saturating_add(1));
        let request = coordinator.enqueue(Some(previous));
        request_ids.push(request_id(&request));
        let run = coordinator
            .run_next_with(
                |_, _| Ok(test_publication(published.clone())),
                || Ok(Some(published.clone())),
                |_| Ok(()),
                |_| Ok(()),
            )
            .expect("queued refresh");
        assert!(!run.failed);
    }

    assert!(request_ids[..3]
        .iter()
        .all(|request_id| coordinator.status(request_id).is_none()));
    assert!(request_ids[3..]
        .iter()
        .all(|request_id| coordinator.status(request_id).is_some()));

    let next = coordinator.enqueue(Some(format!("generation-{total}")));
    assert_eq!(next["request_state"], "queued");
    assert!(coordinator.has_pending_request());
}

#[test]
fn default_executor_with_implicit_import_catalog_keeps_automatic_discovery_and_maps_progress() {
    let coordinator = CoreRefreshEngine::new();
    assert_eq!(
        coordinator.executor.implementation_name(),
        std::any::type_name::<CaptureOwnedSourceBackedRefreshExecutor>()
    );

    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    std::fs::create_dir_all(home.join(".forge")).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    let forge = home.join(".forge/.forge.db");
    let forge_writer = Connection::open(&forge).unwrap();
    forge_writer
        .pragma_update(None, "journal_mode", "wal")
        .unwrap();
    forge_writer
        .pragma_update(None, "wal_autocheckpoint", 0)
        .unwrap();
    forge_writer
        .execute_batch("create table conversations (id text primary key);")
        .unwrap();
    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let updates = Mutex::new(Vec::new());
    let report_progress = |update: SourceBackedRefreshProgressUpdate| {
        updates.lock().unwrap().push((
            update.phase,
            update.completed_sources,
            update.total_sources,
            update.current_source,
            update.completed_records,
            update.completed_bytes,
        ));
        Ok(())
    };
    let implicit_catalog = load_explicit_source_catalog_authority(&data_root).unwrap();
    let execution = SourceBackedRefreshExecution {
        data_root: &data_root,
        index_root: &index_root,
        request_id: "all-provider-request",
        explicit_source_catalog: Some(&implicit_catalog),
        scope: SourceBackedRefreshScope::All,
        covered_route_ids: BTreeSet::new(),
        report_progress: &report_progress,
    };
    let mut provider_wide_calls = 0;

    let publication = execute_capture_owned_refresh_with(
        execution,
        &discovery,
        |observed_discovery,
         observed_report,
         observed_discovery_duration,
         observed_data_root,
         observed_index_root,
         observed_explicit_source_catalog,
         observed_scope,
         observed_covered_route_ids,
         progress| {
            provider_wide_calls += 1;
            assert_eq!(observed_discovery.home(), discovery.home());
            assert_eq!(observed_discovery.cwd(), discovery.cwd());
            assert_eq!(observed_discovery.data_root(), Some(data_root.as_path()));
            assert!(observed_report.sources.iter().any(|source| {
                source.provider == CaptureProvider::ForgeCode
                    && source.path == forge
                    && source.status == ProviderSourceStatus::Available
            }));
            assert_ne!(observed_discovery_duration, StdDuration::ZERO);
            assert_eq!(observed_data_root, data_root);
            assert_eq!(observed_index_root, index_root);
            assert_eq!(
                observed_explicit_source_catalog,
                Some(&implicit_catalog),
                "an implicit import catalog must not replace automatic discovery"
            );
            assert_eq!(observed_scope, SourceBackedRefreshScope::All);
            assert!(observed_covered_route_ids.is_empty());
            progress(CaptureSourceBackedDetailedRefreshProgress {
                progress: CaptureSourceBackedRefreshProgress {
                    phase: "discovering",
                    completed_sources: 0,
                    total_sources: 2,
                    current_source: None,
                    completed_records: None,
                    completed_bytes: None,
                    stage_duration: StdDuration::ZERO,
                    elapsed: StdDuration::ZERO,
                    certified_source_count: None,
                    certified_source_bytes: None,
                },
                current_source_progress: None,
            })?;
            progress(CaptureSourceBackedDetailedRefreshProgress {
                progress: CaptureSourceBackedRefreshProgress {
                    phase: "refreshing",
                    completed_sources: 1,
                    total_sources: 2,
                    current_source: Some("provider-wide-route".to_owned()),
                    completed_records: Some(11),
                    completed_bytes: Some(4_096),
                    stage_duration: StdDuration::ZERO,
                    elapsed: StdDuration::ZERO,
                    certified_source_count: None,
                    certified_source_bytes: None,
                },
                current_source_progress: None,
            })?;
            progress(CaptureSourceBackedDetailedRefreshProgress {
                progress: CaptureSourceBackedRefreshProgress {
                    phase: "verifying",
                    completed_sources: 2,
                    total_sources: 2,
                    current_source: None,
                    completed_records: None,
                    completed_bytes: None,
                    stage_duration: StdDuration::ZERO,
                    elapsed: StdDuration::ZERO,
                    certified_source_count: None,
                    certified_source_bytes: None,
                },
                current_source_progress: None,
            })?;
            Ok(test_publication("all-provider-generation"))
        },
    )
    .unwrap();
    drop(forge_writer);

    assert_eq!(provider_wide_calls, 1);
    assert_eq!(publication.generation_id, "all-provider-generation");
    assert_eq!(
        updates.into_inner().unwrap(),
        vec![
            ("discovering".to_owned(), 0, 0, None, None, None),
            ("discovering".to_owned(), 0, 2, None, None, None),
            (
                "refreshing".to_owned(),
                1,
                2,
                Some("provider-wide-route".to_owned()),
                Some(11),
                Some(4_096),
            ),
            ("verifying".to_owned(), 2, 2, None, None, None),
        ]
    );
}

#[test]
fn unsupported_only_refresh_publishes_empty_once_and_replays_as_a_no_op() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let report = DiscoveryReport {
        sources: vec![ProviderSource {
            provider: CaptureProvider::Warp,
            path: temp.path().join("missing-unsupported.sqlite"),
            exists: false,
            source_format: "warp_sqlite",
            source_kind: ProviderSourceKind::DetectionOnly,
            import_support: ProviderImportSupport::Unsupported,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Unsupported,
            unsupported_reason: Some("fixture has no executable source-backed route"),
        }],
        issues: Vec::new(),
    };
    let mut progress =
        |_: CaptureSourceBackedDetailedRefreshProgress| Ok::<(), SourceBackedRouteError>(());

    let first = refresh_all_provider_sources(
        &discovery,
        report.clone(),
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &BTreeSet::new(),
        &mut progress,
    )
    .unwrap();
    assert_eq!(first.scanned_routes, 0);
    assert_eq!(first.unsupported_routes, 1);
    assert_eq!(first.certified_source_count, 0);
    let empty_catalog = load_explicit_source_catalog_authority(&data_root).unwrap();
    assert_eq!(first.published_explicit_source_catalog, empty_catalog);
    let verified = VerifiedIndex::open(&index_root).unwrap();
    assert!(verified.manifest().sources.is_empty());
    assert!(verified.manifest().source_routes().is_empty());
    drop(verified);

    let replay = refresh_all_provider_sources(
        &discovery,
        report,
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &BTreeSet::new(),
        &mut progress,
    )
    .unwrap();
    assert_eq!(replay.generation_id, first.generation_id);
    assert_eq!(replay.scanned_routes, 0);
    assert_eq!(replay.unsupported_routes, 1);
    assert_eq!(replay.published_explicit_source_catalog, empty_catalog);

    let coordinator = CoreRefreshEngine::new();
    let request = coordinator.enqueue_periodic(&data_root).unwrap();
    let request_id = request_id(&request);
    let generation_id = replay.generation_id.clone();
    let run = coordinator
        .run_next_with(
            |_, _| Ok(replay),
            || Ok(Some(generation_id)),
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
    assert!(!run.failed);
    assert_eq!(
        run.job["published_explicit_source_catalog"],
        empty_catalog.to_json()
    );
    assert_eq!(
        run.job["receipt"]["published_explicit_source_catalog"],
        empty_catalog.to_json()
    );
    assert_eq!(
        coordinator.status(&request_id).unwrap()["request_state"],
        "published"
    );
}

#[test]
fn automatic_refresh_replaces_a_zstd_settings_generation() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let report = DiscoveryReport {
        sources: vec![ProviderSource {
            provider: CaptureProvider::Warp,
            path: temp.path().join("missing-unsupported.sqlite"),
            exists: false,
            source_format: "warp_sqlite",
            source_kind: ProviderSourceKind::DetectionOnly,
            import_support: ProviderImportSupport::Unsupported,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Unsupported,
            unsupported_reason: Some("fixture has no executable source-backed route"),
        }],
        issues: Vec::new(),
    };
    let mut progress =
        |_: CaptureSourceBackedDetailedRefreshProgress| Ok::<(), SourceBackedRouteError>(());
    let baseline = refresh_all_provider_sources(
        &discovery,
        report.clone(),
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &BTreeSet::new(),
        &mut progress,
    )
    .unwrap();
    let pointer_path = index_root.join("active-generation.json");
    let pointer_before = std::fs::read(&pointer_path).unwrap();
    let pointer: Value = serde_json::from_slice(&pointer_before).unwrap();
    let old_directory = pointer["active"]["directory"].as_str().unwrap();
    let old_generation_path = index_root.join("index-generations").join(old_directory);
    let meta_path = old_generation_path.join("meta.json");
    let mut meta: Value = serde_json::from_slice(&std::fs::read(&meta_path).unwrap()).unwrap();
    meta["index_settings"]["docstore_compression"] =
        Value::String("zstd(compression_level=1)".to_owned());
    meta["index_settings"]["docstore_blocksize"] = Value::from(64 * 1024);
    std::fs::write(&meta_path, serde_json::to_vec(&meta).unwrap()).unwrap();
    assert!(matches!(
        VerifiedIndex::open(&index_root),
        Err(IndexError::IndexSettingsMismatch(_))
    ));
    assert!(open_published_generation(&data_root).unwrap().is_none());

    let coordinator = CoreRefreshEngine::new();
    let queued = coordinator.enqueue_periodic(&data_root).unwrap();
    assert!(queued["previous_generation"].is_null());
    let run = coordinator
        .run_next_with(
            |_, _| {
                let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| {
                    Ok::<(), SourceBackedRouteError>(())
                };
                refresh_all_provider_sources(
                    &discovery,
                    report,
                    StdDuration::ZERO,
                    &data_root,
                    &index_root,
                    None,
                    SourceBackedRefreshScope::All,
                    &BTreeSet::new(),
                    &mut progress,
                )
            },
            || {
                Ok(open_published_generation(&data_root)?
                    .map(|index| index.generation_id().to_owned()))
            },
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("queued automatic rebuild");

    assert!(!run.failed);
    assert!(run.did_work);
    assert_eq!(run.job["published_generation"], baseline.generation_id);
    assert_ne!(std::fs::read(&pointer_path).unwrap(), pointer_before);
    assert!(!old_generation_path.exists());
    assert_eq!(
        pin_active_verified_generation(&data_root)
            .unwrap()
            .generation_id(),
        baseline.generation_id
    );
}

#[path = "codex_union_tests.rs"]
mod codex_union_tests;

#[path = "source_backed_refresh_coordinator_tests_request_coalescing.rs"]
mod request_coalescing_tests;

#[path = "source_backed_refresh_coordinator_tests_publication_lifecycle.rs"]
mod publication_lifecycle_tests;
