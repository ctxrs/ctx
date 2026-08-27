use super::*;
use ctx_daemon_runtime::{NativeWatchError, NativeWatchEvent, NativeWatchIgnore};
use ctx_history_capture::{
    SourceBackedProviderRegistry, SourceBackedRoute, SourceBackedRouteDriver,
    SourceBackedSelectorAuthority,
};
use ctx_history_capture_model::{
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus,
};
use ctx_history_core::CaptureProvider;
use std::{sync::Barrier, thread};

#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicBool, Ordering};

fn catalog_route(
    provider: CaptureProvider,
    path: PathBuf,
    source_format: &'static str,
) -> SourceBackedRoute {
    SourceBackedRoute::automatic(
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
    .unwrap()
}

fn watch_catalog(routes: impl IntoIterator<Item = SourceBackedRoute>) -> SourceBackedWatchCatalog {
    let mut registry = SourceBackedProviderRegistry::new();
    for route in routes {
        registry.register(route);
    }
    registry.watch_catalog()
}

fn catalog_owner(catalog: SourceBackedWatchCatalog) -> DaemonWatchCatalog {
    let owner = DaemonWatchCatalog::default();
    owner.publish(catalog);
    owner
}

fn watch_authority(data_root: &Path, catalog: SourceBackedWatchCatalog) -> WatchAuthority {
    WatchAuthority::new(data_root, catalog_owner(catalog))
}

#[cfg(target_os = "linux")]
#[test]
fn request_overlay_is_not_watched_but_provider_append_wakes() {
    use std::{fs::OpenOptions, io::Write};

    let temp = tempfile::tempdir().expect("create watcher fixture");
    let data_root = temp.path().join("data");
    let provider_root = temp.path().join("provider");
    let provider_file = provider_root.join("session.jsonl");
    let explicit_root = temp.path().join("request-overlay");
    let explicit_file = explicit_root.join("one-shot.jsonl");
    fs::create_dir_all(&data_root).expect("create data root");
    fs::create_dir_all(&provider_root).expect("create provider root");
    fs::create_dir_all(&explicit_root).expect("create request overlay root");
    fs::write(&provider_file, b"{\"event\":1}\n").expect("write provider fixture");
    fs::write(&explicit_file, b"{\"event\":1}\n").expect("write request overlay fixture");

    let wakeup = Arc::new(DaemonWakeup::default());
    let watcher = DaemonFileWatcher::start(
        &data_root,
        Arc::clone(&wakeup),
        catalog_owner(watch_catalog([catalog_route(
            CaptureProvider::Codex,
            provider_file.clone(),
            "codex_history_jsonl",
        )])),
    )
    .expect("start daemon watcher");
    let targets = watcher
        .authority
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .target_paths();
    assert!(targets.contains(&provider_file));
    assert!(!targets.contains(&explicit_file));
    assert!(!targets
        .iter()
        .any(|path| path.starts_with(data_root.join("catalogs/explicit-sources"))));

    let mut explicit = OpenOptions::new()
        .append(true)
        .open(&explicit_file)
        .expect("open request overlay fixture for append");
    explicit
        .write_all(b"{\"event\":2}\n")
        .expect("append request overlay event");
    explicit.flush().expect("flush request overlay append");
    drop(explicit);
    thread::sleep(WATCH_DEBOUNCE_QUIET * 3);

    let idle = wakeup.snapshot();
    assert_eq!(idle["filesystem_signals"], 0, "{idle:#}");
    assert_eq!(idle["work_cycles"], 0, "{idle:#}");
    assert_eq!(idle["no_work_cycles"], 0, "{idle:#}");

    let mut file = OpenOptions::new()
        .append(true)
        .open(&provider_file)
        .expect("open provider fixture for append");
    file.write_all(b"{\"event\":2}\n")
        .expect("append provider event");
    file.flush().expect("flush provider append");
    drop(file);
    let wake = wakeup.wait(Duration::from_secs(2));
    assert!(wake.filesystem, "provider append did not wake the daemon");
    assert!(!wake.timed_out, "provider append exceeded two seconds");
    assert_eq!(wake.source_watch.routes.len(), 1);
    assert!(wake.source_watch.reconcile.is_none());

    drop(watcher);
}

#[test]
fn wakeup_blocks_until_signaled_and_coalesces_reasons() {
    let wakeup = Arc::new(DaemonWakeup::default());
    wakeup.signal_filesystem();
    wakeup.signal_ipc();
    let wake = wakeup.wait(Duration::from_secs(1));
    assert!(wake.filesystem);
    assert!(!wake.shutdown);
    assert!(!wake.timed_out);
    assert_eq!(wakeup.snapshot()["ipc_signals"], 1);
}

#[test]
fn catalog_uncertainty_atomically_fences_pending_exact_routes() {
    let catalog = watch_catalog([catalog_route(
        CaptureProvider::Codex,
        PathBuf::from("/tmp/provider/session.jsonl"),
        "codex_history_jsonl",
    )]);
    let route = catalog.route_ids().next().unwrap().clone();
    let wakeup = DaemonWakeup::default();

    for sequence in 1..=WATCH_EVENT_QUEUE_CAPACITY as u64 * 4 {
        let watermark = EventWatermark::new(7, sequence);
        let mut batch = SourceWatchBatch::default();
        batch.routes.insert(route.clone(), watermark);
        if sequence % 2 == 0 {
            batch.reconcile = Some(watermark);
            batch.rearm = true;
        }
        wakeup.signal_source_watch(batch);
    }

    let pending = wakeup.pending_source_watch();
    assert!(pending.routes.is_empty());
    assert!(pending.members.is_empty());
    assert_eq!(
        pending.reconcile,
        Some(EventWatermark::new(
            7,
            WATCH_EVENT_QUEUE_CAPACITY as u64 * 4
        ))
    );
    assert!(pending.rearm);

    let wake = wakeup.wait(Duration::ZERO);
    assert!(wake.source_watch.routes.is_empty());
    assert!(wakeup.pending_source_watch().is_empty());
}

#[test]
fn source_watch_sink_receives_pending_and_live_routes_before_daemon_wait() {
    let catalog = watch_catalog([catalog_route(
        CaptureProvider::Codex,
        PathBuf::from("/tmp/provider/direct-ingress.jsonl"),
        "codex_history_jsonl",
    )]);
    let route = catalog.route_ids().next().unwrap().clone();
    let wakeup = DaemonWakeup::default();
    let mut pending = SourceWatchBatch::default();
    pending
        .routes
        .insert(route.clone(), EventWatermark::new(11, 1));
    wakeup.observe_source_watch(&pending);
    wakeup.signal_source_watch(pending);

    let observed = Arc::new(Mutex::new(SourceWatchBatch::default()));
    let sink_observed = Arc::clone(&observed);
    wakeup.install_source_watch_sink(Arc::new(move |batch| {
        sink_observed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .merge(batch.clone());
    }));
    assert!(wakeup.has_source_watch_sink());
    assert_eq!(
        observed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .routes
            .get(&route),
        Some(&EventWatermark::new(11, 1))
    );

    let mut live = SourceWatchBatch::default();
    live.routes
        .insert(route.clone(), EventWatermark::new(11, 2));
    wakeup.observe_source_watch(&live);
    wakeup.signal_source_watch(live);
    assert_eq!(
        observed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .routes
            .get(&route),
        Some(&EventWatermark::new(11, 2))
    );

    let wake = wakeup.wait(Duration::ZERO);
    assert_eq!(
        wake.source_watch.routes.get(&route),
        Some(&EventWatermark::new(11, 2))
    );
}

#[test]
fn compact_pressure_fence_bypasses_large_staged_route_payload() {
    use crate::source_backed_refresh_coordinator::CoreRefreshEngine;

    let wakeup = DaemonWakeup::default();
    let coordinator = Arc::new(CoreRefreshEngine::new());
    let pressure_coordinator = Arc::clone(&coordinator);
    wakeup.install_source_watch_pressure_sink(Arc::new(move |watermark| {
        pressure_coordinator.fence_watch_uncertainty(watermark);
    }));

    let mut staged = SourceWatchBatch::default();
    for index in 1..=4_096_u64 {
        let route = SourceRouteIdentity::from_sha256(format!("{index:064x}")).unwrap();
        staged
            .routes
            .insert(route.clone(), EventWatermark::new(23, index));
        staged
            .members
            .insert(route, BTreeSet::from([PathBuf::from(format!("/{index}"))]));
    }
    wakeup.observe_source_watch(&staged);

    let pressure = EventWatermark::new(23, 5_000);
    wakeup.fence_source_watch_pressure(pressure);
    assert_eq!(coordinator.watch_uncertainty_watermark(), Some(pressure));

    let flushed = Arc::new(Mutex::new(None));
    let flushed_sink = Arc::clone(&flushed);
    let worker_coordinator = Arc::clone(&coordinator);
    wakeup.install_source_watch_sink(Arc::new(move |batch| {
        if let Some(watermark) = batch.reconcile {
            worker_coordinator.fence_watch_uncertainty(watermark);
        } else {
            *flushed_sink.lock().unwrap() = Some(batch.clone());
        }
    }));
    let flushed = flushed.lock().unwrap().take().expect("staged route batch");
    assert_eq!(flushed.routes, staged.routes);
    assert_eq!(flushed.members, staged.members);

    let worker_boundary = EventWatermark::new(23, 5_001);
    let recovery = SourceWatchBatch::uncertainty(worker_boundary);
    wakeup.observe_source_watch(&recovery);
    wakeup.signal_source_watch(recovery);
    assert_eq!(
        coordinator.watch_uncertainty_watermark(),
        Some(worker_boundary)
    );
    assert_eq!(
        wakeup.wait(Duration::ZERO).source_watch.reconcile,
        Some(worker_boundary)
    );
}

#[test]
fn source_watch_install_cannot_miss_signal_paused_after_pending_merge() {
    let catalog = watch_catalog([catalog_route(
        CaptureProvider::Codex,
        PathBuf::from("/tmp/provider/install-race.jsonl"),
        "codex_history_jsonl",
    )]);
    let route = catalog.route_ids().next().unwrap().clone();
    let wakeup = Arc::new(DaemonWakeup::default());
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let hook_entered = Arc::clone(&entered);
    let hook_release = Arc::clone(&release);
    wakeup.install_before_source_watch_sink_dispatch_hook(Arc::new(move || {
        hook_entered.wait();
        hook_release.wait();
    }));

    let signal_wakeup = Arc::clone(&wakeup);
    let signal_route = route.clone();
    let signal = thread::spawn(move || {
        let mut batch = SourceWatchBatch::default();
        batch
            .routes
            .insert(signal_route, EventWatermark::new(12, 1));
        signal_wakeup.observe_source_watch(&batch);
        signal_wakeup.signal_source_watch(batch);
    });

    entered.wait();
    let observed = Arc::new(Mutex::new(SourceWatchBatch::default()));
    let sink_observed = Arc::clone(&observed);
    wakeup.install_source_watch_sink(Arc::new(move |batch| {
        sink_observed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .merge(batch.clone());
    }));
    assert_eq!(
        observed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .routes
            .get(&route),
        Some(&EventWatermark::new(12, 1)),
        "installation must replay the batch already merged by signal"
    );
    release.wait();
    signal.join().unwrap();

    let wake = wakeup.wait(Duration::ZERO);
    assert!(wake.filesystem);
    assert_eq!(
        wake.source_watch.routes.get(&route),
        Some(&EventWatermark::new(12, 1))
    );
}

#[test]
fn in_capture_route_reaches_handoff_fence_before_debounced_wake() {
    let data_root = Path::new("/tmp/ctx-data");
    let daemon_root = data_root.join("daemon");
    let provider_file = PathBuf::from("/tmp/provider/in-capture.jsonl");
    let catalog = watch_catalog([catalog_route(
        CaptureProvider::Codex,
        provider_file.clone(),
        "codex_history_jsonl",
    )]);
    let route = catalog.route_ids().next().unwrap().clone();
    let authority = RwLock::new(watch_authority(data_root, catalog));
    let counters = Mutex::new(WatchCounters::default());
    let wakeup = DaemonWakeup::default();
    let ledger = Arc::new(Mutex::new(
        BTreeMap::<SourceRouteIdentity, EventWatermark>::new(),
    ));
    let sink_ledger = Arc::clone(&ledger);
    wakeup.install_source_watch_sink(Arc::new(move |batch| {
        let mut ledger = sink_ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (route, watermark) in &batch.routes {
            ledger
                .entry(route.clone())
                .and_modify(|current| *current = (*current).max(*watermark))
                .or_insert(*watermark);
        }
    }));

    let batch = record_and_observe_watch_event(
        &authority,
        &counters,
        &wakeup,
        data_root,
        &daemon_root,
        Ok(NativeWatchEvent::ordinary(vec![provider_file])),
        EventWatermark::new(13, 1),
    );

    assert_eq!(
        ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&route)
            .copied(),
        Some(EventWatermark::new(13, 1)),
        "the publication handoff fence must see the event immediately"
    );
    assert!(
        batch.members.is_empty(),
        "a missing event path must fail closed"
    );
    let before_debounce = wakeup.wait(Duration::ZERO);
    assert!(!before_debounce.filesystem);
    assert!(before_debounce.source_watch.is_empty());

    wakeup.signal_source_watch(batch);
    let after_debounce = wakeup.wait(Duration::ZERO);
    assert!(after_debounce.filesystem);
    assert_eq!(
        after_debounce.source_watch.routes.get(&route),
        Some(&EventWatermark::new(13, 1))
    );
}

