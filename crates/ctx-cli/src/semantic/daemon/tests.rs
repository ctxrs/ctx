use std::{
    cell::Cell,
    collections::BTreeSet,
    fs,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};

use super::*;
use crate::analytics::{ProviderRefreshCompletedV1, Surface};
use crate::semantic::dirty_source_routes::EventWatermark;
use crate::semantic::source_backed_refresh_coordinator::{
    source_backed_index_root, SourceBackedRefreshCurrent, SourceBackedRefreshExecution,
    SourceBackedRefreshExecutor, SourceBackedRefreshPublication, SourceBackedRefreshTimings,
};
use ctx_history_capture::{
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus, SourceBackedProviderRegistry, SourceBackedRefreshScope,
    SourceBackedRoute, SourceBackedRouteDriver, SourceBackedSelectorAuthority,
};
use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, CoreRecord,
    EventIdentityInput, NativeItemKey, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::{GenerationWriter, SourceRouteSnapshot, WriterOptions};
use sha2::{Digest, Sha256};

fn daemon_watch_test_catalog(path: PathBuf) -> SourceBackedWatchCatalog {
    let route = SourceBackedRoute::automatic(
        ProviderSource {
            provider: CaptureProvider::Codex,
            path,
            exists: true,
            source_format: "codex_history_jsonl",
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
        },
        SourceBackedSelectorAuthority::DiscoveredWinner,
        SourceBackedRouteDriver::new(|_| Ok(()), |_| false, |_| true),
    )
    .expect("build watcher test route");
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(route);
    registry.watch_catalog()
}

struct DurableFrontierFixture {
    data_root: PathBuf,
    provider_file: PathBuf,
    catalog: SourceBackedWatchCatalog,
    route: ctx_history_index::SourceRouteIdentity,
    executor: Arc<dyn SourceBackedRefreshExecutor>,
    writer_launches: Arc<AtomicUsize>,
}

impl DurableFrontierFixture {
    fn new(root: &Path) -> Result<Self> {
        let data_root = root.join("data");
        let provider_file = root.join("provider").join("history.jsonl");
        fs::create_dir_all(provider_file.parent().expect("provider parent"))?;
        fs::write(&provider_file, b"one\n")?;
        let catalog = daemon_watch_test_catalog(provider_file.clone());
        let route = catalog
            .route_ids()
            .next()
            .expect("frontier fixture route")
            .clone();
        let source = frontier_fixture_source();
        write_frontier_fixture_generation(
            &source_backed_index_root(&data_root),
            &route,
            &source,
            &provider_file,
            false,
        )?;

        let writer_launches = Arc::new(AtomicUsize::new(0));
        let launches = Arc::clone(&writer_launches);
        let refresh_route = route.clone();
        let refresh_source = source.clone();
        let refresh_file = provider_file.clone();
        let executor: Arc<dyn SourceBackedRefreshExecutor> =
            Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
                launches.fetch_add(1, Ordering::SeqCst);
                let certified_bytes = fs::metadata(&refresh_file)?.len();
                let generation_id = write_frontier_fixture_generation(
                    execution.index_root,
                    &refresh_route,
                    &refresh_source,
                    &refresh_file,
                    true,
                )?;
                Ok(SourceBackedRefreshPublication {
                    generation_id,
                    published_explicit_source_catalog: execution
                        .explicit_source_catalog
                        .cloned()
                        .expect("coordinator catalog authority"),
                    scanned_routes: 1,
                    unsupported_routes: 0,
                    certified_source_count: 1,
                    certified_source_bytes: certified_bytes,
                    current: SourceBackedRefreshCurrent {
                        source_count: 1,
                        indexed_documents: 1,
                        complete_records: 1,
                        retained_records: 1,
                        certified_source_bytes: certified_bytes,
                        ..SourceBackedRefreshCurrent::default()
                    },
                    timings: SourceBackedRefreshTimings::default(),
                    selected_route_ids: vec![refresh_route.as_str().to_owned()],
                    successful_route_ids: vec![refresh_route.as_str().to_owned()],
                    source_failures: Vec::new(),
                })
            });
        let fixture = Self {
            data_root,
            provider_file,
            catalog,
            route,
            executor,
            writer_launches,
        };
        fixture.persist_acknowledged_frontier()?;
        Ok(fixture)
    }

    fn persist_acknowledged_frontier(&self) -> Result<()> {
        let coordinator = CoreRefreshEngine::with_executor(Arc::clone(&self.executor));
        coordinator.initialize_watch_route_authority(self.catalog.route_ids().cloned());
        assert_eq!(
            coordinator.reconcile_route_freshness_frontier(
                &self.data_root,
                &self.catalog,
                EventWatermark::new(1, 0),
                0,
            )?,
            1
        );
        assert!(coordinator.enqueue_next_dirty_route(&self.data_root, u64::MAX)?);
        let run = coordinator
            .run_next(&self.data_root)
            .expect("frontier refresh");
        assert!(!run.failed, "{:#}", run.job);
        assert!(matches!(run.scope, SourceBackedRefreshScope::Exact(_)));
        assert!(!coordinator.has_scheduled_route_work());
        assert!(self
            .data_root
            .join("daemon")
            .join("route-freshness-frontier.json")
            .is_file());
        Ok(())
    }

    fn restarted_coordinator(&self) -> CoreRefreshEngine {
        CoreRefreshEngine::with_executor(Arc::clone(&self.executor))
    }
}

fn frontier_fixture_source() -> SourceKey {
    SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        SourceAnchor::CatalogLineage([0x5a; 32]),
    )
    .unwrap()
}

