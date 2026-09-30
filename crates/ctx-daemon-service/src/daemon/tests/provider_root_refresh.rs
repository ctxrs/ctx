//! Scheduling fixtures: the executor models a failed replacement cohort's
//! retained generation, without interpreting or inventing provider-native data.
use super::*;
use ctx_history_capture_model::ProviderRootKind;
use ctx_history_index::{SourceRouteIdentity, VerifiedIndex};
use std::collections::BTreeMap;

#[path = "mixed_provider_root_refresh.rs"]
mod mixed;

struct ReplacementFixture {
    data_root: PathBuf,
    source_file: PathBuf,
    original: SourceBackedWatchCatalog,
    replacement: SourceBackedWatchCatalog,
    route: SourceRouteIdentity,
    engine: CoreRefreshEngine,
    watch: DaemonWatchRuntime,
    calls: Arc<AtomicUsize>,
    generation: String,
    _temp: tempfile::TempDir,
}

impl ReplacementFixture {
    fn failed() -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let data_root = temp.path().join("data");
        let source_file = temp.path().join("provider/history.jsonl");
        fs::create_dir_all(source_file.parent().unwrap())?;
        fs::write(&source_file, b"retained predecessor")?;
        let original_root = ProviderRootDefinition {
            id: "personal".to_owned(),
            provider: CaptureProvider::OpenHands,
            path: source_file.parent().unwrap().to_path_buf(),
            group: None,
            kind: Some(ProviderRootKind::OpenHandsLegacyPersistence),
        };
        let replacement_root = ProviderRootDefinition {
            kind: Some(ProviderRootKind::OpenHandsCurrentConversations),
            ..original_root.clone()
        };
        let original = catalog(&source_file, original_root.clone())?;
        let replacement = catalog(&source_file, replacement_root.clone())?;
        let route = replacement.route_ids().next().unwrap().clone();
        let source = SourceKey::derive(
            "openhands",
            "openhands_file_events",
            "session",
            1,
            SourceAnchor::CatalogLineage([0x5b; 32]),
        )?;
        let generation = publish(
            &source_backed_index_root(&data_root),
            &route,
            &source,
            &source_file,
            original_root,
        )?;
        let calls = Arc::new(AtomicUsize::new(0));
        let launches = Arc::clone(&calls);
        let executed_route = route.clone();
        let executed_file = source_file.clone();
        let executor = Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
            launches.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                execution.admitted_refresh().publication_scope(),
                SourceBackedRefreshScope::All,
                "pending root metadata must cross a full-refresh boundary"
            );
            let repaired = fs::read(&executed_file)? == b"repaired source";
            if repaired {
                publish(
                    execution.index_root,
                    &executed_route,
                    &source,
                    &executed_file,
                    replacement_root.clone(),
                )?;
            }
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
                route_results: vec![if repaired {
                    SourceBackedRefreshRouteResult::succeeded(
                        executed_route.as_str().to_owned(),
                        true,
                    )
                } else {
                    SourceBackedRefreshRouteResult::failed(
                        executed_route.as_str().to_owned(),
                        "unreadable".to_owned(),
                        true,
                    )
                }],
                zero_source_authority: Vec::new(),
                catalog_route_bindings: Vec::new(),
                verified_index: Some(pin),
            })
        });
        let admitted_route = route.clone();
        let engine = CoreRefreshEngine::with_runtime_for_test(
            executor,
            Arc::new(move |_, _| {
                Ok(BTreeMap::from([(
                    admitted_route.clone(),
                    Some("ab".repeat(32)),
                )]))
            }),
            Arc::new(crate::paths_status::write_daemon_job_status),
        );
        let watch = DaemonWatchRuntime::new(
            Arc::new(DaemonWakeup::default()),
            &crate::test_support::CONFIG,
        );
        let mut fixture = Self {
            _temp: temp,
            data_root,
            source_file,
            original,
            replacement,
            route,
            engine,
            watch,
            calls,
            generation,
        };
        fixture.reconcile(WatchCatalogReconcileTrigger::Startup, false);
        fixture.reconcile(
            WatchCatalogReconcileTrigger::CatalogControl(EventWatermark::new(u64::MAX, 1)),
            true,
        );
        fixture.engine.schedule_startup_route_reconciliation(
            [fixture.route.clone()],
            EventWatermark::new(u64::MAX, 1),
            0,
        );
        fixture.watch.enqueue_pending_provider_root_refresh(
            &fixture.data_root,
            Some(&fixture.engine),
            source_route_ledger_now_ms(),
        );
        let failed = fixture.engine.run_next(&fixture.data_root).unwrap();
        assert!(!failed.failed, "partial publication: {:#}", failed.job);
        assert_eq!(failed.job["structured_outcome"]["retryable"], false);
        assert_eq!(
            failed.job["structured_outcome"]["code"],
            "completed_with_source_failures"
        );
        assert_eq!(
            failed.job["structured_outcome"]["blocked_routes"],
            serde_json::json!([fixture.route.as_str()])
        );
        assert!(!fixture.engine.has_scheduled_route_work());
        assert!(!fixture.engine.has_pending_request());
        assert!(!fixture.watch.provider_root_refresh_pending_for_test());
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
        fixture.assert_predecessor_retained()?;
        Ok(fixture)
    }

    fn reconcile(&mut self, trigger: WatchCatalogReconcileTrigger, replacement: bool) {
        let catalog = if replacement {
            self.replacement.clone()
        } else {
            self.original.clone()
        };
        self.watch.reconcile_catalog_and_route_authority_with(
            &self.data_root,
            Some(&self.engine),
            trigger,
            false,
            |_| Ok(catalog.clone()),
            DaemonFileWatcher::start,
        );
    }

    fn assert_predecessor_retained(&self) -> Result<()> {
        let pin = VerifiedIndex::open_pinned(source_backed_index_root(&self.data_root))?;
        assert_eq!(pin.generation_id(), self.generation);
        assert_eq!(pin.manifest().indexed_documents, 1);
        assert_eq!(
            pin.manifest().provider_root_config_digest(),
            self.original.provider_root_config_digest().unwrap()
        );
        Ok(())
    }
}