#[test]
fn ordinary_tree_event_preserves_one_exact_regular_member() {
    let temp = tempfile::tempdir().expect("create exact-member fixture");
    let data_root = temp.path().join("data");
    let daemon_root = data_root.join("daemon");
    let provider_root = temp.path().join("provider");
    let provider_file = provider_root.join("session.jsonl");
    fs::create_dir_all(&data_root).expect("create data root");
    fs::create_dir_all(&provider_root).expect("create provider root");
    fs::write(&provider_file, b"{\"event\":1}\n").expect("write provider member");
    let catalog = watch_catalog([catalog_route(
        CaptureProvider::Codex,
        provider_root,
        "codex_session_jsonl_tree",
    )]);
    let route = catalog.route_ids().next().unwrap().clone();
    let authority = RwLock::new(watch_authority(&data_root, catalog));
    let counters = Mutex::new(WatchCounters::default());

    let batch = record_watch_event(
        &authority,
        &counters,
        &data_root,
        &daemon_root,
        Ok(NativeWatchEvent::ordinary(vec![provider_file.clone()])),
        EventWatermark::new(14, 1),
    );

    assert_eq!(batch.routes.len(), 1);
    assert_eq!(
        batch.members.get(&route),
        Some(&BTreeSet::from([provider_file]))
    );
}