fn frontier_fixture_record(source: &SourceKey, body: String) -> CoreRecord {
    let native_session = NativeSessionKey::native_id(
        "session",
        TypedKey::utf8("durable-frontier-session").unwrap(),
    )
    .unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &native_session,
    })
    .unwrap();
    let native_item =
        NativeItemKey::native_id("message", TypedKey::utf8("durable-frontier-event").unwrap())
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
        "durable-frontier-test-v1",
        body,
    )
    .unwrap();
    record.role = Some("user".to_owned());
    record
}

fn frontier_fixture_certificate(source: &SourceKey, bytes: &[u8]) -> CertifiedSource {
    let revision: [u8; 32] = Sha256::digest(bytes).into();
    let observation =
        SourceObservation::new(source.clone(), "test-file-digest-v1", revision.to_vec()).unwrap();
    CertifiedSource::certify(
        observation.clone(),
        observation,
        "durable-frontier-test-v1",
        revision,
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 1,
            indexed_documents: 1,
            certified_bytes: u64::try_from(bytes.len()).unwrap(),
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap()
}

fn write_frontier_fixture_generation(
    index_root: &Path,
    route: &ctx_history_index::SourceRouteIdentity,
    source: &SourceKey,
    provider_file: &Path,
    route_staged: bool,
) -> Result<String> {
    let bytes = fs::read(provider_file)?;
    let mut writer = GenerationWriter::open(index_root, WriterOptions::default())?;
    if route_staged {
        writer.set_source_route_plan(BTreeSet::from([route.clone()]), BTreeSet::new())?;
        writer.begin_source_route_stage(route.clone())?;
    }
    writer.begin_source(source.clone())?;
    writer.add_core_record(frontier_fixture_record(
        source,
        String::from_utf8_lossy(&bytes).into_owned(),
    ))?;
    writer.certify_source(frontier_fixture_certificate(source, &bytes))?;
    if route_staged {
        writer.finish_source_route_stage(route)?;
    }
    writer.set_present_source_routes(vec![SourceRouteSnapshot::present(
        route.clone(),
        vec![source.clone()],
    )?])?;
    Ok(writer.commit(|_| true)?.generation_id)
}

fn manual_run() -> DaemonRunFactsV1 {
    DaemonRunFactsV1::new(DaemonStartModeV1::Manual, DaemonSupervisorV1::User, None)
}

fn runtime_names(events: &[PublicEventV1]) -> Vec<&'static str> {
    events
        .iter()
        .filter_map(|event| match event {
            PublicEventV1::RuntimeObservation(event) => Some(event.kind.name()),
            _ => None,
        })
        .collect()
}

#[test]
fn liveness_is_jittered_daily_and_never_a_loop_heartbeat() {
    assert_eq!(daemon_liveness_interval(0), DAEMON_LIVENESS_MIN_INTERVAL);
    assert!(
        daemon_liveness_interval(u64::MAX)
            < DAEMON_LIVENESS_MIN_INTERVAL + DAEMON_LIVENESS_JITTER_WINDOW
    );
    let started = Instant::now();
    let mut telemetry = DaemonTelemetry::new(manual_run(), started, 0);
    assert!(telemetry
        .liveness_events(started + StdDuration::from_secs(5))
        .is_empty());
    let due = started + DAEMON_LIVENESS_MIN_INTERVAL;
    assert_eq!(runtime_names(&telemetry.liveness_events(due)), ["liveness"]);
    assert!(telemetry.liveness_events(due).is_empty());
}

#[test]
fn idle_cycles_emit_first_then_flush_a_coalesced_transition() {
    let started = Instant::now();
    let mut telemetry = DaemonTelemetry::new(manual_run(), started, 0);
    let mut first = DaemonIteration::new(false, false, DaemonCycleStateV1::unknown());
    assert_eq!(
        runtime_names(&telemetry.observe_cycle(&mut first, StdDuration::from_millis(10))),
        ["cycle"]
    );
    for _ in 0..6 {
        let mut idle = DaemonIteration::new(false, false, DaemonCycleStateV1::unknown());
        assert!(telemetry
            .observe_cycle(&mut idle, StdDuration::from_millis(10))
            .is_empty());
    }
    assert_eq!(
        runtime_names(&telemetry.stopped_events(false, started + StdDuration::from_secs(1))),
        ["cycle", "stopped"]
    );
}

#[test]
fn scheduler_cycle_without_runtime_telemetry_preserves_provider_handoff() {
    let provider = PublicEventV1::ProviderRefreshCompleted(ProviderRefreshCompletedV1::new(
        Surface::Daemon,
        Outcome::Success,
        StdDuration::from_secs(1),
    ));
    let mut iteration = DaemonIteration::new(true, false, DaemonCycleStateV1::unknown())
        .with_provider_refresh_events(vec![provider]);
    let events = daemon_iteration_events(None, &mut iteration, StdDuration::from_secs(1));
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0],
        PublicEventV1::ProviderRefreshCompleted(_)
    ));
    assert!(iteration.provider_refresh_events.is_empty());
}

