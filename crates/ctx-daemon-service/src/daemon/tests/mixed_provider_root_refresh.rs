use super::*;
use std::sync::{mpsc, Mutex};

struct MixedFixture {
    data_root: PathBuf,
    files: [PathBuf; 2],
    routes: [SourceRouteIdentity; 2],
    replacement: SourceBackedWatchCatalog,
    engine: Arc<CoreRefreshEngine>,
    watch: DaemonWatchRuntime,
    fault: Arc<AtomicUsize>,
    scopes: Arc<Mutex<Vec<SourceBackedRefreshScope>>>,
    roots: Arc<Mutex<Vec<AppliedProviderRoot>>>,
    entered: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
    block: Arc<std::sync::atomic::AtomicBool>,
    _temp: tempfile::TempDir,
}

impl MixedFixture {
    fn new() -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let data_root = temp.path().join("data");
        let files = [temp.path().join("a.jsonl"), temp.path().join("b.jsonl")];
        for file in &files {
            fs::write(file, b"retained record\n")?;
        }
        let mut registry = SourceBackedProviderRegistry::new();
        let mut routes = Vec::new();
        let mut sources = Vec::new();
        let mut original_roots = Vec::new();
        for (i, file) in files.iter().enumerate() {
            let (provider, source_format) = if i == 0 {
                (CaptureProvider::Codex, "codex_history_jsonl")
            } else {
                (CaptureProvider::Claude, "claude_projects_jsonl_tree")
            };
            let route = SourceBackedRoute::automatic(
                ProviderSource {
                    provider,
                    path: file.clone(),
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
            )?;
            let identity = route.metadata().route_identity.clone().unwrap();
            registry.register(route);
            original_roots.push(AppliedProviderRoot::new(
                ProviderRootDefinition {
                    id: format!("root-{i}"),
                    provider,
                    path: file.clone(),
                    group: Some("original".to_owned()),
                    kind: None,
                },
                vec![identity.clone()],
            )?);
            routes.push(identity);
            sources.push(SourceKey::derive(
                if i == 0 { "codex" } else { "claude" },
                source_format,
                format!("session-{i}"),
                1,
                SourceAnchor::CatalogLineage([0x60 + i as u8; 32]),
            )?);
        }
        let mut replacement_registry = registry.clone();
        registry.set_applied_provider_roots(
            true,
            digest(&original_roots),
            original_roots.clone(),
        )?;
        let original = registry.watch_catalog();
        let replacement_roots = original_roots
            .iter()
            .map(|root| {
                let mut definition = root.definition().clone();
                definition.group = Some("replacement".to_owned());
                AppliedProviderRoot::new(definition, root.routes().to_vec())
            })
            .collect::<ctx_history_index::Result<Vec<_>>>()?;
        replacement_registry.set_applied_provider_roots(
            true,
            digest(&replacement_roots),
            replacement_roots,
        )?;
        let replacement = replacement_registry.watch_catalog();
        publish_mixed(
            &source_backed_index_root(&data_root),
            &routes,
            &sources,
            &files,
            &original_roots,
        )?;
        let roots = Arc::new(Mutex::new(original_roots));
        let fault = Arc::new(AtomicUsize::new(3));
        let scopes = Arc::new(Mutex::new(Vec::new()));
        let block = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (entered_tx, entered) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let executor_roots = Arc::clone(&roots);
        let executor_fault = Arc::clone(&fault);
        let executor_scopes = Arc::clone(&scopes);
        let executor_block = Arc::clone(&block);
        let executor_routes = routes.clone();
        let executor_files = files.clone();
        let executor = Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
            let scope = execution.admitted_refresh().publication_scope();
            executor_scopes.lock().unwrap().push(scope.clone());
            let roots = executor_roots.lock().unwrap().clone();
            if executor_block.swap(false, Ordering::SeqCst) {
                entered_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
            }
            let selected = match scope {
                SourceBackedRefreshScope::All => executor_routes.iter().cloned().collect(),
                SourceBackedRefreshScope::Exact(routes) => routes,
            };
            let fault = executor_fault.load(Ordering::SeqCst);
            if selected.contains(&executor_routes[0]) && fault == 2 {
                anyhow::bail!("stable internal fixture failure");
            }
            publish_mixed(
                execution.index_root,
                &executor_routes,
                &sources,
                &executor_files,
                &roots,
            )?;
            let pin = Arc::new(VerifiedIndex::open_pinned(execution.index_root)?);
            let current = SourceBackedRefreshCurrent::from_sources(&pin.manifest().sources, 0)?;
            Ok(SourceBackedRefreshPublication {
                generation_id: pin.generation_id().to_owned(),
                published_explicit_source_catalog: execution.explicit_source_catalog.cloned(),
                unsupported_routes: 0,
                certified_source_count: current.source_count,
                certified_source_bytes: current.certified_source_bytes,
                current,
                timings: SourceBackedRefreshTimings::default(),
                route_results: selected
                    .into_iter()
                    .map(|route| {
                        if route == executor_routes[0] && fault < 2 {
                            SourceBackedRefreshRouteResult::failed(
                                route.as_str().to_owned(),
                                if fault == 0 {
                                    "unreadable"
                                } else {
                                    "source_changed"
                                }
                                .to_owned(),
                                true,
                            )
                        } else {
                            SourceBackedRefreshRouteResult::succeeded(
                                route.as_str().to_owned(),
                                true,
                            )
                        }
                    })
                    .collect(),
                zero_source_authority: Vec::new(),
                catalog_route_bindings: Vec::new(),
                verified_index: Some(pin),
            })
        });
        let observations = original.clone();
        let engine = Arc::new(CoreRefreshEngine::with_runtime_for_test(
            executor,
            Arc::new(move |_, _| {
                Ok(observations
                    .route_ids()
                    .map(|route| (route.clone(), observations.certify_route_observation(route)))
                    .collect())
            }),
            Arc::new(crate::paths_status::write_daemon_job_status),
        ));
        engine.install_watch_catalog(original.clone());
        let watch = DaemonWatchRuntime::new(
            Arc::new(DaemonWakeup::default()),
            &crate::test_support::CONFIG,
        );
        watch.catalog.publish(original);
        Ok(Self {
            data_root,
            files,
            routes: routes.try_into().unwrap(),
            replacement,
            engine,
            watch,
            fault,
            scopes,
            roots,
            entered,
            release,
            block,
            _temp: temp,
        })
    }

    fn fail_a(&self, fault: usize) -> Result<serde_json::Value> {
        self.fault.store(fault, Ordering::SeqCst);
        self.engine.schedule_startup_route_reconciliation(
            [self.routes[0].clone()],
            EventWatermark::new(u64::MAX, 1),
            0,
        );
        assert!(self
            .engine
            .enqueue_next_dirty_route(&self.data_root, source_route_ledger_now_ms())?);
        Ok(self.engine.run_next(&self.data_root).unwrap().job)
    }

    fn pending_config(&mut self) {
        self.watch.catalog.publish(self.replacement.clone());
        self.engine.install_watch_catalog(self.replacement.clone());
    }

    fn repair_config(&self) -> Result<()> {
        let mut roots = self.roots.lock().unwrap();
        *roots = roots
            .iter()
            .map(|root| {
                let mut definition = root.definition().clone();
                definition.group = Some("replacement".to_owned());
                AppliedProviderRoot::new(definition, root.routes().to_vec())
            })
            .collect::<ctx_history_index::Result<Vec<_>>>()?;
        Ok(())
    }

    fn wake(&mut self, routes: &[usize], sequence: u64) {
        self.engine.record_watch_routes(
            routes.iter().map(|i| {
                (
                    self.routes[*i].clone(),
                    EventWatermark::new(u64::MAX, sequence),
                )
            }),
            0,
        );
        self.watch.enqueue_pending_provider_root_refresh(
            &self.data_root,
            Some(&self.engine),
            source_route_ledger_now_ms(),
        );
    }

    fn run_scheduled(&self) -> Result<ctx_history_refresh::RefreshRun> {
        self.engine
            .enqueue_next_scheduled_refresh(&self.data_root, source_route_ledger_now_ms())?;
        Ok(self
            .engine
            .run_next(&self.data_root)
            .expect("eligible route work"))
    }
}