#[cfg(target_os = "linux")]
#[test]
fn native_tree_append_preserves_one_exact_regular_member() {
    use std::{fs::OpenOptions, io::Write};

    let temp = tempfile::tempdir().expect("create native watcher fixture");
    let data_root = temp.path().join("data");
    let provider_root = temp.path().join("provider");
    let provider_file = provider_root.join("session.jsonl");
    fs::create_dir_all(&data_root).expect("create data root");
    fs::create_dir_all(&provider_root).expect("create provider root");
    fs::write(&provider_file, b"{\"event\":1}\n").expect("write provider member");
    let catalog = watch_catalog([catalog_route(
        CaptureProvider::Codex,
        provider_root,
        "codex_session_jsonl_tree",
    )]);
    let route = catalog.route_ids().next().unwrap().clone();
    let wakeup = Arc::new(DaemonWakeup::default());
    let watcher = DaemonFileWatcher::start(&data_root, Arc::clone(&wakeup), catalog_owner(catalog))
        .expect("start daemon watcher");

    let mut file = OpenOptions::new()
        .append(true)
        .open(&provider_file)
        .expect("open provider member for append");
    file.write_all(b"{\"event\":2}\n")
        .expect("append provider event");
    file.flush().expect("flush provider append");
    drop(file);

    let wake = wakeup.wait(Duration::from_secs(3));
    assert!(wake.filesystem, "provider append did not wake the daemon");
    assert_eq!(wake.source_watch.routes.len(), 1);
    assert!(wake.source_watch.reconcile.is_none());
    assert_eq!(
        wake.source_watch.members.get(&route),
        Some(&BTreeSet::from([provider_file]))
    );

    drop(watcher);
}
#[cfg(target_os = "linux")]
#[test]
fn forced_rearm_observes_a_recreated_recursive_root() {
    let temp = tempfile::tempdir().expect("create watcher fixture");
    let data_root = temp.path().join("data");
    let provider_root = temp.path().join("provider");
    fs::create_dir_all(&data_root).expect("create data root");
    fs::create_dir_all(&provider_root).expect("create provider root");
    fs::write(provider_root.join("initial.jsonl"), b"{\"event\":1}\n")
        .expect("write initial source");
    let catalog = watch_catalog([catalog_route(
        CaptureProvider::Codex,
        provider_root.clone(),
        "codex_session_jsonl_tree",
    )]);
    let route = catalog.route_ids().next().unwrap().clone();
    let wakeup = Arc::new(DaemonWakeup::default());
    let mut watcher =
        DaemonFileWatcher::start(&data_root, Arc::clone(&wakeup), catalog_owner(catalog)).unwrap();

    fs::remove_dir_all(&provider_root).expect("remove watched root");
    let removed = wakeup.wait(Duration::from_secs(3));
    assert!(removed.filesystem, "root removal did not wake the watcher");
    assert!(removed.source_watch.routes.contains_key(&route));
    assert!(removed.source_watch.rearm);

    fs::create_dir_all(&provider_root).expect("recreate watched root");
    let attempts_before = watcher.runtime.snapshot().registration_attempts;
    let (_, registration) = watcher.reconcile_roots(true);
    registration.expect("force native watcher re-registration");
    fs::write(provider_root.join("recreated.jsonl"), b"{\"event\":2}\n")
        .expect("write recreated source");

    let recreated = wakeup.wait(Duration::from_secs(3));
    assert!(
        recreated.filesystem,
        "recreated root write did not wake the watcher"
    );
    assert!(recreated.source_watch.routes.contains_key(&route));
    let counters = watcher.runtime.snapshot();
    assert_eq!(counters.forced_rearms, 1);
    assert!(counters.registration_attempts > attempts_before);
}

