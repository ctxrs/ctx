use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Barrier,
};

use ctx_history_core::{
    derive_event_id, derive_session_id, CoreRecord, EventIdentityInput, NativeItemKey,
    NativeSessionKey, SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::{
    policy::AUTOMATIC_ROUTE_DELETION_GRACE_OBSERVATIONS, GenerationWriter, SourceRouteSnapshot,
};

use super::*;
use crate::semantic::dirty_source_routes::EventWatermark;

#[derive(Clone)]
struct MissingRouteFixture {
    route: SourceRouteIdentity,
    source: SourceKey,
    path: PathBuf,
}

fn fixture_route(root: &Path, byte: u8) -> MissingRouteFixture {
    MissingRouteFixture {
        route: SourceRouteIdentity::from_sha256(format!("{byte:02x}").repeat(32)).unwrap(),
        source: SourceKey::derive(
            "codex",
            "codex_session_jsonl",
            "session",
            1,
            SourceAnchor::CatalogLineage([byte; 32]),
        )
        .unwrap(),
        path: root.join(format!("route-{byte:02x}.jsonl")),
    }
}

fn fixture_record(fixture: &MissingRouteFixture) -> CoreRecord {
    let native_session = TypedKey::utf8(format!("session-{}", fixture.route.as_str())).unwrap();
    let session_key = NativeSessionKey::native_id("session", native_session).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source: &fixture.source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let native_item = NativeItemKey::native_id(
        "message",
        TypedKey::utf8(format!("event-{}", fixture.route.as_str())).unwrap(),
    )
    .unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &fixture.source,
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
        fixture.source.clone(),
        0,
        "message",
        "primary",
        true,
        "pending-missing-test-v1",
        format!("searchable route {}", fixture.route.as_str()),
    )
    .unwrap();
    record.role = Some("user".to_owned());
    record.validate_contract().unwrap();
    record
}