#[test]
fn safety_reconciliation_recovers_a_failed_startup_catalog_without_empty_authority() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let data_root = temp.path().join("data");
    let provider_root = temp.path().join("provider");
    let provider_file = provider_root.join("history.jsonl");
    fs::create_dir_all(&data_root)?;
    fs::create_dir_all(&provider_root)?;
    fs::write(&provider_file, b"{\"event\":1}\n")?;
    let catalog = daemon_watch_test_catalog(provider_file);
    let route = catalog
        .route_ids()
        .next()
        .expect("one watcher test route")
        .clone();
    let coordinator = CoreRefreshEngine::new();
    let wakeup = Arc::new(DaemonWakeup::default());
    let mut watch_runtime = DaemonWatchRuntime::new(Arc::clone(&wakeup));

    watch_runtime.reconcile_catalog_and_route_authority_with(
        &data_root,
        Some(&coordinator),
        WatchCatalogReconcileTrigger::Startup,
        false,
        |_| Err(anyhow!("injected startup catalog failure")),
        DaemonFileWatcher::start,
    );

    assert!(watch_runtime.file_watcher.is_some());
    assert!(watch_runtime.catalog.snapshot().is_none());
    assert!(!coordinator.watch_routes_initialized());
    assert!(!coordinator.has_scheduled_route_work());
    assert_eq!(
        super::super::daemon_wakeup::daemon_wakeup_report(&data_root)["status"],
        "degraded"
    );

    watch_runtime.reconcile_catalog_and_route_authority_with(
        &data_root,
        Some(&coordinator),
        WatchCatalogReconcileTrigger::SafetyTimeout,
        false,
        |_| Ok(catalog.clone()),
        |_, _, _| -> Result<DaemonFileWatcher> {
            panic!("an existing watcher must be updated through the shared catalog owner")
        },
    );

    let recovered = watch_runtime
        .catalog
        .snapshot()
        .expect("safety reconciliation publishes catalog authority");
    assert_eq!(recovered.route_ids().next(), Some(&route));
    assert!(coordinator.watch_routes_initialized());
    assert!(
        coordinator.has_scheduled_route_work(),
        "a route without a published frontier is cold and must be scheduled"
    );
    assert_eq!(
        super::super::daemon_wakeup::daemon_wakeup_report(&data_root)["status"],
        "active"
    );
    Ok(())
}