fn digest(roots: &[AppliedProviderRoot]) -> String {
    provider_source_config_digest(
        true,
        &roots
            .iter()
            .map(|root| root.definition().clone())
            .collect::<Vec<_>>(),
    )
}

fn publish_mixed(
    index_root: &Path,
    routes: &[SourceRouteIdentity],
    sources: &[SourceKey],
    files: &[PathBuf],
    roots: &[AppliedProviderRoot],
) -> Result<()> {
    let mut writer = GenerationWriter::open(index_root, WriterOptions::default())?
        .into_writer()
        .map_err(crate::committed_generation_recovery_error)?;
    writer.set_applied_provider_roots(true, digest(roots), roots.to_vec())?;
    for (source, file) in sources.iter().zip(files) {
        let bytes = fs::read(file)?;
        writer.begin_source(source.clone())?;
        writer.add_core_record(observation_fixture_record(
            source,
            String::from_utf8(bytes.clone())?,
        ))?;
        writer.certify_source(observation_fixture_certificate(source, &bytes))?;
    }
    let mut snapshots = routes
        .iter()
        .zip(sources)
        .map(|(route, source)| SourceRouteSnapshot::present(route.clone(), vec![source.clone()]))
        .collect::<ctx_history_index::Result<Vec<_>>>()?;
    snapshots.sort_by(|left, right| left.route_identity().cmp(right.route_identity()));
    writer.set_present_source_routes(snapshots)?;
    commit_source_backed_generation_for_test(writer)?;
    Ok(())
}