#[cfg(target_os = "linux")]
#[test]
fn clean_forced_rearm_emits_no_catalog_routes() {
    let temp = tempfile::tempdir().expect("create watcher fixture");
    let data_root = temp.path().join("data");
    let provider_root = temp.path().join("provider");
    fs::create_dir_all(&data_root).expect("create data root");
    fs::create_dir_all(&provider_root).expect("create provider root");
    fs::write(provider_root.join("history.jsonl"), b"{\"event\":1}\n")
        .expect("write initial source");
    let catalog = watch_catalog([catalog_route(
        CaptureProvider::Codex,
        provider_root,
        "codex_session_jsonl_tree",
    )]);
    let wakeup = Arc::new(DaemonWakeup::default());
    let mut watcher = DaemonFileWatcher::start(&data_root, wakeup, catalog_owner(catalog))
        .expect("start watcher");

    let (affected, registration) = watcher.reconcile_roots(true);
    registration.expect("force native watcher re-registration");

    assert!(affected.routes.is_empty());
    assert!(affected.reconcile.is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn partial_incremental_and_replacement_registration_failures_poll_until_restored() {
    let temp = tempfile::tempdir().expect("create watcher fixture");
    let data_root = temp.path().join("data");
    let first_root = temp.path().join("first");
    let second_root = temp.path().join("second");
    let first_file = first_root.join("history.jsonl");
    let second_file = second_root.join("history.jsonl");
    fs::create_dir_all(&data_root).unwrap();
    fs::create_dir_all(&first_root).unwrap();
    fs::create_dir_all(&second_root).unwrap();
    fs::write(&first_file, b"one\n").unwrap();
    fs::write(&second_file, b"one\n").unwrap();
    let initial = watch_catalog([catalog_route(
        CaptureProvider::Codex,
        first_file.clone(),
        "codex_history_jsonl",
    )]);
    let expanded = watch_catalog([
        catalog_route(CaptureProvider::Codex, first_file, "codex_history_jsonl"),
        catalog_route(
            CaptureProvider::Claude,
            second_file,
            "claude_projects_jsonl_tree",
        ),
    ]);
    let expected_routes = expanded.route_ids().cloned().collect::<BTreeSet<_>>();
    let owner = catalog_owner(initial);
    let wakeup = Arc::new(DaemonWakeup::default());
    let mut watcher =
        DaemonFileWatcher::start(&data_root, wakeup, owner.clone()).expect("start watcher");
    owner.publish(expanded);

    let fail_registration = Arc::new(AtomicBool::new(true));
    let hook_failure = Arc::clone(&fail_registration);
    watcher.install_registration_attempt_hook(move |path| {
        if path == second_root && hook_failure.load(Ordering::SeqCst) {
            anyhow::bail!("injected dynamic registration failure");
        }
        Ok(())
    });

    for _ in 0..2 {
        let (affected, registration) = watcher.reconcile_roots(false);
        assert!(registration.is_err());
        assert_eq!(
            affected.routes.keys().cloned().collect::<BTreeSet<_>>(),
            expected_routes,
            "every degraded safety pass must poll the complete current catalog"
        );
    }

    fail_registration.store(false, Ordering::SeqCst);
    let (catch_up, registration) = watcher.reconcile_roots(false);
    registration.expect("restore dynamic registration coverage");
    assert_eq!(
        catch_up.routes.keys().cloned().collect::<BTreeSet<_>>(),
        expected_routes
    );
    let (healthy, registration) = watcher.reconcile_roots(false);
    registration.expect("healthy safety pass");
    assert!(healthy.routes.is_empty());

    let watched_before = watcher.runtime_snapshot().watched_roots;
    fail_registration.store(true, Ordering::SeqCst);
    for force_rearm in [true, false] {
        let (affected, registration) = watcher.reconcile_roots(force_rearm);
        assert!(registration.is_err());
        assert_eq!(
            affected.routes.keys().cloned().collect::<BTreeSet<_>>(),
            expected_routes,
            "partial replacement failure must poll every current route"
        );
        assert_eq!(
            watcher.runtime_snapshot().watched_roots,
            watched_before,
            "partial replacement must retain the complete old watcher"
        );
    }

    fail_registration.store(false, Ordering::SeqCst);
    let (replacement, registration) = watcher.reconcile_roots(false);
    registration.expect("retry complete replacement registration");
    assert!(replacement.routes.is_empty());
    assert_eq!(watcher.runtime_snapshot().watched_roots, watched_before);
    let (healthy, registration) = watcher.reconcile_roots(false);
    registration.expect("quiet pass after replacement recovery");
    assert!(healthy.routes.is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn callback_channel_overflow_holds_active_wait_until_rearmed_recovery() -> Result<()> {
    use std::{
        collections::BTreeMap,
        sync::{atomic::AtomicUsize, Barrier},
        time::Instant,
    };

    use crate::source_backed_refresh_coordinator::{
        publish_authoritative_empty_generation_with_route_results_for_test, CoreRefreshEngine,
        SourceBackedRefreshExecution, SourceBackedRefreshExecutor, SourceBackedRefreshRouteResult,
    };

    let temp = tempfile::tempdir()?;
    let data_root = temp.path().join("data");
    let provider_root = temp.path().join("provider");
    let provider_file = provider_root.join("history.jsonl");
    fs::create_dir_all(&data_root)?;
    fs::create_dir_all(&provider_root)?;
    fs::write(&provider_file, b"before\n")?;
    let catalog = watch_catalog([catalog_route(
        CaptureProvider::Codex,
        provider_file.clone(),
        "codex_history_jsonl",
    )]);
    let route = catalog.route_ids().next().expect("one route").clone();
    let execution_published = Arc::new(Barrier::new(2));
    let execution_release = Arc::new(Barrier::new(2));
    let block_once = Arc::new(AtomicBool::new(true));
    let launches = Arc::new(AtomicUsize::new(0));
    let refresh_route = route.clone();
    let entered = Arc::clone(&execution_published);
    let release = Arc::clone(&execution_release);
    let first = Arc::clone(&block_once);
    let launched = Arc::clone(&launches);
    let executor: Arc<dyn SourceBackedRefreshExecutor> =
        Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
            launched.fetch_add(1, Ordering::SeqCst);
            let publication = publish_authoritative_empty_generation_with_route_results_for_test(
                execution.index_root,
                execution.request_id,
                execution.operation,
                execution.admitted_refresh().publication_scope().clone(),
                execution.explicit_source_catalog.cloned(),
                Some(vec![SourceBackedRefreshRouteResult::succeeded(
                    refresh_route.as_str().to_owned(),
                    true,
                )]),
            )?;
            if first.swap(false, Ordering::SeqCst) {
                entered.wait();
                release.wait();
            }
            Ok(publication)
        });
    let admitted_route = route.clone();
    let coordinator = Arc::new(CoreRefreshEngine::with_runtime_for_test(
        executor,
        Arc::new(move |_, _| {
            Ok(BTreeMap::from([(
                admitted_route.clone(),
                Some("ab".repeat(32)),
            )]))
        }),
        Arc::new(|_, _| Ok(())),
    ));
    coordinator.install_watch_catalog(catalog.clone());

    let wakeup = Arc::new(DaemonWakeup::default());
    let pressure_coordinator = Arc::clone(&coordinator);
    wakeup.install_source_watch_pressure_sink(Arc::new(move |watermark| {
        pressure_coordinator.fence_watch_uncertainty(watermark);
    }));
    let sink_coordinator = Arc::clone(&coordinator);
    wakeup.install_source_watch_sink(Arc::new(move |batch| {
        if let Some(watermark) = batch.reconcile {
            sink_coordinator.fence_watch_uncertainty(watermark);
        } else {
            sink_coordinator.record_watch_routes_with_members(
                batch
                    .routes
                    .iter()
                    .map(|(route, watermark)| (route.clone(), *watermark)),
                batch.members.clone(),
                0,
            );
        }
    }));
    let mut watcher = DaemonFileWatcher::start(
        &data_root,
        Arc::clone(&wakeup),
        catalog_owner(catalog.clone()),
    )?;
    let worker_entered = Arc::new(Barrier::new(2));
    let worker_release = Arc::new(Barrier::new(2));
    let hook_entered = Arc::clone(&worker_entered);
    let hook_release = Arc::clone(&worker_release);
    wakeup.install_before_source_watch_sink_dispatch_hook(Arc::new(move || {
        hook_entered.wait();
        hook_release.wait();
    }));

    let request_id = "019fcaaa-0000-7000-8000-000000000690";
    let admitted = coordinator
        .handle_ipc_request(
            &data_root,
            &serde_json::json!({
                "schema_version": 1,
                "op": "source_refresh_request",
                "request_id": request_id,
                "mode": "wait",
                "operation": "refresh",
                "fresh_after_admitted_snapshot": true,
            }),
        )?
        .expect("wait admission");
    assert_eq!(admitted["request_state"], "admission_pending");
    let runner = Arc::clone(&coordinator);
    let run_root = data_root.clone();
    let run = thread::spawn(move || runner.run_next(&run_root).expect("active wait run"));
    execution_published.wait();

    fs::write(&provider_file, b"changed during active refresh\n")?;
    worker_entered.wait();
    for index in 0..1_024 {
        fs::write(
            provider_root.join(format!("overflow-{index}.jsonl")),
            b"event\n",
        )?;
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while watcher.runtime_snapshot().ingress_overflows == 0 && Instant::now() < deadline {
        thread::yield_now();
    }
    let overflowed = watcher.runtime_snapshot().ingress_overflows > 0;
    let first_boundary = coordinator.watch_uncertainty_watermark();
    execution_release.wait();
    let stale = run.join().expect("join active wait");
    worker_release.wait();
    assert!(overflowed, "real callback channel did not overflow");
    let first_boundary = first_boundary.expect("overflow must synchronously fence Core");
    assert_eq!(stale.job["request_state"], "running");
    assert_eq!(stale.job["progress"]["phase"], "watch_recovery");

    let newer_boundary = EventWatermark::new(
        first_boundary.watcher_epoch,
        first_boundary.sequence.saturating_add(1),
    );
    coordinator.fence_watch_uncertainty(newer_boundary);
    watcher.reconcile_roots(true).1?;
    assert!(!coordinator.complete_watch_uncertainty_recovery(
        &data_root,
        catalog.clone(),
        first_boundary,
        0,
    )?);
    let mut recovered_coverage = false;
    for _ in 0..8 {
        let boundary = coordinator
            .watch_uncertainty_watermark()
            .expect("uncertainty remains fenced until recovery");
        watcher.reconcile_roots(true).1?;
        if coordinator.complete_watch_uncertainty_recovery(
            &data_root,
            catalog.clone(),
            boundary,
            0,
        )? {
            recovered_coverage = true;
            break;
        }
        let _ = wakeup.wait(Duration::from_millis(50));
    }
    assert!(recovered_coverage, "callback pressure never quiesced");
    assert_eq!(
        coordinator.status(request_id).unwrap()["request_state"],
        "admission_pending"
    );
    let mut recovered = None;
    for _ in 0..64 {
        if let Some(boundary) = coordinator.watch_uncertainty_watermark() {
            watcher.reconcile_roots(true).1?;
            let _ = coordinator.complete_watch_uncertainty_recovery(
                &data_root,
                catalog.clone(),
                boundary,
                0,
            )?;
            let _ = wakeup.wait(Duration::from_millis(50));
            continue;
        }
        let Some(rerun) = coordinator.run_next(&data_root) else {
            thread::yield_now();
            continue;
        };
        if rerun.job["request_state"] == "published" {
            recovered = Some(rerun);
            break;
        }
        assert_eq!(rerun.job["request_state"], "running");
        assert_eq!(rerun.job["progress"]["phase"], "watch_recovery");
    }
    let recovered = recovered.expect("overflow recovery and successor publication");
    assert!(!recovered.failed, "{:#}", recovered.job);
    assert_eq!(recovered.job["request_state"], "published");
    assert!(launches.load(Ordering::SeqCst) >= 2);
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn forced_rearm_emits_only_the_route_mutated_during_registration_overlap() {
    let temp = tempfile::tempdir().expect("create watcher fixture");
    let data_root = temp.path().join("data");
    let provider_root = temp.path().join("provider");
    let provider_file = provider_root.join("history.jsonl");
    let healthy_root = temp.path().join("healthy");
    let healthy_file = healthy_root.join("history.jsonl");
    fs::create_dir_all(&data_root).expect("create data root");
    fs::create_dir_all(&provider_root).expect("create provider root");
    fs::create_dir_all(&healthy_root).expect("create healthy root");
    fs::write(&provider_file, b"{\"event\":1}\n").expect("write initial source");
    fs::write(&healthy_file, b"{\"event\":1}\n").expect("write healthy source");
    let catalog = watch_catalog([
        catalog_route(
            CaptureProvider::Codex,
            provider_root.clone(),
            "codex_session_jsonl_tree",
        ),
        catalog_route(
            CaptureProvider::Claude,
            healthy_root,
            "claude_projects_jsonl_tree",
        ),
    ]);
    let route = catalog
        .routes_overlapping_path(&provider_file)
        .into_iter()
        .next()
        .expect("mutated route");
    let healthy_route = catalog
        .routes_overlapping_path(&healthy_file)
        .into_iter()
        .next()
        .expect("healthy route");
    let wakeup = Arc::new(DaemonWakeup::default());
    let mut watcher =
        DaemonFileWatcher::start(&data_root, Arc::clone(&wakeup), catalog_owner(catalog))
            .expect("start watcher");
    let mutation_observed = Arc::new(AtomicBool::new(false));
    let hook_observed = Arc::clone(&mutation_observed);
    let hook_root = provider_root.clone();
    let hook_file = provider_file.clone();
    watcher.install_rearm_overlap_hook(move |watched| {
        if watched == hook_root && !hook_observed.swap(true, Ordering::SeqCst) {
            fs::write(&hook_file, b"{\"event\":2}\n")
                .expect("mutate source during forced-rearm overlap");
        }
    });

    let (affected, registration) = watcher.reconcile_roots(true);
    registration.expect("force native watcher re-registration");

    assert!(mutation_observed.load(Ordering::SeqCst));
    assert_eq!(
        fs::read(&provider_file).expect("read gap mutation"),
        b"{\"event\":2}\n"
    );
    assert!(affected.routes.is_empty());
    let observed = wakeup.wait(Duration::from_secs(3));
    assert!(observed.filesystem, "overlap mutation did not wake watcher");
    assert!(
        observed.source_watch.routes.contains_key(&route),
        "{:#?}",
        observed.source_watch
    );
    assert!(!observed.source_watch.routes.contains_key(&healthy_route));
}

#[test]
fn rescan_and_backend_errors_require_catalog_reconciliation_and_rearm() {
    let data_root = Path::new("/tmp/ctx-data");
    let daemon_root = data_root.join("daemon");
    let provider_file = PathBuf::from("/tmp/provider/session.jsonl");
    let catalog = watch_catalog([catalog_route(
        CaptureProvider::Codex,
        provider_file.clone(),
        "codex_history_jsonl",
    )]);
    let authority = RwLock::new(watch_authority(data_root, catalog));
    let counters = Mutex::new(WatchCounters::default());
    let pathless = record_watch_event(
        &authority,
        &counters,
        data_root,
        &daemon_root,
        Ok(NativeWatchEvent::ordinary(Vec::new())),
        EventWatermark::new(3, 0),
    );
    assert_eq!(pathless.reconcile, Some(EventWatermark::new(3, 0)));
    assert!(pathless.rearm);
    let later_exact = record_watch_event(
        &authority,
        &counters,
        data_root,
        &daemon_root,
        Ok(NativeWatchEvent::ordinary(vec![provider_file])),
        EventWatermark::new(3, 1),
    );
    assert_eq!(later_exact.routes.len(), 1);
    assert!(later_exact.reconcile.is_none());
    let rescan = NativeWatchEvent::rescan(vec![
        data_root.join("catalogs/explicit-sources/catalog.lock")
    ]);

    let rescan_batch = record_watch_event(
        &authority,
        &counters,
        data_root,
        &daemon_root,
        Ok(rescan),
        EventWatermark::new(3, 2),
    );
    assert_eq!(rescan_batch.reconcile, Some(EventWatermark::new(3, 2)));
    assert!(rescan_batch.rearm);

    let error_batch = record_watch_event(
        &authority,
        &counters,
        data_root,
        &daemon_root,
        Err(NativeWatchError),
        EventWatermark::new(3, 3),
    );
    assert_eq!(error_batch.reconcile, Some(EventWatermark::new(3, 3)));
    assert!(error_batch.rearm);
    let counters = counters.lock().unwrap();
    assert_eq!(counters.rescan_notifications, 1);
    assert_eq!(counters.backend_errors, 1);
}

#[test]
fn sqlite_companion_files_are_exact_catalog_targets() {
    let catalog = watch_catalog([catalog_route(
        CaptureProvider::OpenCode,
        PathBuf::from("/tmp/history.sqlite"),
        "opencode_sqlite",
    )]);
    assert_eq!(
        catalog
            .routes_overlapping_path(Path::new("/tmp/history.sqlite-wal"))
            .len(),
        1
    );
    assert_eq!(
        catalog
            .routes_overlapping_path(Path::new("/tmp/history.sqlite-shm"))
            .len(),
        1
    );
    assert!(catalog
        .routes_overlapping_path(Path::new("/tmp/unrelated.sqlite-wal"))
        .is_empty());
}

#[test]
fn missing_target_uses_exact_nearest_ancestor_without_sibling_matching() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp
        .path()
        .join("missing")
        .join("nested")
        .join("history.jsonl");
    let catalog = watch_catalog([catalog_route(
        CaptureProvider::Codex,
        missing.clone(),
        "codex_history_jsonl",
    )]);
    let roots = ctx_daemon_runtime::watch_roots(catalog.target_paths());
    assert_eq!(roots, BTreeMap::from([(temp.path().to_path_buf(), false)]));
    assert_eq!(
        catalog
            .routes_overlapping_path(&temp.path().join("missing"))
            .len(),
        1
    );
    assert!(catalog
        .routes_overlapping_path(
            &temp
                .path()
                .join("unrelated")
                .join("nested")
                .join("history.jsonl"),
        )
        .is_empty());
}

#[test]
fn core_owned_writes_do_not_retrigger_provider_refresh_or_increment_work_counters() {
    let data_root = Path::new("/tmp/ctx-data");
    let daemon_root = data_root.join("daemon");
    let targets = RwLock::new(watch_authority(
        data_root,
        watch_catalog([catalog_route(
            CaptureProvider::Codex,
            PathBuf::from("/tmp/provider/session.jsonl"),
            "codex_history_jsonl",
        )]),
    ));
    let counters = Mutex::new(WatchCounters::default());
    let event = |path: &Path| NativeWatchEvent::requiring_rearm(vec![path.to_path_buf()]);

    assert!(record_watch_event(
        &targets,
        &counters,
        data_root,
        &daemon_root,
        Ok(event(&daemon_root.join("wakeup.json"))),
        EventWatermark::new(1, 1),
    )
    .is_empty());
    let access = NativeWatchEvent::ignored(
        vec![data_root.join("config.toml")],
        NativeWatchIgnore::Access,
    );
    assert!(record_watch_event(
        &targets,
        &counters,
        data_root,
        &daemon_root,
        Ok(access),
        EventWatermark::new(1, 3),
    )
    .is_empty());
    assert_eq!(counters.lock().unwrap().raw_events, 0);
    let access_time = NativeWatchEvent::ignored(
        vec![data_root.join("config.toml")],
        NativeWatchIgnore::AccessTime,
    );
    assert!(record_watch_event(
        &targets,
        &counters,
        data_root,
        &daemon_root,
        Ok(access_time),
        EventWatermark::new(1, 4),
    )
    .is_empty());
    let request_overlay = event(
        &data_root
            .join("catalogs")
            .join("explicit-sources")
            .join("catalog.lock"),
    );
    assert!(record_watch_event(
        &targets,
        &counters,
        data_root,
        &daemon_root,
        Ok(request_overlay),
        EventWatermark::new(1, 5),
    )
    .is_empty());
    let close_write = NativeWatchEvent::ordinary(vec![data_root.join("config.toml")]);
    assert!(!record_watch_event(
        &targets,
        &counters,
        data_root,
        &daemon_root,
        Ok(close_write),
        EventWatermark::new(1, 6),
    )
    .is_empty());
    assert!(!record_watch_event(
        &targets,
        &counters,
        data_root,
        &daemon_root,
        Ok(event(&data_root.join("config.toml"))),
        EventWatermark::new(1, 7),
    )
    .is_empty());
    assert_eq!(counters.lock().unwrap().raw_events, 2);
    let invalidated = record_watch_event(
        &targets,
        &counters,
        data_root,
        &daemon_root,
        Ok(event(data_root)),
        EventWatermark::new(1, 8),
    );
    assert_eq!(invalidated.reconcile, Some(EventWatermark::new(1, 8)));
    assert!(invalidated.rearm);
}

#[test]
fn authoritative_changes_and_dynamic_discovery_remain_relevant() {
    let data_root = Path::new("/tmp/ctx-data");
    let daemon_root = data_root.join("daemon");
    let provider_file = PathBuf::from("/tmp/provider/session.jsonl");
    let sqlite = PathBuf::from("/tmp/provider/history.sqlite");
    let dynamic_source = PathBuf::from("/tmp/home/.codex/sessions");
    let catalog_root = data_root.join("catalogs").join("explicit-sources");
    let targets = RwLock::new(watch_authority(
        data_root,
        watch_catalog([
            catalog_route(
                CaptureProvider::Codex,
                provider_file.clone(),
                "codex_history_jsonl",
            ),
            catalog_route(CaptureProvider::OpenCode, sqlite.clone(), "opencode_sqlite"),
            catalog_route(
                CaptureProvider::Codex,
                dynamic_source,
                "codex_session_jsonl_tree",
            ),
        ]),
    ));
    let counters = Mutex::new(WatchCounters::default());
    let sequence = std::cell::Cell::new(0_u64);
    let relevant = |event: NativeWatchEvent| {
        sequence.set(sequence.get().saturating_add(1));
        !record_watch_event(
            &targets,
            &counters,
            data_root,
            &daemon_root,
            Ok(event),
            EventWatermark::new(1, sequence.get()),
        )
        .is_empty()
    };

    assert!(!relevant(NativeWatchEvent::ordinary(vec![
        catalog_root.join("catalog-00000000000000000002.json"),
    ]),));
    assert!(relevant(NativeWatchEvent::ordinary(vec![
        provider_file.clone()
    ]),));
    assert!(relevant(NativeWatchEvent::requiring_rearm(vec![
        PathBuf::from("/tmp/home/.codex")
    ]),));
    assert!(relevant(NativeWatchEvent::ordinary(vec![PathBuf::from(
        "/tmp/provider/history.sqlite-wal",
    )]),));
    assert!(relevant(NativeWatchEvent::ordinary(vec![
        data_root.join("config.toml")
    ]),));
    let mixed = record_watch_event(
        &targets,
        &counters,
        data_root,
        &daemon_root,
        Ok(NativeWatchEvent::requiring_rearm(vec![
            PathBuf::from("/tmp/provider/session.tmp"),
            provider_file,
        ])),
        EventWatermark::new(1, 98),
    );
    assert_eq!(mixed.reconcile, Some(EventWatermark::new(1, 98)));
    assert!(mixed.rearm);
    assert!(!record_watch_event(
        &targets,
        &counters,
        data_root,
        &daemon_root,
        Err(NativeWatchError),
        EventWatermark::new(1, 99),
    )
    .is_empty());
}