#[test]
fn safety_reconciliation_recreates_a_failed_startup_watcher_without_healthy_catalog_scans(
) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let data_root = temp.path().join("data");
    let provider_root = temp.path().join("provider");
    let provider_file = provider_root.join("history.jsonl");
    fs::create_dir_all(&data_root)?;
    fs::create_dir_all(&provider_root)?;
    fs::write(&provider_file, b"{\"event\":1}\n")?;
    let catalog = daemon_watch_test_catalog(provider_file);
    let coordinator = CoreRefreshEngine::new();
    let wakeup = Arc::new(DaemonWakeup::default());
    let mut watch_runtime = DaemonWatchRuntime::new(Arc::clone(&wakeup));
    let catalog_attempts = Cell::new(0_u64);
    let watcher_attempts = Cell::new(0_u64);

    watch_runtime.reconcile_catalog_and_route_authority_with(
        &data_root,
        Some(&coordinator),
        WatchCatalogReconcileTrigger::Startup,
        false,
        |_| {
            catalog_attempts.set(catalog_attempts.get().saturating_add(1));
            Ok(catalog.clone())
        },
        |_, _, _| {
            watcher_attempts.set(watcher_attempts.get().saturating_add(1));
            Err(anyhow!("injected startup watcher creation failure"))
        },
    );

    assert_eq!(catalog_attempts.get(), 1);
    assert_eq!(watcher_attempts.get(), 1);
    assert!(watch_runtime.file_watcher.is_none());
    assert!(watch_runtime.catalog.snapshot().is_some());
    assert!(coordinator.watch_routes_initialized());
    assert!(coordinator.has_scheduled_route_work());

    watch_runtime.reconcile_catalog_and_route_authority_with(
        &data_root,
        Some(&coordinator),
        WatchCatalogReconcileTrigger::SafetyTimeout,
        false,
        |_| {
            catalog_attempts.set(catalog_attempts.get().saturating_add(1));
            Ok(catalog.clone())
        },
        |path, wakeup, catalog| {
            watcher_attempts.set(watcher_attempts.get().saturating_add(1));
            DaemonFileWatcher::start(path, wakeup, catalog)
        },
    );

    assert_eq!(catalog_attempts.get(), 2);
    assert_eq!(watcher_attempts.get(), 2);
    assert!(watch_runtime.file_watcher.is_some());
    assert!(coordinator.has_scheduled_route_work());

    watch_runtime.reconcile_catalog_and_route_authority_with(
        &data_root,
        Some(&coordinator),
        WatchCatalogReconcileTrigger::SafetyTimeout,
        false,
        |_| -> Result<SourceBackedWatchCatalog> {
            panic!("healthy idle safety reconciliation must reuse catalog authority")
        },
        |_, _, _| -> Result<DaemonFileWatcher> {
            panic!("healthy idle safety reconciliation must reuse the watcher")
        },
    );

    assert_eq!(catalog_attempts.get(), 2);
    assert_eq!(watcher_attempts.get(), 2);
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn clean_forced_watcher_recovery_schedules_zero_healthy_route_scans() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let fixture = DurableFrontierFixture::new(temp.path())?;
    let baseline_writer_launches = fixture.writer_launches.load(Ordering::SeqCst);
    let coordinator = fixture.restarted_coordinator();
    let wakeup = Arc::new(DaemonWakeup::default());
    let mut watch_runtime = DaemonWatchRuntime::new(Arc::clone(&wakeup));
    watch_runtime.reconcile_catalog_and_route_authority_with(
        &fixture.data_root,
        Some(&coordinator),
        WatchCatalogReconcileTrigger::Startup,
        false,
        |_| Ok(fixture.catalog.clone()),
        DaemonFileWatcher::start,
    );
    assert!(coordinator.watch_routes_initialized());
    assert!(!coordinator.has_scheduled_route_work());
    assert!(!coordinator.enqueue_next_dirty_route(&fixture.data_root, u64::MAX)?);
    for _ in 0..3 {
        let response = coordinator
            .handle_ipc_request(
                &fixture.data_root,
                &json!({
                    "schema_version": 1,
                    "op": "source_refresh_request",
                    "mode": "background",
                    "operation": "refresh",
                }),
            )?
            .expect("background maintenance wake");
        assert_eq!(response["maintenance_wake"], true);
        assert!(!coordinator.has_pending_request());
    }
    assert_eq!(
        fixture.writer_launches.load(Ordering::SeqCst),
        baseline_writer_launches
    );

    watch_runtime.reconcile_catalog_and_route_authority_with(
        &fixture.data_root,
        Some(&coordinator),
        WatchCatalogReconcileTrigger::WatcherRecovery,
        true,
        |_| -> Result<SourceBackedWatchCatalog> {
            panic!("watcher rearm must use the shared catalog snapshot")
        },
        |_, _, _| -> Result<DaemonFileWatcher> {
            panic!("forced rearm must retain the current watcher owner")
        },
    );

    assert!(!coordinator.has_scheduled_route_work());
    assert!(!coordinator.enqueue_next_dirty_route(&fixture.data_root, u64::MAX)?);
    assert_eq!(
        fixture.writer_launches.load(Ordering::SeqCst),
        baseline_writer_launches
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn startup_reconciliation_schedules_a_route_changed_while_daemon_was_stopped() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let fixture = DurableFrontierFixture::new(temp.path())?;
    let baseline_writer_launches = fixture.writer_launches.load(Ordering::SeqCst);

    // No watcher/runtime is alive here: this mutation must be recovered from
    // the durable route frontier during the next production startup.
    fs::write(&fixture.provider_file, b"one\ntwo\n")?;
    let coordinator = fixture.restarted_coordinator();
    let wakeup = Arc::new(DaemonWakeup::default());
    let mut watch_runtime = DaemonWatchRuntime::new(Arc::clone(&wakeup));
    watch_runtime.reconcile_catalog_and_route_authority_with(
        &fixture.data_root,
        Some(&coordinator),
        WatchCatalogReconcileTrigger::Startup,
        false,
        |_| Ok(fixture.catalog.clone()),
        DaemonFileWatcher::start,
    );

    assert!(coordinator.has_scheduled_route_work());
    std::thread::sleep(StdDuration::from_millis(300));
    assert!(coordinator.enqueue_next_dirty_route(&fixture.data_root, u64::MAX)?);
    let run = coordinator
        .run_next(&fixture.data_root)
        .expect("changed route catch-up");
    assert!(!run.failed, "{:#}", run.job);
    assert!(matches!(
        run.scope,
        SourceBackedRefreshScope::Exact(ref routes) if routes == &BTreeSet::from([fixture.route.clone()])
    ));
    assert_eq!(
        fixture.writer_launches.load(Ordering::SeqCst),
        baseline_writer_launches.saturating_add(1)
    );
    assert!(!coordinator.has_scheduled_route_work());
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn forced_watcher_recovery_emits_a_route_mutated_during_rearm_overlap() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let data_root = temp.path().join("data");
    let provider_root = temp.path().join("provider");
    let provider_file = provider_root.join("history.jsonl");
    fs::create_dir_all(&data_root)?;
    fs::create_dir_all(&provider_root)?;
    fs::write(&provider_file, b"{\"event\":1}\n")?;
    let catalog = daemon_watch_test_catalog(provider_file.clone());
    let coordinator = CoreRefreshEngine::new();
    let wakeup = Arc::new(DaemonWakeup::default());
    let mut watch_runtime = DaemonWatchRuntime::new(Arc::clone(&wakeup));
    watch_runtime.reconcile_catalog_and_route_authority_with(
        &data_root,
        None,
        WatchCatalogReconcileTrigger::Startup,
        false,
        |_| Ok(catalog.clone()),
        DaemonFileWatcher::start,
    );
    coordinator.initialize_watch_route_authority(catalog.route_ids().cloned());
    assert!(!coordinator.has_scheduled_route_work());
    let watcher = watch_runtime
        .file_watcher
        .as_mut()
        .expect("startup watcher");
    let mutation_observed = Arc::new(AtomicBool::new(false));
    let hook_observed = Arc::clone(&mutation_observed);
    let hook_root = provider_root.clone();
    let hook_file = provider_file.clone();
    watcher.install_rearm_overlap_hook(move |watched| {
        if watched == hook_root && !hook_observed.swap(true, Ordering::SeqCst) {
            fs::write(&hook_file, b"{\"event\":2}\n")
                .expect("mutate provider source during forced-rearm overlap");
        }
    });

    watch_runtime.reconcile_catalog_and_route_authority_with(
        &data_root,
        Some(&coordinator),
        WatchCatalogReconcileTrigger::WatcherRecovery,
        true,
        |_| -> Result<SourceBackedWatchCatalog> {
            panic!("watcher rearm must use the shared catalog snapshot")
        },
        |_, _, _| -> Result<DaemonFileWatcher> {
            panic!("forced rearm must retain the current watcher")
        },
    );

    assert!(mutation_observed.load(Ordering::SeqCst));
    assert_eq!(fs::read(provider_file)?, b"{\"event\":2}\n");
    assert!(coordinator.watch_routes_initialized());
    assert!(!coordinator.has_scheduled_route_work());
    let observed = wakeup.wait(StdDuration::from_secs(3));
    assert!(observed.filesystem, "overlap mutation did not wake watcher");
    assert_eq!(observed.source_watch.routes.len(), 1);
    coordinator.record_watch_routes(observed.source_watch.routes, source_route_ledger_now_ms());
    assert!(coordinator.has_scheduled_route_work());
    Ok(())
}