fn catalog(path: &Path, root: ProviderRootDefinition) -> Result<SourceBackedWatchCatalog> {
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(SourceBackedRoute::automatic(
        ProviderSource {
            provider: CaptureProvider::OpenHands,
            path: path.to_path_buf(),
            exists: true,
            source_format: "openhands_file_events",
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
            route_provenance: Default::default(),
        },
        SourceBackedSelectorAuthority::DiscoveredWinner,
        SourceBackedRouteDriver::new(|_| Ok(()), |_| false, |_| true),
    )?);
    let routes = registry.watch_catalog().route_ids().cloned().collect();
    registry.set_applied_provider_roots(
        true,
        provider_source_config_digest(true, std::slice::from_ref(&root)),
        vec![AppliedProviderRoot::new(root, routes)?],
    )?;
    Ok(registry.watch_catalog())
}

fn publish(
    index_root: &Path,
    route: &SourceRouteIdentity,
    source: &SourceKey,
    source_file: &Path,
    root: ProviderRootDefinition,
) -> Result<String> {
    let bytes = fs::read(source_file)?;
    let mut writer = GenerationWriter::open(index_root, WriterOptions::default())?
        .into_writer()
        .map_err(crate::committed_generation_recovery_error)?;
    writer.set_applied_provider_roots(
        true,
        provider_source_config_digest(true, std::slice::from_ref(&root)),
        vec![AppliedProviderRoot::new(root, vec![route.clone()])?],
    )?;
    writer.begin_source(source.clone())?;
    writer.add_core_record(observation_fixture_record(
        source,
        String::from_utf8(bytes.clone())?,
    ))?;
    writer.certify_source(observation_fixture_certificate(source, &bytes))?;
    writer.set_present_source_routes(vec![SourceRouteSnapshot::present(
        route.clone(),
        vec![source.clone()],
    )?])?;
    Ok(commit_source_backed_generation_for_test(writer)?
        .receipt()
        .generation_id
        .clone())
}