fn fixture_certificate(fixture: &MissingRouteFixture) -> CertifiedSource {
    let observation = SourceObservation::new(
        fixture.source.clone(),
        "regular-file-v1",
        vec![fixture.route.as_str().as_bytes()[0]],
    )
    .unwrap();
    CertifiedSource::certify(
        observation.clone(),
        observation,
        "pending-missing-test-v1",
        [0x5a; 32],
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 1,
            indexed_documents: 1,
            certified_bytes: 64,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap()
}

fn establish_fixture_generation(data_root: &Path, fixtures: &[MissingRouteFixture]) {
    let index_root = source_backed_index_root(data_root);
    let mut writer = GenerationWriter::open(&index_root, WriterOptions::default()).unwrap();
    let mut routes = Vec::new();
    for fixture in fixtures {
        fs::create_dir_all(fixture.path.parent().unwrap()).unwrap();
        fs::write(&fixture.path, b"present\n").unwrap();
        writer.begin_source(fixture.source.clone()).unwrap();
        writer.add_core_record(fixture_record(fixture)).unwrap();
        writer.certify_source(fixture_certificate(fixture)).unwrap();
        routes.push(
            SourceRouteSnapshot::present(fixture.route.clone(), vec![fixture.source.clone()])
                .unwrap(),
        );
    }
    writer.set_present_source_routes(routes).unwrap();
    writer.commit(|_| true).unwrap();
}

fn missing_fixture_executor(
    fixtures: Vec<MissingRouteFixture>,
    scans: Arc<Mutex<BTreeMap<SourceRouteIdentity, usize>>>,
    pause_next: Arc<AtomicBool>,
    entered: Arc<Barrier>,
    release: Arc<Barrier>,
) -> Arc<dyn SourceBackedRefreshExecutor> {
    let fixtures = fixtures
        .into_iter()
        .map(|fixture| (fixture.route.clone(), fixture))
        .collect::<BTreeMap<_, _>>();
    let observed_at = Arc::new(AtomicU64::new(100));
    Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
        if pause_next.swap(false, Ordering::SeqCst) {
            entered.wait();
            release.wait();
        }
        let selected = match &execution.scope {
            SourceBackedRefreshScope::All => fixtures
                .keys()
                .filter(|route| !execution.covered_route_ids.contains(*route))
                .cloned()
                .collect::<BTreeSet<_>>(),
            SourceBackedRefreshScope::Exact(routes) => routes.clone(),
        };
        let base = open_verified_index(execution.index_root)?;
        let base_routes = base
            .manifest()
            .source_routes()
            .iter()
            .map(|route| route.route_identity().clone())
            .collect::<BTreeSet<_>>();
        drop(base);
        let carried = base_routes.difference(&selected).cloned().collect();
        let mut writer = GenerationWriter::open(execution.index_root, WriterOptions::default())?;
        writer.set_source_route_plan(selected.clone(), carried)?;
        let mut present = Vec::new();
        let mut removed_source_count = 0_usize;
        for route in &selected {
            *scans.lock().unwrap().entry(route.clone()).or_default() += 1;
            let fixture = fixtures.get(route).expect("selected fixture route");
            writer.begin_source_route_stage(route.clone())?;
            if fixture.path.is_file() {
                writer.begin_source(fixture.source.clone())?;
                writer.add_core_record(fixture_record(fixture))?;
                writer.certify_source(fixture_certificate(fixture))?;
                writer.finish_source_route_stage(route)?;
                present.push(SourceRouteSnapshot::present(
                    route.clone(),
                    vec![fixture.source.clone()],
                )?);
            } else {
                let revalidate_path = fixture.path.clone();
                let outcome = writer.observe_certified_missing_route(
                    route.clone(),
                    observed_at.fetch_add(1, Ordering::SeqCst),
                    AUTOMATIC_ROUTE_DELETION_GRACE_OBSERVATIONS,
                    move || !revalidate_path.exists(),
                )?;
                if outcome.deleted() {
                    removed_source_count =
                        removed_source_count.saturating_add(outcome.retained_sources().len());
                }
                writer.finish_source_route_stage(route)?;
            }
        }
        writer.set_present_source_routes(present)?;
        let commit = writer.commit(|_| true)?;
        let current = SourceBackedRefreshCurrent::from_sources(
            &commit.manifest().sources,
            removed_source_count,
        )?;
        Ok(SourceBackedRefreshPublication {
            generation_id: commit.generation_id,
            published_explicit_source_catalog: execution
                .explicit_source_catalog
                .cloned()
                .expect("fixture catalog authority"),
            scanned_routes: selected.len(),
            unsupported_routes: 0,
            certified_source_count: current.source_count,
            certified_source_bytes: current.certified_source_bytes,
            current,
            timings: SourceBackedRefreshTimings::default(),
            selected_route_ids: selected
                .iter()
                .map(|route| route.as_str().to_owned())
                .collect(),
            successful_route_ids: selected
                .iter()
                .map(|route| route.as_str().to_owned())
                .collect(),
            source_failures: Vec::new(),
        })
    })
}

struct MissingFixtureHarness {
    data_root: PathBuf,
    fixtures: Vec<MissingRouteFixture>,
    coordinator: Arc<CoreRefreshEngine>,
    scans: Arc<Mutex<BTreeMap<SourceRouteIdentity, usize>>>,
    pause_next: Arc<AtomicBool>,
    entered: Arc<Barrier>,
    release: Arc<Barrier>,
}

impl MissingFixtureHarness {
    fn new(root: &Path) -> Self {
        let data_root = root.join("data");
        let provider_root = root.join("provider");
        let fixtures = vec![
            fixture_route(&provider_root, 0x31),
            fixture_route(&provider_root, 0x32),
        ];
        establish_fixture_generation(&data_root, &fixtures);
        let scans = Arc::new(Mutex::new(BTreeMap::new()));
        let pause_next = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let coordinator = Arc::new(CoreRefreshEngine::with_executor(missing_fixture_executor(
            fixtures.clone(),
            Arc::clone(&scans),
            Arc::clone(&pause_next),
            Arc::clone(&entered),
            Arc::clone(&release),
        )));
        let routes = fixtures.iter().map(|fixture| fixture.route.clone());
        coordinator.reconcile_watch_routes(routes, EventWatermark::new(1, 0), 0);
        let authority = load_explicit_source_catalog_authority(&data_root).unwrap();
        coordinator
            .handle_ipc_request(
                &data_root,
                &json!({
                    "op": SOURCE_REFRESH_REQUEST_OP,
                    "mode": "wait",
                    "explicit_source_catalog": authority.to_json(),
                }),
            )
            .unwrap()
            .expect("initial all-route request");
        let initial = coordinator.run_next(&data_root).unwrap();
        assert!(!initial.failed, "{:#}", initial.job);
        assert!(!coordinator.has_scheduled_route_work());
        scans.lock().unwrap().clear();
        Self {
            data_root,
            fixtures,
            coordinator,
            scans,
            pause_next,
            entered,
            release,
        }
    }