#[cfg(unix)]
#[test]
fn released_daemon_service_artifacts_are_removed_after_forced_shutdown() -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let root = tempfile::tempdir()?;
    let daemon_root = super::super::paths_status::daemon_root_path(root.path());
    fs::create_dir_all(&daemon_root)?;
    fs::set_permissions(&daemon_root, fs::Permissions::from_mode(0o700))?;
    for (service, name) in [
        (DaemonIpcService::SemanticQuery, "query.sock"),
        (DaemonIpcService::SourceRefresh, "source-refresh.sock"),
    ] {
        let socket_path = daemon_root.join(name);
        fs::write(&socket_path, b"stale")?;
        super::super::query_service::write_daemon_service_endpoint(
            root.path(),
            service,
            &DaemonQueryEndpoint::Unix {
                path: socket_path.clone(),
                token: format!("{name}-token-00000000000000000000000000000000"),
            },
        )?;
        assert!(socket_path.exists());
        assert!(daemon_service_endpoint_path(root.path(), service).exists());
    }

    remove_released_daemon_service_artifacts(root.path())?;

    assert!(!daemon_root.join("query.sock").exists());
    assert!(!daemon_root.join("source-refresh.sock").exists());
    assert!(!daemon_service_endpoint_path(root.path(), DaemonIpcService::SemanticQuery).exists());
    assert!(!daemon_service_endpoint_path(root.path(), DaemonIpcService::SourceRefresh).exists());
    Ok(())
}

#[test]
fn only_enabled_long_lived_daemon_uses_upgrade_scheduler() {
    assert!(daemon_should_schedule_auto_upgrade(true, DaemonMode::Full));
    assert!(!daemon_should_schedule_auto_upgrade(
        false,
        DaemonMode::Full
    ));
    assert!(!daemon_should_schedule_auto_upgrade(
        true,
        DaemonMode::SourceRefreshOnly
    ));
}

#[test]
fn explicit_finite_idle_exit_remains_due_with_retry_and_refresh_pending() {
    let mut runtime = DaemonRuntime::default();
    runtime.history_retry.consecutive_failures = 1;
    let retry_due = super::super::daemon_scheduler::daemon_retry_due(&runtime);
    assert!(retry_due);

    let coordinator = CoreRefreshEngine::new();
    coordinator.enqueue_for_test(None);
    let source_refresh_pending = coordinator.has_pending_request();
    assert!(source_refresh_pending);

    assert!(daemon_should_attempt_finite_idle_shutdown(
        Some(StdDuration::ZERO),
        Some(Instant::now()),
        retry_due,
        source_refresh_pending,
    ));
}

#[test]
fn persistent_default_never_has_a_finite_idle_exit() {
    assert!(!daemon_should_attempt_finite_idle_shutdown(
        None,
        Some(Instant::now()),
        true,
        true,
    ));
}

#[test]
fn due_consumer_retry_wait_loop_blocks_and_wakes_when_query_becomes_idle() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let generation = ctx_history_index::GenerationWriter::open(
        super::super::source_backed_refresh_coordinator::source_backed_index_root(temp.path()),
        ctx_history_index::WriterOptions::default(),
    )?
    .commit(|_| true)?
    .generation_id;
    let wakeup = Arc::new(super::super::daemon_wakeup::DaemonWakeup::default());
    let activity = Arc::new(
        super::super::query_service::DaemonQueryActivity::with_idle_wakeup(Arc::clone(&wakeup)),
    );
    let request = activity.begin_request().expect("foreground query");
    let mut runtime = DaemonRuntime::default();
    runtime.pro_retry.consecutive_failures = 1;
    runtime.pro_retry.retry_not_before = Some(Instant::now() - StdDuration::from_secs(1));
    runtime.pro_retry.retry_not_before_at_ms = Some(utc_now().timestamp_millis() - 1);
    runtime.history_retry.consecutive_failures = 1;
    runtime.history_retry.retry_not_before = Some(Instant::now() - StdDuration::from_secs(1));
    runtime.history_retry.retry_not_before_at_ms = Some(utc_now().timestamp_millis() - 1);

    let deferred = run_daemon_scheduler_cycle_with_activity(
        &test_daemon_run_args(),
        temp.path(),
        &mut runtime,
        None,
        false,
        Some(activity.as_ref()),
        None,
    )?;
    assert!(!deferred.did_work);
    assert_eq!(runtime.pro_retry.retry_after_ms(), Some(0));

    let now = Instant::now();
    let wait_for = daemon_wait_duration(
        &runtime,
        None,
        now + StdDuration::from_secs(30),
        None,
        None,
        now,
    );
    assert!(wait_for > StdDuration::ZERO);
    assert!(wait_for <= super::super::daemon_scheduler::DAEMON_CONSUMER_RETRY_QUERY_GRACE);

    drop(request);
    let wake = wakeup.wait(wait_for);
    assert!(
        !wake.timed_out,
        "query-idle transition must wake the daemon"
    );

    let retried = run_daemon_scheduler_cycle_with_activity(
        &test_daemon_run_args(),
        temp.path(),
        &mut runtime,
        None,
        false,
        Some(activity.as_ref()),
        None,
    )?;
    assert!(!retried.failed);
    assert!(runtime.consumer_retry_deferral.retry_at.is_none());
    let status = super::super::source_backed_pro_catch_up::read_status_json(temp.path())
        .expect("Pro retry attempt");
    assert_eq!(status["core_generation_id"], generation);
    assert_eq!(status["attempts"], 1);
    Ok(())
}