#[test]
fn pending_root_replacement_blocked_a_stays_idle_while_b_is_due() -> Result<()> {
    let mut fixture = MixedFixture::new()?;
    let failed = fixture.fail_a(0)?;
    assert_eq!(failed["structured_outcome"]["retryable"], false);
    fixture.pending_config();
    for sequence in 2..5 {
        fixture.wake(&[1], sequence);
        let run = fixture.run_scheduled()?;
        assert_eq!(
            run.scope,
            SourceBackedRefreshScope::exact([fixture.routes[1].clone()])
        );
        assert!(!run.failed, "{:#}", run.job);
        assert!(!fixture.engine.has_scheduled_route_work());
        assert_eq!(
            VerifiedIndex::open_pinned(source_backed_index_root(&fixture.data_root))?
                .manifest()
                .indexed_documents,
            2
        );
    }
    Ok(())
}

#[test]
fn pending_root_replacement_backoff_a_keeps_deadline_while_b_is_due() -> Result<()> {
    let mut fixture = MixedFixture::new()?;
    let failed = fixture.fail_a(1)?;
    assert_eq!(failed["structured_outcome"]["retryable"], true);
    let deadline = fixture.engine.next_dirty_route_due_in_ms(0).unwrap();
    fixture.pending_config();
    fixture.wake(&[1], 2);
    let run = fixture.run_scheduled()?;
    assert_eq!(
        run.scope,
        SourceBackedRefreshScope::exact([fixture.routes[1].clone()])
    );
    assert_eq!(fixture.engine.next_dirty_route_due_in_ms(0), Some(deadline));
    Ok(())
}

#[test]
fn pending_root_replacement_paused_a_repair_is_not_starved_by_b() -> Result<()> {
    let mut fixture = MixedFixture::new()?;
    fixture.fail_a(2)?;
    let paused = fixture.fail_a(2)?;
    assert_eq!(paused["automatic_retry"]["state"], "paused");
    fixture.pending_config();
    fs::write(&fixture.files[0], b"repaired record\n")?;
    fixture.repair_config()?;
    fixture.fault.store(3, Ordering::SeqCst);
    fixture.wake(&[0, 1], 2);
    let run = fixture.run_scheduled()?;
    assert_eq!(run.scope, SourceBackedRefreshScope::All);
    assert!(!run.failed, "{:#}", run.job);
    assert_eq!(fixture.scopes.lock().unwrap().len(), 3);
    assert!(!fixture.engine.has_scheduled_route_work());
    let pin = VerifiedIndex::open_pinned(source_backed_index_root(&fixture.data_root))?;
    assert_eq!(
        pin.manifest().provider_root_config_digest(),
        fixture.replacement.provider_root_config_digest().unwrap()
    );
    Ok(())
}

