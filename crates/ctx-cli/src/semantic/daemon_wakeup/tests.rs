use super::*;
use ctx_history_capture::{
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus, SourceBackedProviderRegistry, SourceBackedRoute, SourceBackedRouteDriver,
    SourceBackedSelectorAuthority,
};
use ctx_history_core::CaptureProvider;
use std::sync::Barrier;

use super::super::dirty_source_routes::DirtySourceRoutes;

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
fn source_watch_batches_coalesce_to_catalog_cardinality() {
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

    let pending = wakeup.lock_state();
    assert_eq!(pending.source_watch.routes.len(), 1);
    assert_eq!(
        pending.source_watch.routes.get(&route),
        Some(&EventWatermark::new(
            7,
            WATCH_EVENT_QUEUE_CAPACITY as u64 * 4
        ))
    );
    assert_eq!(
        pending.source_watch.reconcile,
        Some(EventWatermark::new(
            7,
            WATCH_EVENT_QUEUE_CAPACITY as u64 * 4
        ))
    );
    assert!(pending.source_watch.rearm);
    drop(pending);

    let wake = wakeup.wait(Duration::ZERO);
    assert_eq!(wake.source_watch.routes.len(), 1);
    assert!(wakeup.lock_state().source_watch.is_empty());
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
    use notify::event::DataChange;

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
    let ledger = Arc::new(Mutex::new(DirtySourceRoutes::default()));
    let sink_ledger = Arc::clone(&ledger);
    wakeup.install_source_watch_sink(Arc::new(move |batch| {
        let mut ledger = sink_ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (route, watermark) in &batch.routes {
            ledger.record_event(route.clone(), *watermark, 1_000);
        }
    }));

    let batch = record_and_observe_watch_event(
        &authority,
        &counters,
        &wakeup,
        data_root,
        &daemon_root,
        Ok(
            Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
                .add_path(provider_file),
        ),
        EventWatermark::new(13, 1),
    );

    assert_eq!(
        ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .seen_watermark(&route),
        Some(EventWatermark::new(13, 1)),
        "the publication handoff fence must see the event immediately"
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
fn full_watcher_ingress_fails_closed_into_catalog_reconciliation() {
    use notify::event::DataChange;

    let data_root = Path::new("/tmp/ctx-data");
    let counters = Mutex::new(WatchCounters::default());
    let wakeup = DaemonWakeup::default();
    let accepting_events = AtomicBool::new(true);
    let sequence = AtomicU64::new(0);
    let (sender, receiver) = mpsc::sync_channel(1);
    let event = || {
        Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
            .add_path(data_root.join("config.toml"))
    };

    forward_watch_event(
        data_root,
        &counters,
        &sender,
        &wakeup,
        &accepting_events,
        9,
        &sequence,
        Ok(event()),
    );
    forward_watch_event(
        data_root,
        &counters,
        &sender,
        &wakeup,
        &accepting_events,
        9,
        &sequence,
        Ok(event()),
    );

    let wake = wakeup.wait(Duration::ZERO);
    assert!(wake.filesystem);
    assert_eq!(wake.source_watch.reconcile, Some(EventWatermark::new(9, 2)));
    assert!(wake.source_watch.rearm);
    assert!(wake.source_watch.routes.is_empty());
    assert_eq!(counters.lock().unwrap().ingress_overflows, 1);
    match receiver.try_recv().expect("one event remains bounded") {
        WatchMessage::Event { watermark, .. } => {
            assert_eq!(watermark, EventWatermark::new(9, 1));
        }
        WatchMessage::Stop => panic!("unexpected stop message"),
    }
    assert!(receiver.try_recv().is_err());
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
    let attempts_before = watcher.lock_counters().registration_attempts;
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
    let counters = watcher.lock_counters();
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
    assert!(observed.source_watch.routes.contains_key(&route));
    assert!(!observed.source_watch.routes.contains_key(&healthy_route));
}

#[test]
fn rescan_and_backend_errors_require_catalog_reconciliation_and_rearm() {
    use notify::event::Flag;

    let data_root = Path::new("/tmp/ctx-data");
    let daemon_root = data_root.join("daemon");
    let authority = RwLock::new(watch_authority(data_root, watch_catalog([])));
    let counters = Mutex::new(WatchCounters::default());
    let rescan = Event::new(EventKind::Access(AccessKind::Read))
        .add_path(data_root.join("catalogs/explicit-sources/catalog.lock"))
        .set_flag(Flag::Rescan);

    let rescan_batch = record_watch_event(
        &authority,
        &counters,
        data_root,
        &daemon_root,
        Ok(rescan),
        EventWatermark::new(3, 1),
    );
    assert_eq!(rescan_batch.reconcile, Some(EventWatermark::new(3, 1)));
    assert!(rescan_batch.rearm);

    let error_batch = record_watch_event(
        &authority,
        &counters,
        data_root,
        &daemon_root,
        Err(notify::Error::generic("backend watch loss")),
        EventWatermark::new(3, 2),
    );
    assert_eq!(error_batch.reconcile, Some(EventWatermark::new(3, 2)));
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
    let roots = watch_roots(catalog.target_paths());
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
    let event = |path: &Path| {
        let mut event = Event::new(notify::EventKind::Any);
        event.paths.push(path.to_path_buf());
        event
    };

    assert!(record_watch_event(
        &targets,
        &counters,
        data_root,
        &daemon_root,
        Ok(event(&daemon_root.join("wakeup.json"))),
        EventWatermark::new(1, 1),
    )
    .is_empty());
    assert!(record_watch_event(
        &targets,
        &counters,
        data_root,
        &daemon_root,
        Ok(event(data_root)),
        EventWatermark::new(1, 2),
    )
    .is_empty());
    let mut access = event(&data_root.join("config.toml"));
    access.kind = EventKind::Access(AccessKind::Read);
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
    let mut access_time = event(&data_root.join("config.toml"));
    access_time.kind = EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime));
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
    let mut close_write = event(&data_root.join("config.toml"));
    close_write.kind = EventKind::Access(AccessKind::Close(AccessMode::Write));
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
}

#[test]
fn authoritative_changes_and_dynamic_discovery_remain_relevant() {
    use notify::event::{CreateKind, DataChange, RenameMode};

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
    let relevant = |kind, paths: &[&Path]| {
        let mut event = Event::new(kind);
        event
            .paths
            .extend(paths.iter().map(|path| path.to_path_buf()));
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

    assert!(relevant(
        EventKind::Access(AccessKind::Close(AccessMode::Write)),
        &[&data_root.join("config.toml")],
    ));
    assert!(!relevant(
        EventKind::Create(CreateKind::File),
        &[&catalog_root.join("catalog-00000000000000000002.json")],
    ));
    assert!(relevant(
        EventKind::Modify(ModifyKind::Data(DataChange::Content)),
        &[&provider_file],
    ));
    assert!(relevant(
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
        &[Path::new("/tmp/provider/session.tmp"), &provider_file],
    ));
    assert!(relevant(
        EventKind::Create(CreateKind::Folder),
        &[Path::new("/tmp/home/.codex")],
    ));
    assert!(relevant(
        EventKind::Modify(ModifyKind::Data(DataChange::Content)),
        &[Path::new("/tmp/provider/history.sqlite-wal")],
    ));
    assert!(!record_watch_event(
        &targets,
        &counters,
        data_root,
        &daemon_root,
        Err(notify::Error::generic("overflow")),
        EventWatermark::new(1, 99),
    )
    .is_empty());
}