#[test]
fn continuous_query_wait_loop_reaches_consumer_retry_fairness_deadline() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let generation = ctx_history_index::GenerationWriter::open(
        super::super::source_backed_refresh_coordinator::source_backed_index_root(temp.path()),
        ctx_history_index::WriterOptions::default(),
    )?
    .commit(|_| true)?
    .generation_id;
    let wakeup = Arc::new(super::super::daemon_wakeup::DaemonWakeup::default());
    let activity = Arc::new(
        super::super::query_service::DaemonQueryActivity::with_idle_wakeup(Arc::clone(&wakeup)),
    );
    let _request = activity.begin_request().expect("continuous query");
    let mut runtime = DaemonRuntime::default();
    runtime.pro_retry.consecutive_failures = 1;
    runtime.pro_retry.retry_not_before = Some(Instant::now() - StdDuration::from_secs(1));
    runtime.pro_retry.retry_not_before_at_ms = Some(utc_now().timestamp_millis() - 1);

    let deferred = run_daemon_scheduler_cycle_with_activity(
        &test_daemon_run_args(),
        temp.path(),
        &mut runtime,
        None,
        false,
        Some(activity.as_ref()),
        None,
    )?;
    assert!(!deferred.did_work);
    assert!(runtime.consumer_retry_deferral.retry_at.is_some());

    let deadline = Instant::now();
    runtime.consumer_retry_deferral.retry_at = Some(deadline);
    let wait_for = daemon_wait_duration(
        &runtime,
        None,
        deadline + StdDuration::from_secs(30),
        None,
        None,
        deadline,
    );
    assert_eq!(wait_for, StdDuration::ZERO);
    assert!(wakeup.wait(wait_for).timed_out);

    let fair = run_daemon_scheduler_cycle_with_activity(
        &test_daemon_run_args(),
        temp.path(),
        &mut runtime,
        None,
        false,
        Some(activity.as_ref()),
        None,
    )?;
    assert!(!fair.failed);
    assert!(runtime.consumer_retry_deferral.retry_at.is_none());
    let status = super::super::source_backed_pro_catch_up::read_status_json(temp.path())
        .expect("fair Pro retry attempt");
    assert_eq!(status["core_generation_id"], generation);
    assert_eq!(status["attempts"], 1);
    Ok(())
}

fn test_daemon_run_args() -> DaemonRunArgs {
    DaemonRunArgs {
        foreground: false,
        idle_exit_seconds: None,
        loop_interval_seconds: None,
        max_chunks: None,
        max_seconds: None,
        force: false,
        start_mode: None,
        trigger_command: None,
        format: crate::output::JsonOutputFormat::Json,
    }
}

#[test]
fn semantic_runtime_is_requested_only_for_supported_full_daemons() {
    let mut config = AppConfig::default();
    config.search.semantic = Some(true);
    config.daemon.mode = DaemonMode::Full;

    assert!(daemon_semantic_runtime_requested(&config, true));
    assert!(!daemon_semantic_runtime_requested(&config, false));

    config.search.semantic = Some(false);
    assert!(!daemon_semantic_runtime_requested(&config, true));

    config.search.semantic = Some(true);
    config.daemon.mode = DaemonMode::SourceRefreshOnly;
    assert!(!daemon_semantic_runtime_requested(&config, true));
}

#[test]
fn source_refresh_only_scheduler_runs_no_unrelated_job() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let _hooks = install_daemon_test_job_hooks(DaemonTestJobHooks {
        calls: calls.clone(),
        semantic_index: Some(json!({"status": "completed"})),
    });
    let mut runtime = DaemonRuntime {
        config: AppConfig {
            daemon: crate::config::DaemonConfig {
                enabled: true,
                mode: DaemonMode::SourceRefreshOnly,
            },
            search: crate::config::SearchConfig {
                semantic: Some(true),
            },
            ..AppConfig::default()
        },
        ..DaemonRuntime::default()
    };

    let iteration = run_daemon_scheduler_cycle_with_activity(
        &test_daemon_run_args(),
        temp.path(),
        &mut runtime,
        None,
        true,
        None,
        None,
    )?;

    assert!(!iteration.did_work);
    assert!(!iteration.failed);
    assert!(calls.borrow().is_empty());
    assert!(!super::super::paths_status::daemon_semantic_job_path(temp.path()).exists());
    Ok(())
}