#[test]
fn pending_root_replacement_exact_b_does_not_consume_paused_a_config_demand() -> Result<()> {
    let mut fixture = MixedFixture::new()?;
    fixture.fail_a(2)?;
    let paused = fixture.fail_a(2)?;
    assert_eq!(paused["automatic_retry"]["state"], "paused");
    fixture.watch.reconcile_catalog_and_route_authority_with(
        &fixture.data_root,
        Some(&fixture.engine),
        WatchCatalogReconcileTrigger::CatalogControl(EventWatermark::new(u64::MAX, 2)),
        false,
        |_| Ok(fixture.replacement.clone()),
        DaemonFileWatcher::start,
    );
    fixture.engine.schedule_startup_route_reconciliation(
        [fixture.routes[1].clone()],
        EventWatermark::new(u64::MAX, 2),
        0,
    );
    fixture.watch.enqueue_pending_provider_root_refresh(
        &fixture.data_root,
        Some(&fixture.engine),
        source_route_ledger_now_ms(),
    );
    let healthy = fixture.run_scheduled()?;
    assert_eq!(
        healthy.scope,
        SourceBackedRefreshScope::exact([fixture.routes[1].clone()])
    );
    assert!(!healthy.failed, "{:#}", healthy.job);
    assert!(fixture.watch.provider_root_refresh_pending_for_test());
    fixture.watch.enqueue_pending_provider_root_refresh(
        &fixture.data_root,
        Some(&fixture.engine),
        source_route_ledger_now_ms(),
    );
    assert!(!fixture.engine.has_pending_request());
    assert!(fixture.watch.provider_root_refresh_pending_for_test());
    assert_eq!(fixture.scopes.lock().unwrap().len(), 3);

    fixture.repair_config()?;
    fs::write(&fixture.files[0], b"repaired record\n")?;
    fixture.fault.store(3, Ordering::SeqCst);
    fixture.wake(&[0], 3);
    let repaired = fixture.run_scheduled()?;
    assert_eq!(repaired.scope, SourceBackedRefreshScope::All);
    assert!(!repaired.failed, "{:#}", repaired.job);
    assert!(!fixture.watch.provider_root_refresh_pending_for_test());
    assert_eq!(fixture.scopes.lock().unwrap().len(), 4);
    let pin = VerifiedIndex::open_pinned(source_backed_index_root(&fixture.data_root))?;
    assert_eq!(
        pin.manifest().provider_root_config_digest(),
        fixture.replacement.provider_root_config_digest().unwrap()
    );
    Ok(())
}

#[test]
fn pending_root_replacement_config_during_running_request_is_not_consumed() -> Result<()> {
    let mut fixture = MixedFixture::new()?;
    fixture.engine.enqueue_periodic(&fixture.data_root)?;
    fixture.block.store(true, Ordering::SeqCst);
    std::thread::scope(|scope| -> Result<()> {
        let engine = Arc::clone(&fixture.engine);
        let root = fixture.data_root.clone();
        let running = scope.spawn(move || engine.run_next(&root).unwrap());
        fixture.entered.recv_timeout(StdDuration::from_secs(5))?;
        fixture.watch.reconcile_catalog_and_route_authority_with(
            &fixture.data_root,
            Some(&fixture.engine),
            WatchCatalogReconcileTrigger::CatalogControl(EventWatermark::new(u64::MAX, 2)),
            false,
            |_| Ok(fixture.replacement.clone()),
            DaemonFileWatcher::start,
        );
        let pending = fixture.watch.provider_root_refresh_pending_for_test();
        fixture.release.send(())?;
        let run = running.join().unwrap();
        assert!(!run.failed, "{:#}", run.job);
        assert!(
            pending,
            "running predecessor cannot consume a new config demand"
        );
        Ok(())
    })?;
    fixture.repair_config()?;
    fixture.engine.schedule_startup_route_reconciliation(
        fixture.routes.clone(),
        EventWatermark::new(u64::MAX, 2),
        0,
    );
    fixture.watch.enqueue_pending_provider_root_refresh(
        &fixture.data_root,
        Some(&fixture.engine),
        source_route_ledger_now_ms(),
    );
    let run = fixture.run_scheduled()?;
    assert_eq!(run.scope, SourceBackedRefreshScope::All);
    assert!(!run.failed, "{:#}", run.job);
    assert!(!fixture.watch.provider_root_refresh_pending_for_test());
    let pin = VerifiedIndex::open_pinned(source_backed_index_root(&fixture.data_root))?;
    assert_eq!(
        pin.manifest().provider_root_config_digest(),
        fixture.replacement.provider_root_config_digest().unwrap()
    );
    Ok(())
}