    fn missing(&self) -> &MissingRouteFixture {
        &self.fixtures[0]
    }

    fn healthy(&self) -> &MissingRouteFixture {
        &self.fixtures[1]
    }

    fn record_missing_event(&self) {
        fs::remove_file(&self.missing().path).unwrap();
        self.coordinator.record_watch_routes(
            [(self.missing().route.clone(), EventWatermark::new(1, 1))],
            0,
        );
        self.run_due();
    }

    fn run_due(&self) {
        assert!(self
            .coordinator
            .enqueue_next_dirty_route(&self.data_root, u64::MAX)
            .unwrap());
        let run = self.coordinator.run_next(&self.data_root).unwrap();
        assert!(!run.failed, "{:#}", run.job);
    }

    fn missing_count(&self) -> Option<u32> {
        let index = open_verified_index(&source_backed_index_root(&self.data_root)).unwrap();
        index
            .manifest()
            .source_route(&self.missing().route)
            .and_then(|route| route.missing_state())
            .map(|missing| missing.consecutive_missing().get())
    }
}

#[test]
fn idle_safety_rechecks_delete_only_the_pending_missing_route() {
    let temp = tempfile::tempdir().unwrap();
    let harness = MissingFixtureHarness::new(temp.path());
    assert_eq!(
        harness
            .coordinator
            .schedule_pending_missing_route_rechecks(
                &harness.data_root,
                EventWatermark::new(1, 0),
                0,
            )
            .unwrap(),
        0
    );
    assert!(harness.scans.lock().unwrap().is_empty());

    harness.record_missing_event();
    assert_eq!(harness.missing_count(), Some(1));
    for expected in 2..=AUTOMATIC_ROUTE_DELETION_GRACE_OBSERVATIONS {
        assert_eq!(
            harness
                .coordinator
                .schedule_pending_missing_route_rechecks(
                    &harness.data_root,
                    EventWatermark::new(1, 0),
                    0,
                )
                .unwrap(),
            1
        );
        harness.run_due();
        if expected < AUTOMATIC_ROUTE_DELETION_GRACE_OBSERVATIONS {
            assert_eq!(harness.missing_count(), Some(expected));
        }
    }

    let index = open_verified_index(&source_backed_index_root(&harness.data_root)).unwrap();
    assert!(index
        .manifest()
        .source_route(&harness.missing().route)
        .is_none());
    assert_eq!(index.document_count(), 1);
    assert_eq!(
        harness
            .coordinator
            .schedule_pending_missing_route_rechecks(
                &harness.data_root,
                EventWatermark::new(1, 0),
                0,
            )
            .unwrap(),
        0
    );
    let scans = harness.scans.lock().unwrap();
    assert_eq!(scans.get(&harness.missing().route), Some(&3));
    assert_eq!(scans.get(&harness.healthy().route), None);
}

