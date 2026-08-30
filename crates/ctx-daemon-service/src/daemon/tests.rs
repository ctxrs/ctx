#[path = "semantic_activation_backoff_tests.rs"]
mod semantic_activation_backoff;

#[path = "tests/startup_recovery.rs"]
mod startup_recovery;

use std::{
    collections::BTreeSet,
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

#[cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt;
#[cfg(target_os = "linux")]
use std::sync::atomic::AtomicBool;

use super::*;
use crate::source_backed_refresh_coordinator::{
    source_backed_index_root, SourceBackedRefreshCurrent, SourceBackedRefreshExecution,
    SourceBackedRefreshExecutor, SourceBackedRefreshPublication, SourceBackedRefreshRouteResult,
    SourceBackedRefreshTimings,
};
use crate::{
    analytics::{Outcome, ProviderRefreshCompletedV1, Surface},
    query_service::{
        daemon_service_endpoint_path, daemon_source_refresh_request, DaemonIpcService,
    },
};
use ctx_history_capture::{
    SourceBackedProviderRegistry, SourceBackedRefreshScope, SourceBackedRoute,
    SourceBackedRouteDriver, SourceBackedSelectorAuthority,
};
use ctx_history_capture_model::{
    provider_source_config_digest, ProviderCatalogSupport, ProviderImportSupport,
    ProviderRootDefinition, ProviderSource, ProviderSourceKind, ProviderSourceStatus,
};
use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, CoreRecord,
    EventIdentityInput, NativeItemKey, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::{
    AppliedProviderRoot, GenerationWriter, SourceRouteSnapshot, WriterOptions,
};
use ctx_history_refresh::EventWatermark;

#[test]
fn scheduler_source_refresh_selection_borrows_without_refcount_churn() {
    let source_refresh = Arc::new(crate::source_backed_refresh_adapter::refresh_engine(
        &crate::test_support::CONFIG,
    ));
    let stable_coordinator = Some(Arc::clone(&source_refresh));
    let strong_count = Arc::strong_count(&source_refresh);

    let selected = daemon_scheduler_source_refresh(&stable_coordinator)
        .expect("stable coordinator must be selectable");

    assert!(std::ptr::eq(selected, source_refresh.as_ref()));
    assert_eq!(Arc::strong_count(&source_refresh), strong_count);
}

#[cfg(unix)]
#[test]
fn completed_ipc_listener_is_a_fatal_daemon_health_failure() -> Result<()> {
    let root = tempfile::tempdir()?;
    let wakeup = Arc::new(crate::daemon_wakeup::DaemonWakeup::default());
    let handler = crate::query_service::ctx_authenticated_request_handler(
        root.path(),
        SharedSemanticRuntime::default(),
        Arc::new(crate::source_backed_refresh_adapter::refresh_engine(
            &crate::test_support::CONFIG,
        )),
        wakeup,
        &crate::test_support::CONFIG,
    );
    let service = crate::query_service::start_daemon_source_refresh_service_with_request_timeout(
        root.path(),
        handler,
        StdDuration::from_millis(100),
    )?;
    assert!(daemon_service_endpoint_path(root.path(), DaemonIpcService::SourceRefresh).exists());

    service.terminate_listener_for_test();
    let deadline = Instant::now() + StdDuration::from_secs(2);
    while !service.listener_finished() && Instant::now() < deadline {
        std::thread::sleep(StdDuration::from_millis(5));
    }

    assert!(service.listener_finished(), "listener thread did not exit");
    assert!(
        !daemon_service_endpoint_path(root.path(), DaemonIpcService::SourceRefresh).exists(),
        "dead listener retained its published endpoint"
    );
    let error = ensure_daemon_ipc_services_healthy(None, Some(&service))
        .expect_err("daemon must fail when a retained IPC listener has exited");
    assert!(
        error
            .to_string()
            .contains("source-refresh IPC listener exited unexpectedly"),
        "{error:#}"
    );
    Ok(())
}

fn daemon_watch_test_catalog(path: PathBuf) -> SourceBackedWatchCatalog {
    daemon_watch_test_catalog_for_paths([path])
}

fn daemon_watch_test_catalog_for_paths(
    paths: impl IntoIterator<Item = PathBuf>,
) -> SourceBackedWatchCatalog {
    let mut registry = SourceBackedProviderRegistry::new();
    for (index, path) in paths.into_iter().enumerate() {
        let (provider, source_format) = if index == 0 {
            (CaptureProvider::Codex, "codex_history_jsonl")
        } else {
            (CaptureProvider::Claude, "claude_projects_jsonl_tree")
        };
        let route = SourceBackedRoute::automatic(
            ProviderSource {
                provider,
                path,
                exists: true,
                source_format,
                source_kind: ProviderSourceKind::NativeHistory,
                import_support: ProviderImportSupport::Native,
                catalog_support: ProviderCatalogSupport::None,
                status: ProviderSourceStatus::Available,
                unsupported_reason: None,
                route_provenance: Default::default(),
            },
            SourceBackedSelectorAuthority::DiscoveredWinner,
            SourceBackedRouteDriver::new(|_| Ok(()), |_| false, |_| true),
        )
        .expect("build watcher test route");
        registry.register(route);
    }
    registry.watch_catalog()
}

fn daemon_watch_test_catalog_with_provider_root_group(
    path: PathBuf,
    group: &str,
    automatic_provider_discovery: bool,
) -> SourceBackedWatchCatalog {
    let mut registry = SourceBackedProviderRegistry::new();
    let route = SourceBackedRoute::automatic(
        ProviderSource {
            provider: CaptureProvider::Codex,
            path: path.clone(),
            exists: true,
            source_format: "codex_history_jsonl",
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
            route_provenance: Default::default(),
        },
        SourceBackedSelectorAuthority::DiscoveredWinner,
        SourceBackedRouteDriver::new(|_| Ok(()), |_| false, |_| true),
    )
    .expect("build watcher provider-root test route");
    registry.register(route);
    let route_identity = registry
        .watch_catalog()
        .route_ids()
        .next()
        .expect("one watcher provider-root test route")
        .clone();
    let definition = ProviderRootDefinition {
        id: "personal".to_owned(),
        provider: CaptureProvider::Codex,
        path: path.parent().expect("history path parent").to_path_buf(),
        group: Some(group.to_owned()),
        kind: None,
    };
    registry
        .set_applied_provider_roots(
            automatic_provider_discovery,
            provider_source_config_digest(
                automatic_provider_discovery,
                std::slice::from_ref(&definition),
            ),
            vec![AppliedProviderRoot::new(definition, vec![route_identity])
                .expect("valid watcher provider-root definition")],
        )
        .expect("install watcher provider-root definitions");
    registry.watch_catalog()
}

#[test]
fn provider_root_config_reload_enqueues_one_full_refresh_even_when_routes_are_unchanged(
) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let data_root = temp.path().join("data");
    let provider_root = temp.path().join("provider");
    let provider_file = provider_root.join("history.jsonl");
    fs::create_dir_all(&data_root)?;
    fs::create_dir_all(&provider_root)?;
    fs::write(&provider_file, b"{\"event\":1}\n")?;
    let personal =
        daemon_watch_test_catalog_with_provider_root_group(provider_file.clone(), "personal", true);
    let work =
        daemon_watch_test_catalog_with_provider_root_group(provider_file.clone(), "work", true);
    let automatic_disabled =
        daemon_watch_test_catalog_with_provider_root_group(provider_file, "personal", false);
    assert_eq!(
        personal.route_ids().collect::<Vec<_>>(),
        work.route_ids().collect::<Vec<_>>(),
        "group-only changes deliberately keep the physical route topology"
    );
    assert_ne!(
        personal.provider_root_config_digest(),
        work.provider_root_config_digest()
    );
    assert_ne!(
        personal.provider_root_config_digest(),
        automatic_disabled.provider_root_config_digest()
    );

    let route = personal
        .route_ids()
        .next()
        .expect("one provider-root route")
        .clone();
    let source = observation_fixture_source();
    write_observation_fixture_generation(
        &source_backed_index_root(&data_root),
        &route,
        &source,
        &provider_root.join("history.jsonl"),
        false,
        Some("personal"),
    )?;

    let coordinator = CoreRefreshEngine::new();
    let wakeup = Arc::new(DaemonWakeup::default());
    let mut watch_runtime =
        DaemonWatchRuntime::new(Arc::clone(&wakeup), &crate::test_support::CONFIG);
    watch_runtime.reconcile_catalog_and_route_authority_with(
        &data_root,
        Some(&coordinator),
        WatchCatalogReconcileTrigger::Startup,
        false,
        |_| Ok(personal.clone()),
        DaemonFileWatcher::start,
    );
    assert!(!coordinator.has_pending_request());

    watch_runtime.reconcile_catalog_and_route_authority_with(
        &data_root,
        Some(&coordinator),
        WatchCatalogReconcileTrigger::CatalogControl(EventWatermark::new(1, 1)),
        false,
        |_| Ok(work.clone()),
        |_, _, _| -> Result<DaemonFileWatcher> {
            panic!("a config reload must retain the active watcher owner")
        },
    );

    assert!(
        coordinator.has_pending_request(),
        "exact watch reconciliation cannot publish changed root aliases or source_groups"
    );
    assert!(
        watch_runtime.provider_root_refresh_pending_for_test(),
        "enqueue may coalesce into an older running full refresh, so digest demand must stay latched"
    );

    watch_runtime.reconcile_catalog_and_route_authority_with(
        &data_root,
        Some(&coordinator),
        WatchCatalogReconcileTrigger::CatalogControl(EventWatermark::new(2, 2)),
        false,
        |_| Ok(personal.clone()),
        |_, _, _| -> Result<DaemonFileWatcher> {
            panic!("A→B→A config churn must retain the active watcher owner")
        },
    );
    assert!(
        watch_runtime.provider_root_refresh_pending_for_test(),
        "matching the published A digest cannot consume demand while an admitted B refresh remains pending"
    );

    watch_runtime.reconcile_catalog_and_route_authority_with(
        &data_root,
        Some(&coordinator),
        WatchCatalogReconcileTrigger::CatalogControl(EventWatermark::new(3, 3)),
        false,
        |_| Ok(work.clone()),
        |_, _, _| -> Result<DaemonFileWatcher> {
            panic!("a repeated config reconciliation must retain the active watcher owner")
        },
    );
    assert!(
        watch_runtime.provider_root_refresh_pending_for_test(),
        "a coalesced old-snapshot request must not consume provider-root publication demand"
    );

    write_observation_fixture_generation(
        &source_backed_index_root(&data_root),
        &route,
        &source,
        &provider_root.join("history.jsonl"),
        false,
        Some("work"),
    )?;
    // The fixture publishes the completed successor directly instead of
    // running the queued engine request. Observe it with an idle engine to
    // model the real post-completion state.
    let completed_coordinator = CoreRefreshEngine::new();
    watch_runtime.reconcile_catalog_and_route_authority_with(
        &data_root,
        Some(&completed_coordinator),
        WatchCatalogReconcileTrigger::CatalogControl(EventWatermark::new(4, 4)),
        false,
        |_| Ok(work.clone()),
        |_, _, _| -> Result<DaemonFileWatcher> {
            panic!("a completed successor must retain the active watcher owner")
        },
    );
    assert!(
        !watch_runtime.provider_root_refresh_pending_for_test(),
        "publishing the desired config digest must consume the latched refresh demand"
    );
    Ok(())
}