#[test]
fn source_refresh_only_and_full_modes_share_the_same_refresh_path() -> Result<()> {
    use super::super::source_backed_refresh_coordinator::{
        SourceBackedRefreshCurrent, SourceBackedRefreshExecution, SourceBackedRefreshPublication,
        SourceBackedRefreshTimings,
    };

    fn run_mode(daemon_mode: DaemonMode, calls: Arc<AtomicUsize>) -> Result<serde_json::Value> {
        let temp = tempfile::tempdir()?;
        let coordinator = CoreRefreshEngine::with_executor(Arc::new(
            move |execution: SourceBackedRefreshExecution<'_>| {
                calls.fetch_add(1, Ordering::SeqCst);
                execution.report_progress(
                    "refreshing",
                    0,
                    1,
                    Some("all-providers".to_owned()),
                    Some(12),
                    Some(4_096),
                )?;
                execution.report_progress("verifying", 1, 1, None, None, None)?;
                let writer = ctx_history_index::GenerationWriter::open(
                    execution.index_root,
                    ctx_history_index::WriterOptions::default(),
                )?;
                let receipt = writer.commit(|_| true)?;
                Ok(SourceBackedRefreshPublication {
                    selected_route_ids: Vec::new(),
                    successful_route_ids: Vec::new(),
                    source_failures: Vec::new(),
                    generation_id: receipt.generation_id,
                    published_explicit_source_catalog:
                        crate::commands::import::load_explicit_source_catalog_authority(
                            execution.data_root,
                        )?,
                    scanned_routes: 1,
                    unsupported_routes: 0,
                    certified_source_count: 0,
                    certified_source_bytes: 0,
                    current: SourceBackedRefreshCurrent::default(),
                    timings: SourceBackedRefreshTimings {
                        discovery_us: 7,
                        scan_stage_us: 11,
                        commit_us: 13,
                    },
                })
            },
        ));
        coordinator.enqueue_for_test(None);
        let mut config = AppConfig::default();
        config.daemon.mode = daemon_mode;
        let mut runtime = DaemonRuntime {
            config,
            ..DaemonRuntime::default()
        };

        let iteration = run_daemon_scheduler_cycle_with_activity(
            &test_daemon_run_args(),
            temp.path(),
            &mut runtime,
            None,
            false,
            None,
            Some(&coordinator),
        )?;
        assert!(!iteration.failed);
        read_daemon_job_status(&daemon_core_refresh_job_path(temp.path()))
            .ok_or_else(|| anyhow!("source refresh job was not persisted"))
    }

    let source_only_calls = Arc::new(AtomicUsize::new(0));
    let full_calls = Arc::new(AtomicUsize::new(0));
    let source_only = run_mode(DaemonMode::SourceRefreshOnly, source_only_calls.clone())?;
    let full = run_mode(DaemonMode::Full, full_calls.clone())?;

    assert_eq!(source_only_calls.load(Ordering::SeqCst), 1);
    assert_eq!(full_calls.load(Ordering::SeqCst), 1);
    for key in [
        "status",
        "request_state",
        "owner",
        "kind",
        "source_count",
        "scanned_routes",
        "unsupported_routes",
        "certified_source_count",
        "certified_source_bytes",
        "published_explicit_source_catalog",
        "receipt",
    ] {
        assert_eq!(source_only[key], full[key], "{key}");
    }
    for key in ["discovery", "scan_stage", "commit"] {
        assert_eq!(
            source_only["timings_us"][key], full["timings_us"][key],
            "timings_us.{key}"
        );
    }
    for job in [&source_only, &full] {
        assert!(
            job["timings_us"]["publication_probe"]
                .as_u64()
                .is_some_and(|duration| duration > 0),
            "{job:#}"
        );
    }
    Ok(())
}

#[test]
fn one_scheduler_cycle_publishes_core_before_consumer_jobs() -> Result<()> {
    use super::super::dirty_source_routes::EventWatermark;
    use super::super::source_backed_refresh_coordinator::{
        SourceBackedRefreshCurrent, SourceBackedRefreshExecution, SourceBackedRefreshPublication,
        SourceBackedRefreshTimings,
    };

    let temp = tempfile::tempdir()?;
    let route = ctx_history_index::SourceRouteIdentity::from_sha256("42".repeat(32))?;
    let refresh_calls = Arc::new(AtomicUsize::new(0));
    let executor_calls = refresh_calls.clone();
    let executor_route = route.clone();
    let coordinator = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            executor_calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                execution.scope,
                ctx_history_capture::SourceBackedRefreshScope::All
            );
            let writer = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                ctx_history_index::WriterOptions::default(),
            )?;
            let receipt = writer.commit(|_| true)?;
            Ok(SourceBackedRefreshPublication {
                selected_route_ids: vec![executor_route.as_str().to_owned()],
                successful_route_ids: vec![executor_route.as_str().to_owned()],
                source_failures: Vec::new(),
                generation_id: receipt.generation_id,
                published_explicit_source_catalog:
                    crate::commands::import::load_explicit_source_catalog_authority(
                        execution.data_root,
                    )?,
                scanned_routes: 1,
                unsupported_routes: 0,
                certified_source_count: 0,
                certified_source_bytes: 0,
                current: SourceBackedRefreshCurrent::default(),
                timings: SourceBackedRefreshTimings::default(),
            })
        },
    ));
    assert!(!coordinator.has_pending_request());
    coordinator.reconcile_watch_routes([route], EventWatermark::new(1, 0), 0);
    assert!(coordinator.has_scheduled_route_work());
    let mut runtime = DaemonRuntime::default();

    let iteration = run_daemon_scheduler_cycle_with_activity(
        &test_daemon_run_args(),
        temp.path(),
        &mut runtime,
        None,
        true,
        None,
        Some(&coordinator),
    )?;

    assert!(iteration.did_work);
    assert!(!iteration.failed);
    assert!(iteration.continue_immediately);
    assert_eq!(refresh_calls.load(Ordering::SeqCst), 1);
    let job = read_daemon_job_status(&daemon_core_refresh_job_path(temp.path()))
        .ok_or_else(|| anyhow!("periodic source refresh job was not persisted"))?;
    assert_eq!(job["status"], "completed");
    assert_eq!(job["daemon_mode"], "full");
    assert_eq!(job["trigger"], "periodic");
    assert_eq!(job["trigger_provenance"], "daemon_scheduler");
    assert_eq!(
        job["published_explicit_source_catalog"],
        job["receipt"]["published_explicit_source_catalog"]
    );
    assert!(job.get("pro_projection").is_none());
    assert!(job.get("semantic_projection").is_none());
    assert!(!super::super::paths_status::daemon_semantic_job_path(temp.path()).exists());
    let published_generation = job["published_generation"]
        .as_str()
        .ok_or_else(|| anyhow!("Core generation was not published"))?;
    assert_eq!(
        runtime.sidecar_drain.generation.as_deref(),
        Some(published_generation)
    );
    assert!(temp
        .path()
        .join("search")
        .join("lexical")
        .join("active-generation.json")
        .is_file());
    Ok(())
}