#[test]
fn safety_recheck_recovers_pending_missing_state_after_engine_restart() {
    let temp = tempfile::tempdir().unwrap();
    let harness = MissingFixtureHarness::new(temp.path());
    harness.record_missing_event();
    assert_eq!(harness.missing_count(), Some(1));

    let restarted = CoreRefreshEngine::with_executor(missing_fixture_executor(
        harness.fixtures.clone(),
        Arc::clone(&harness.scans),
        Arc::clone(&harness.pause_next),
        Arc::clone(&harness.entered),
        Arc::clone(&harness.release),
    ));
    restarted.initialize_watch_route_authority(
        harness.fixtures.iter().map(|fixture| fixture.route.clone()),
    );

    assert!(restarted.pinned_core_publication().is_none());
    assert_eq!(
        restarted
            .schedule_pending_missing_route_rechecks(
                &harness.data_root,
                EventWatermark::new(2, 0),
                0,
            )
            .unwrap(),
        1
    );
    assert!(restarted
        .enqueue_next_dirty_route(&harness.data_root, u64::MAX)
        .unwrap());
    let run = restarted.run_next(&harness.data_root).unwrap();
    assert!(!run.failed, "{:#}", run.job);
    assert_eq!(harness.missing_count(), Some(2));
    assert_eq!(
        restarted
            .schedule_pending_missing_route_rechecks(
                &harness.data_root,
                EventWatermark::new(2, 0),
                0,
            )
            .unwrap(),
        1
    );
    assert!(restarted
        .enqueue_next_dirty_route(&harness.data_root, u64::MAX)
        .unwrap());
    let run = restarted.run_next(&harness.data_root).unwrap();
    assert!(!run.failed, "{:#}", run.job);
    assert_eq!(harness.missing_count(), None);
    assert_eq!(
        harness.scans.lock().unwrap().get(&harness.healthy().route),
        None
    );
}

#[test]
fn pending_missing_route_reappearance_resets_grace_before_threshold() {
    let temp = tempfile::tempdir().unwrap();
    let harness = MissingFixtureHarness::new(temp.path());
    harness.record_missing_event();
    assert_eq!(harness.missing_count(), Some(1));
    assert_eq!(
        harness
            .coordinator
            .schedule_pending_missing_route_rechecks(
                &harness.data_root,
                EventWatermark::new(1, 0),
                0,
            )
            .unwrap(),
        1
    );
    fs::write(&harness.missing().path, b"reappeared\n").unwrap();
    harness.run_due();

    assert_eq!(harness.missing_count(), None);
    assert_eq!(
        harness
            .coordinator
            .schedule_pending_missing_route_rechecks(
                &harness.data_root,
                EventWatermark::new(1, 0),
                0,
            )
            .unwrap(),
        0
    );
    let index = open_verified_index(&source_backed_index_root(&harness.data_root)).unwrap();
    assert_eq!(index.document_count(), 2);
    let scans = harness.scans.lock().unwrap();
    assert_eq!(scans.get(&harness.missing().route), Some(&2));
    assert_eq!(scans.get(&harness.healthy().route), None);
}

#[test]
fn watcher_and_manual_race_cannot_overadvance_or_delete_a_live_route() {
    let temp = tempfile::tempdir().unwrap();
    let harness = MissingFixtureHarness::new(temp.path());
    harness.record_missing_event();
    assert_eq!(
        harness
            .coordinator
            .schedule_pending_missing_route_rechecks(
                &harness.data_root,
                EventWatermark::new(1, 0),
                0,
            )
            .unwrap(),
        1
    );
    assert!(harness
        .coordinator
        .enqueue_next_dirty_route(&harness.data_root, u64::MAX)
        .unwrap());
    harness.pause_next.store(true, Ordering::SeqCst);

    std::thread::scope(|scope| {
        let coordinator = Arc::clone(&harness.coordinator);
        let data_root = harness.data_root.clone();
        scope.spawn(move || {
            assert!(!coordinator.run_next(&data_root).unwrap().failed);
        });
        harness.entered.wait();
        fs::write(&harness.missing().path, b"live-again\n").unwrap();
        harness.coordinator.record_watch_routes(
            [(harness.missing().route.clone(), EventWatermark::new(1, 2))],
            0,
        );
        let authority = load_explicit_source_catalog_authority(&harness.data_root).unwrap();
        harness
            .coordinator
            .handle_ipc_request(
                &harness.data_root,
                &json!({
                    "op": SOURCE_REFRESH_REQUEST_OP,
                    "mode": "wait",
                    "explicit_source_catalog": authority.to_json(),
                }),
            )
            .unwrap()
            .expect("manual race request");
        harness.release.wait();
    });

    let manual = harness
        .coordinator
        .run_next(&harness.data_root)
        .expect("manual successor");
    assert!(!manual.failed);
    assert_eq!(harness.missing_count(), None);
    assert!(!harness.coordinator.has_scheduled_route_work());
    let index = open_verified_index(&source_backed_index_root(&harness.data_root)).unwrap();
    assert_eq!(index.document_count(), 2);
}