struct ProviderObservationFixture {
    data_root: PathBuf,
    provider_file: PathBuf,
    catalog: SourceBackedWatchCatalog,
    route: ctx_history_index::SourceRouteIdentity,
    executor: Arc<dyn SourceBackedRefreshExecutor>,
    // Read only by linux-gated watcher tests; the fixture builds it on every
    // platform so the constructor stays platform-neutral.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    writer_launches: Arc<AtomicUsize>,
}

impl ProviderObservationFixture {
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
        let source = observation_fixture_source();
        write_observation_fixture_generation(
            &source_backed_index_root(&data_root),
            &route,
            &source,
            &provider_file,
            false,
            None,
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
                let generation_id = write_observation_fixture_generation(
                    execution.index_root,
                    &refresh_route,
                    &refresh_source,
                    &refresh_file,
                    true,
                    None,
                )?;
                Ok(SourceBackedRefreshPublication {
                    zero_source_authority: Vec::new(),
                    generation_id,
                    published_explicit_source_catalog: execution.explicit_source_catalog.cloned(),
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
                    route_results: vec![SourceBackedRefreshRouteResult::succeeded(
                        refresh_route.as_str().to_owned(),
                        true,
                    )],
                    catalog_route_bindings: Vec::new(),
                    verified_index: None,
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
        fixture.publish_initial_observation()?;
        Ok(fixture)
    }

    fn publish_initial_observation(&self) -> Result<()> {
        let coordinator = CoreRefreshEngine::with_executor(Arc::clone(&self.executor));
        coordinator.install_watch_catalog(self.catalog.clone());
        coordinator.schedule_startup_route_reconciliation(
            self.catalog.route_ids().cloned(),
            EventWatermark::new(1, 0),
            0,
        );
        assert!(coordinator.enqueue_next_dirty_route(&self.data_root, u64::MAX)?);
        let run = coordinator
            .run_next(&self.data_root)
            .expect("frontier refresh");
        assert!(!run.failed, "{:#}", run.job);
        assert!(matches!(run.scope, SourceBackedRefreshScope::Exact(_)));
        assert!(!coordinator.has_scheduled_route_work());
        assert!(!self
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

fn observation_fixture_source() -> SourceKey {
    SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        SourceAnchor::CatalogLineage([0x5a; 32]),
    )
    .unwrap()
}

fn observation_fixture_record(source: &SourceKey, body: String) -> CoreRecord {
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
        source.clone(),
        0,
        "message",
        "durable-frontier-test-v1",
        body,
    )
    .unwrap();
    record.role = Some("user".to_owned());
    record
}

fn observation_fixture_certificate(source: &SourceKey, bytes: &[u8]) -> CertifiedSource {
    let mut revision = [0_u8; 32];
    for (index, byte) in bytes.iter().enumerate() {
        revision[index % 32] ^= byte;
    }
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

fn write_observation_fixture_generation(
    index_root: &Path,
    route: &ctx_history_index::SourceRouteIdentity,
    source: &SourceKey,
    provider_file: &Path,
    route_staged: bool,
    provider_root_group: Option<&str>,
) -> Result<String> {
    let bytes = fs::read(provider_file)?;
    let mut writer = GenerationWriter::open(index_root, WriterOptions::default())?
        .into_writer()
        .map_err(crate::committed_generation_recovery_error)?;
    if let Some(group) = provider_root_group {
        let definition = ProviderRootDefinition {
            id: "personal".to_owned(),
            provider: CaptureProvider::Codex,
            path: provider_file
                .parent()
                .expect("provider-root fixture file parent")
                .to_path_buf(),
            group: Some(group.to_owned()),
            kind: None,
        };
        writer.set_applied_provider_roots(
            true,
            provider_source_config_digest(true, std::slice::from_ref(&definition)),
            vec![AppliedProviderRoot::new(definition, vec![route.clone()])?],
        )?;
    }
    if route_staged {
        writer.set_source_route_plan(BTreeSet::from([route.clone()]), BTreeSet::new())?;
        writer.begin_source_route_stage(route.clone())?;
    }
    writer.begin_source(source.clone())?;
    writer.add_core_record(observation_fixture_record(
        source,
        String::from_utf8_lossy(&bytes).into_owned(),
    ))?;
    writer.certify_source(observation_fixture_certificate(source, &bytes))?;
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
    let mut watch_runtime =
        DaemonWatchRuntime::new(Arc::clone(&wakeup), &crate::test_support::CONFIG);

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
        "a newly restored provider route must be scheduled"
    );
    assert_eq!(
        super::super::daemon_wakeup::daemon_wakeup_report(&data_root)["status"],
        "active"
    );
    #[cfg(target_os = "linux")]
    {
        coordinator.initialize_watch_route_authority([]);
        coordinator.install_watch_catalog(catalog.clone());
        assert!(!coordinator.has_scheduled_route_work());
        let uncertain = EventWatermark::new(41, 7);
        coordinator.fence_watch_uncertainty(uncertain);
        let reconcile = |runtime: &mut DaemonWatchRuntime| {
            runtime.reconcile_catalog_and_route_authority_with(
                &data_root,
                Some(&coordinator),
                WatchCatalogReconcileTrigger::SafetyTimeout,
                false,
                |_| Ok(catalog.clone()),
                DaemonFileWatcher::start,
            );
        };
        let permissions = fs::metadata(&provider_root)?.permissions();
        fs::set_permissions(&provider_root, fs::Permissions::from_mode(0o0))?;
        reconcile(&mut watch_runtime);
        fs::set_permissions(&provider_root, permissions)?;
        assert_eq!(coordinator.watch_uncertainty_watermark(), Some(uncertain));
        reconcile(&mut watch_runtime);
        assert!(coordinator.watch_uncertainty_watermark().is_none());
        assert!(coordinator.scheduled_route_ids_for_test().contains(&route));
    }
    Ok(())
}

#[test]
fn safety_reconciliation_recreates_a_failed_startup_watcher_without_healthy_catalog_scans(
) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let data_root = temp.path().join("data");
    let provider_root = temp.path().join("provider");
    let provider_file = provider_root.join("history.jsonl");
    let newly_discovered_file = provider_root.join("new-history.jsonl");
    fs::create_dir_all(&data_root)?;
    fs::create_dir_all(&provider_root)?;
    fs::write(&provider_file, b"{\"event\":1}\n")?;
    fs::write(&newly_discovered_file, b"{\"event\":2}\n")?;
    let catalog = daemon_watch_test_catalog(provider_file);
    let recovered_catalog = daemon_watch_test_catalog_for_paths([
        provider_root.join("history.jsonl"),
        newly_discovered_file,
    ]);
    let new_route = recovered_catalog
        .route_ids()
        .find(|route| !catalog.route_ids().any(|initial| initial == *route))
        .cloned()
        .expect("recovered catalog adds one route");
    let coordinator = CoreRefreshEngine::new();
    let wakeup = Arc::new(DaemonWakeup::default());
    let mut watch_runtime =
        DaemonWatchRuntime::new(Arc::clone(&wakeup), &crate::test_support::CONFIG);
    let catalog_attempts = std::cell::Cell::new(0_u64);
    let watcher_attempts = std::cell::Cell::new(0_u64);

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
            Ok(recovered_catalog.clone())
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
    assert!(
        coordinator
            .scheduled_route_ids_for_test()
            .contains(&new_route),
        "a route discovered while the watcher was absent must receive an initial observation"
    );

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

#[test]
fn persistent_watcher_failure_polls_healthy_routes_on_every_safety_pass() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let fixture = ProviderObservationFixture::new(temp.path())?;
    let coordinator = fixture.restarted_coordinator();
    let wakeup = Arc::new(DaemonWakeup::default());
    let mut watch_runtime = DaemonWatchRuntime::new(wakeup, &crate::test_support::CONFIG);

    let startup_missing_scans = watch_runtime.reconcile_catalog_and_route_authority_with(
        &fixture.data_root,
        Some(&coordinator),
        WatchCatalogReconcileTrigger::Startup,
        false,
        |_| Ok(fixture.catalog.clone()),
        |_, _, _| Err(anyhow!("injected persistent watcher failure")),
    );
    assert_eq!(startup_missing_scans, 1);
    assert!(watch_runtime.file_watcher.is_none());
    assert!(
        coordinator.has_scheduled_route_work(),
        "an unchanged observation is unsafe when no watcher fenced it"
    );
    // Normalize the test's synthetic scheduler clock without sleeping through
    // the production debounce window.
    coordinator.schedule_startup_route_reconciliation(
        [fixture.route.clone()],
        EventWatermark::new(0, 0),
        0,
    );
    assert!(coordinator.enqueue_next_dirty_route(&fixture.data_root, u64::MAX)?);
    let fallback = coordinator
        .run_next(&fixture.data_root)
        .expect("fallback refresh");
    assert!(!fallback.failed, "{:#}", fallback.job);
    assert!(!coordinator.has_scheduled_route_work());

    fs::write(
        &fixture.provider_file,
        b"changed while watcher unavailable\n",
    )?;
    let safety_missing_scans = watch_runtime.reconcile_catalog_and_route_authority_with(
        &fixture.data_root,
        Some(&coordinator),
        WatchCatalogReconcileTrigger::SafetyTimeout,
        false,
        |_| Ok(fixture.catalog.clone()),
        |_, _, _| Err(anyhow!("injected persistent watcher failure")),
    );
    assert_eq!(
        safety_missing_scans, 1,
        "one safety timeout must run one pending-missing scan"
    );
    assert!(
        coordinator.has_scheduled_route_work(),
        "safety polling must keep healthy routes live during watcher failure"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn forced_watcher_recovery_adds_no_scans_after_startup_reconciliation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let fixture = ProviderObservationFixture::new(temp.path())?;
    let baseline_writer_launches = fixture.writer_launches.load(Ordering::SeqCst);
    let coordinator = fixture.restarted_coordinator();
    let wakeup = Arc::new(DaemonWakeup::default());
    let mut watch_runtime =
        DaemonWatchRuntime::new(Arc::clone(&wakeup), &crate::test_support::CONFIG);
    watch_runtime.reconcile_catalog_and_route_authority_with(
        &fixture.data_root,
        Some(&coordinator),
        WatchCatalogReconcileTrigger::Startup,
        false,
        |_| Ok(fixture.catalog.clone()),
        DaemonFileWatcher::start,
    );
    assert!(coordinator.watch_routes_initialized());
    assert!(coordinator.has_scheduled_route_work());
    std::thread::sleep(StdDuration::from_millis(300));
    assert!(coordinator.enqueue_next_dirty_route(&fixture.data_root, u64::MAX)?);
    let startup_run = coordinator
        .run_next(&fixture.data_root)
        .expect("bounded startup reconciliation");
    assert!(!startup_run.failed, "{:#}", startup_run.job);
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
        baseline_writer_launches.saturating_add(1)
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
        baseline_writer_launches.saturating_add(1)
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn startup_reconciliation_schedules_a_route_changed_while_daemon_was_stopped() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let fixture = ProviderObservationFixture::new(temp.path())?;
    let baseline_writer_launches = fixture.writer_launches.load(Ordering::SeqCst);

    // No watcher/runtime is alive here: the next bounded provider refresh
    // must certify this mutation during production startup.
    fs::write(&fixture.provider_file, b"one\ntwo\n")?;
    let coordinator = fixture.restarted_coordinator();
    let wakeup = Arc::new(DaemonWakeup::default());
    let mut watch_runtime =
        DaemonWatchRuntime::new(Arc::clone(&wakeup), &crate::test_support::CONFIG);
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
    let mut watch_runtime =
        DaemonWatchRuntime::new(Arc::clone(&wakeup), &crate::test_support::CONFIG);
    watch_runtime.reconcile_catalog_and_route_authority_with(
        &data_root,
        None,
        WatchCatalogReconcileTrigger::Startup,
        false,
        |_| Ok(catalog.clone()),
        DaemonFileWatcher::start,
    );
    coordinator.install_watch_catalog(catalog.clone());
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

#[test]
fn only_enabled_long_lived_daemon_uses_upgrade_scheduler() {
    assert!(daemon_should_schedule_auto_upgrade(
        true,
        DaemonMode::Full,
        true
    ));
    assert!(!daemon_should_schedule_auto_upgrade(
        false,
        DaemonMode::Full,
        true
    ));
    assert!(!daemon_should_schedule_auto_upgrade(
        true,
        DaemonMode::SourceRefreshOnly,
        true
    ));
    assert!(!daemon_should_schedule_auto_upgrade(
        true,
        DaemonMode::Full,
        false
    ));
}

#[test]
fn due_dirty_route_wakes_the_scheduler_immediately() {
    let coordinator =
        CoreRefreshEngine::with_executor(Arc::new(|_: SourceBackedRefreshExecution<'_>| {
            anyhow::bail!("executor must remain idle")
        }));
    let route = ctx_history_index::SourceRouteIdentity::from_sha256("ab".repeat(32))
        .expect("route identity");
    coordinator.reconcile_watch_routes([route], EventWatermark::new(1, 0), 0);
    let now = Instant::now();
    let runtime = DaemonRuntime::default();

    let wait_for = daemon_wait_duration(
        &runtime,
        Some(&coordinator),
        now + StdDuration::from_secs(30),
        now,
    );

    assert_eq!(wait_for, StdDuration::ZERO);
    assert!(daemon_scheduled_refresh_due(Some(&coordinator), 1_000));
}

#[test]
fn pending_source_refresh_wait_ignores_unreachable_work_until_retry() {
    let coordinator =
        CoreRefreshEngine::with_executor(Arc::new(|_: SourceBackedRefreshExecution<'_>| {
            anyhow::bail!("executor must remain backed off")
        }));
    coordinator.enqueue_for_test(None);
    let route = ctx_history_index::SourceRouteIdentity::from_sha256("cd".repeat(32))
        .expect("route identity");
    coordinator.reconcile_watch_routes([route], EventWatermark::new(1, 0), 0);
    let now = Instant::now();
    let mut runtime = DaemonRuntime::default();
    runtime.history_retry.consecutive_failures = 1;
    runtime.history_retry.retry_not_before = Some(now + StdDuration::from_secs(5));
    runtime.history_retry.retry_not_before_at_ms = Some(utc_now().timestamp_millis() + 5_000);
    runtime.semantic_retry.consecutive_failures = 1;
    runtime.semantic_retry.retry_not_before = Some(now);
    runtime.consumer_retry_deferral.retry_at = Some(now + StdDuration::from_millis(1));

    let wait_for = daemon_wait_duration(
        &runtime,
        Some(&coordinator),
        now + StdDuration::from_secs(30),
        now,
    );

    assert!(wait_for > StdDuration::from_secs(4));
    assert!(wait_for <= StdDuration::from_secs(5));
    assert!(daemon_scheduled_refresh_due(Some(&coordinator), 1_000));
}

fn test_daemon_run_args() -> DaemonRunArgs {
    DaemonRunArgs {
        loop_interval_seconds: None,
        max_chunks: None,
        handle_process_signals: false,
        force: false,
        profile: crate::DaemonRunProfile::Persistent,
        start_mode: None,
        trigger_command: None,
        supervisor: crate::DaemonSupervisor::User,
    }
}

#[test]
fn finite_worker_quiescence_waits_for_active_ipc_and_then_rejects_new_requests() {
    let activity = Arc::new(crate::query_service::DaemonQueryActivity::new());
    let guard = activity.begin_request().expect("first request admitted");

    assert!(!activity.begin_stopping_if_idle());
    drop(guard);
    assert!(activity.begin_stopping_if_idle());
    assert!(activity.begin_request().is_none());
}

fn run_daemon_scheduler_cycle_with_activity(
    args: &DaemonRunArgs,
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    deadline: Option<Instant>,
    semantic_enabled: bool,
    query_activity: Option<&crate::query_service::DaemonQueryActivity>,
    source_refresh: Option<&CoreRefreshEngine>,
) -> Result<DaemonIteration> {
    super::super::daemon_scheduler::run_daemon_scheduler_cycle_with_activity(
        args,
        data_root,
        runtime,
        super::super::daemon_scheduler::DaemonSchedulerCycleContext {
            deadline,
            semantic_enabled,
            query_activity,
            source_refresh,
        },
        super::super::daemon_scheduler::DaemonSchedulerPorts {
            generation_published: &crate::test_support::GENERATION_PUBLISHED,
            semantic: super::super::daemon_scheduler::DaemonSemanticJobPorts {
                artifact_fetcher: &crate::test_support::ARTIFACT,
                config: &crate::test_support::CONFIG,
            },
            observation: &crate::test_support::OBSERVATION,
        },
    )
}

#[test]
fn semantic_runtime_is_requested_only_for_supported_full_daemons() {
    let mut config = AppConfig {
        semantic_enabled: true,
        daemon: crate::DaemonProductConfig {
            mode: DaemonMode::Full,
            ..Default::default()
        },
        ..Default::default()
    };

    assert!(daemon_semantic_runtime_requested(&config, true));
    assert!(!daemon_semantic_runtime_requested(&config, false));

    config.semantic_enabled = false;
    assert!(!daemon_semantic_runtime_requested(&config, true));

    config.semantic_enabled = true;
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
            daemon: crate::DaemonProductConfig {
                enabled: true,
                mode: DaemonMode::SourceRefreshOnly,
            },
            semantic_enabled: true,
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
fn foreground_query_preempts_daemon_background_jobs() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let _hooks = install_daemon_test_job_hooks(DaemonTestJobHooks {
        calls: calls.clone(),
        semantic_index: Some(json!({
            "status": "budget_exhausted",
            "indexed_chunks": 1,
        })),
    });
    let activity = Arc::new(crate::query_service::DaemonQueryActivity::new());
    let _request = activity
        .begin_request()
        .expect("test foreground query should be accepted");
    let mut runtime = DaemonRuntime::default();

    let iteration = run_daemon_scheduler_cycle_with_activity(
        &test_daemon_run_args(),
        temp.path(),
        &mut runtime,
        None,
        true,
        Some(activity.as_ref()),
        None,
    )?;

    assert!(!iteration.did_work);
    assert!(!iteration.failed);
    assert!(calls.borrow().is_empty());
    assert!(!super::super::paths_status::daemon_core_refresh_job_path(temp.path()).exists());
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
                )?
                .into_writer()
                .map_err(crate::committed_generation_recovery_error)?;
                let receipt = writer.commit(|_| true)?;
                Ok(SourceBackedRefreshPublication {
                    route_results: Vec::new(),
                    zero_source_authority: Vec::new(),
                    catalog_route_bindings: Vec::new(),
                    verified_index: None,
                    generation_id: receipt.generation_id,
                    published_explicit_source_catalog: execution.explicit_source_catalog.cloned(),
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
    use super::super::source_backed_refresh_coordinator::{
        SourceBackedRefreshCurrent, SourceBackedRefreshExecution, SourceBackedRefreshPublication,
        SourceBackedRefreshTimings,
    };
    use ctx_history_refresh::EventWatermark;

    let temp = tempfile::tempdir()?;
    let route = ctx_history_index::SourceRouteIdentity::from_sha256("42".repeat(32))?;
    let refresh_calls = Arc::new(AtomicUsize::new(0));
    let executor_calls = refresh_calls.clone();
    let executor_route = route.clone();
    let expected_scope =
        ctx_history_capture::SourceBackedRefreshScope::Exact(BTreeSet::from([route.clone()]));
    let coordinator = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            executor_calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                execution.admitted_refresh().publication_scope(),
                expected_scope
            );
            let writer = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                ctx_history_index::WriterOptions::default(),
            )?
            .into_writer()
            .map_err(crate::committed_generation_recovery_error)?;
            let receipt = writer.commit(|_| true)?;
            Ok(SourceBackedRefreshPublication {
                route_results: vec![SourceBackedRefreshRouteResult::succeeded(
                    executor_route.as_str().to_owned(),
                    true,
                )],
                zero_source_authority: Vec::new(),
                catalog_route_bindings: Vec::new(),
                verified_index: None,
                generation_id: receipt.generation_id,
                published_explicit_source_catalog: execution.explicit_source_catalog.cloned(),
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
    assert!(coordinator.enqueue_next_dirty_route(temp.path(), u64::MAX)?);
    let pending = read_daemon_job_status(&daemon_core_refresh_job_path(temp.path()))
        .ok_or_else(|| anyhow!("periodic source refresh admission was not persisted"))?;
    coordinator.complete_pending_admission_for_test(
        temp.path(),
        pending["request_id"]
            .as_str()
            .ok_or_else(|| anyhow!("periodic source refresh admission has no request ID"))?,
        Default::default(),
    )?;
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

    let job = read_daemon_job_status(&daemon_core_refresh_job_path(temp.path()))
        .ok_or_else(|| anyhow!("periodic source refresh job was not persisted"))?;
    assert!(iteration.did_work, "{job:#}");
    assert!(!iteration.failed);
    assert!(iteration.continue_immediately);
    assert_eq!(refresh_calls.load(Ordering::SeqCst), 1);
    assert_eq!(job["status"], "completed");
    assert_eq!(job["daemon_mode"], "full");
    assert_eq!(job["trigger"], "periodic");
    assert_eq!(job["trigger_provenance"], "daemon_scheduler");
    assert!(job.get("published_explicit_source_catalog").is_none());
    assert!(job["receipt"]
        .get("published_explicit_source_catalog")
        .is_none());
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