#[test]
fn pending_root_replacement_does_not_repeat_unchanged_nonretryable_work() -> Result<()> {
    let mut fixture = ReplacementFixture::failed()?;
    fixture.engine.record_watch_routes(
        [(fixture.route.clone(), EventWatermark::new(u64::MAX, 1))],
        0,
    );
    assert!(!fixture.engine.has_scheduled_route_work());
    for trigger in [
        WatchCatalogReconcileTrigger::Filesystem,
        WatchCatalogReconcileTrigger::SafetyTimeout,
        WatchCatalogReconcileTrigger::Filesystem,
    ] {
        fixture.reconcile(trigger, true);
        fixture.watch.enqueue_pending_provider_root_refresh(
            &fixture.data_root,
            Some(&fixture.engine),
            u64::MAX,
        );
        let repeated = fixture.engine.run_next(&fixture.data_root);
        assert!(
            repeated.is_none(),
            "unchanged reconciliation retried the replacement: calls={}, job={:#?}",
            fixture.calls.load(Ordering::SeqCst),
            repeated.map(|run| run.job)
        );
        assert!(!fixture.engine.has_pending_request());
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
        fixture.assert_predecessor_retained()?;
    }
    Ok(())
}

#[test]
fn pending_root_replacement_source_repair_retries_and_publishes_requested_config() -> Result<()> {
    let mut fixture = ReplacementFixture::failed()?;
    fs::write(&fixture.source_file, b"repaired source")?;
    fixture.engine.record_watch_routes(
        [(fixture.route.clone(), EventWatermark::new(u64::MAX, 2))],
        0,
    );
    fixture.reconcile(WatchCatalogReconcileTrigger::Filesystem, true);
    let repaired = fixture.engine.run_next(&fixture.data_root).unwrap();
    assert!(!repaired.failed, "{:#}", repaired.job);
    assert_eq!(repaired.job["structured_outcome"]["code"], "completed");
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
    fixture.reconcile(WatchCatalogReconcileTrigger::Filesystem, true);
    assert!(!fixture.watch.provider_root_refresh_pending_for_test());
    assert!(fixture.engine.run_next(&fixture.data_root).is_none());
    let pin = VerifiedIndex::open_pinned(source_backed_index_root(&fixture.data_root))?;
    assert_eq!(
        pin.manifest().provider_root_config_digest(),
        fixture.replacement.provider_root_config_digest().unwrap()
    );
    Ok(())
}

#[test]
fn pending_root_replacement_config_repair_clears_demand_without_retry() -> Result<()> {
    let mut fixture = ReplacementFixture::failed()?;
    fixture.reconcile(
        WatchCatalogReconcileTrigger::CatalogControl(EventWatermark::new(u64::MAX, 2)),
        false,
    );
    assert!(!fixture.watch.provider_root_refresh_pending_for_test());
    assert!(fixture.engine.run_next(&fixture.data_root).is_none());
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    fixture.assert_predecessor_retained()?;
    Ok(())
}

#[test]
fn pending_root_replacement_source_repair_waits_until_route_is_due() -> Result<()> {
    let mut fixture = ReplacementFixture::failed()?;
    fs::write(&fixture.source_file, b"repaired source")?;
    fixture.engine.record_watch_routes(
        [(fixture.route.clone(), EventWatermark::new(u64::MAX, 2))],
        u64::MAX - 1_000,
    );
    fixture.reconcile(WatchCatalogReconcileTrigger::Filesystem, true);
    assert!(fixture.engine.run_next(&fixture.data_root).is_none());
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    let due_at = fixture.engine.next_dirty_route_due_in_ms(0).unwrap();
    fixture.watch.enqueue_pending_provider_root_refresh(
        &fixture.data_root,
        Some(&fixture.engine),
        due_at,
    );
    let repaired = fixture.engine.run_next(&fixture.data_root).unwrap();
    assert!(!repaired.failed, "{:#}", repaired.job);
    assert_eq!(repaired.job["structured_outcome"]["code"], "completed");
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
    Ok(())
}