#[test]
fn source_refresh_only_status_exposes_runtime_and_certified_refresh_identity() -> Result<()> {
    let _env_lock = crate::config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir()?;
    fs::write(
        temp.path().join(CONFIG_FILE),
        "[daemon]\nmode = \"source-refresh-only\"\n",
    )?;
    let lock = DaemonLock::acquire(temp.path())?.expect("daemon lock");
    let now = utc_now().timestamp_millis();
    write_daemon_status(
        temp.path(),
        &json!({
            "schema_version": 1,
            "status": "running",
            "pid": process::id(),
            "started_at_ms": now,
            "heartbeat_at_ms": now,
            "start_mode": "auto",
            "trigger_command": "search",
            "semantic_runtime_active": false,
            "config_reload": {
                "status": "applied",
                "requested": {
                    "daemon_enabled": true,
                    "daemon_mode": "source-refresh-only",
                    "semantic_enabled": false,
                },
                "applied": {
                    "daemon_enabled": true,
                    "daemon_mode": "source-refresh-only",
                    "semantic_enabled": false,
                },
            },
        }),
    )?;
    super::super::paths_status::write_private_json_file(
        &super::super::paths_status::daemon_root_path(temp.path())
            .join("source-refresh-endpoint.json"),
        &json!({
            "schema_version": 1,
            "transport": "unix",
            "path": temp.path().join("daemon/source-refresh.sock"),
            "token": "must-not-appear-in-status",
            "pid": process::id(),
        }),
    )?;
    super::super::paths_status::write_daemon_job_status(
        &daemon_core_refresh_job_path(temp.path()),
        &json!({
            "status": "completed",
            "daemon_mode": "source-refresh-only",
            "trigger": "search",
            "trigger_provenance": "autostart",
            "certified_source_count": 4,
            "certified_source_bytes": 8192,
            "timings_us": {
                "discovery": 5,
                "scan_stage": 7,
                "commit": 11,
            },
        }),
    )?;

    let report = daemon_report(temp.path());

    assert_eq!(report["mode"], "source-refresh-only");
    assert_eq!(report["live_pid"], process::id());
    assert_eq!(report["trigger_command"], "search");
    assert_eq!(report["trigger_provenance"], "autostart");
    assert_eq!(report["lock_identity"]["active"], true);
    assert!(report["lock_identity"]["owner_id"]
        .as_str()
        .is_some_and(|owner| !owner.is_empty()));
    assert_eq!(report["core_refresh_endpoint"]["available"], true);
    assert_eq!(report["core_refresh_endpoint"]["owner_pid"], process::id());
    assert!(!report.to_string().contains("must-not-appear-in-status"));
    assert_eq!(
        report["jobs"]["semantic_index"]["reason"],
        "daemon_mode_source_refresh_only"
    );
    assert_eq!(report["jobs"]["core_refresh"]["certified_source_count"], 4);
    assert_eq!(
        report["jobs"]["core_refresh"]["certified_source_bytes"],
        8192
    );
    for stage in ["discovery", "scan_stage", "commit"] {
        assert!(
            report["jobs"]["core_refresh"]["timings_us"][stage]
                .as_u64()
                .is_some_and(|duration| duration > 0),
            "{stage}"
        );
    }
    drop(lock);
    Ok(())
}

#[test]
fn post_lock_initialization_failure_retains_restart_intent() -> Result<()> {
    let root = tempfile::tempdir()?;
    super::super::daemon_autostart::write_daemon_restart_request(
        root.path(),
        DaemonTriggerCommandArg::Search,
        "ua_01890f3e-2c80-7000-8000-00000000000b",
    )?;
    let failure_marker = root.path().join(".fail-daemon-before-ready-for-test");
    fs::write(&failure_marker, b"fail")?;

    let error = run_daemon_inner(
        DaemonRunArgs {
            foreground: false,
            idle_exit_seconds: None,
            loop_interval_seconds: None,
            max_chunks: None,
            max_seconds: None,
            force: false,
            start_mode: Some(DaemonStartModeArg::Auto),
            trigger_command: Some(DaemonTriggerCommandArg::Search),
            format: crate::output::JsonOutputFormat::Json,
        },
        root.path(),
        &AppConfig::default(),
    )
    .expect_err("the injected post-lock initialization failure must surface");

    assert!(error
        .to_string()
        .contains("injected daemon failure before readiness"));
    assert!(super::super::daemon_autostart::read_daemon_restart_request(root.path()).is_some());
    Ok(())
}

#[test]
fn telemetry_policy_is_reloaded_and_failures_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join(CONFIG_FILE);

    fs::write(&config_path, "analytics.enabled = true\n").unwrap();
    assert!(reload_daemon_analytics_config(root.path()).is_some());

    fs::write(&config_path, "analytics.enabled = false\n").unwrap();
    assert!(reload_daemon_analytics_config(root.path()).is_none());

    fs::write(&config_path, "not valid config\n").unwrap();
    assert!(reload_daemon_analytics_config(root.path()).is_none());

    let event = runtime_event(
        DaemonRuntimeObservationV1::ready(manual_run()),
        Outcome::Success,
        StdDuration::ZERO,
    );
    send_daemon_events(root.path(), &[event]);
    assert!(!crate::identity::install_path(root.path()).exists());
}
