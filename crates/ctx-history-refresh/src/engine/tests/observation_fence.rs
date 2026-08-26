//! Observation-fence coverage owned by the refresh engine.

use super::*;
use std::{fs::OpenOptions, io::Write};

#[derive(Debug, Clone)]
struct ObservationFenceRuntime {
    home: PathBuf,
    cwd: PathBuf,
}

impl RefreshRuntime for ObservationFenceRuntime {
    fn metadata(&self, _data_root: &Path, operation: RefreshOperation) -> RefreshRuntimeMetadata {
        RefreshRuntimeMetadata {
            operation,
            ..RefreshRuntimeMetadata::default()
        }
    }

    fn discovery_context(&self, _data_root: &Path) -> Result<DiscoveryContext> {
        Ok(DiscoveryContext::new(
            &self.home,
            &self.cwd,
            DiscoveryPlatform::Linux,
            DiscoveryPlatformDirs::default(),
        ))
    }
}

#[test]
fn post_scan_jsonl_append_is_not_certified_clean_across_restart() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let source_path = home.join(".codex/history.jsonl");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    writeln!(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&source_path)
            .unwrap(),
        "{}",
        json!({"session_id": "fence-session", "ts": 1, "text": "initialmarker"})
    )
    .unwrap();
    let source = provider_source_for_path(CaptureProvider::Codex, source_path.clone());
    assert_eq!(source.source_format, "codex_history_jsonl");
    let report = DiscoveryReport {
        sources: vec![source],
        issues: Vec::new(),
    };
    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let make_executor = || {
        let executor_discovery = discovery.clone();
        let executor_report = report.clone();
        Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
            let mut progress = |_: CaptureSourceBackedDetailedRefreshProgress| {
                Ok::<(), SourceBackedRouteError>(())
            };
            ctx_history_refresh_execution::refresh_all_provider_sources_route_local(
                &executor_discovery,
                executor_report.clone(),
                StdDuration::ZERO,
                execution.request_id,
                execution.operation,
                execution.data_root,
                execution.index_root,
                execution.explicit_source_catalog,
                execution.admitted_refresh().publication_scope(),
                execution.published_state,
                &mut progress,
            )
        }) as Arc<dyn SourceBackedRefreshExecutor>
    };
    let catalog =
        build_automatic_source_backed_registry_from_report(&discovery, &data_root, report.clone())
            .registry
            .watch_catalog();
    let route = catalog.route_ids().next().unwrap().clone();
    let missing_route = route_identity(0xd1);
    let requested_routes = BTreeSet::from([route.clone(), missing_route.clone()]);
    let requested_observations =
        source_backed_requested_route_observations(&catalog, &requested_routes);
    assert_eq!(
        requested_observations.keys().collect::<BTreeSet<_>>(),
        requested_routes.iter().collect()
    );
    assert!(requested_observations[&route].is_some());
    assert_eq!(requested_observations[&missing_route], None);
    let journal = Arc::new(TestRefreshJournal::default());
    let runtime = Arc::new(ObservationFenceRuntime {
        home: home.clone(),
        cwd: cwd.clone(),
    });
    let coordinator = CoreRefreshEngine(super::super::CoreRefreshEngine::with_journal_for_test(
        Arc::clone(&journal) as Arc<dyn RefreshJournal>,
        Arc::clone(&runtime) as Arc<dyn RefreshRuntime>,
        make_executor(),
    ));
    let request = coordinator.enqueue_periodic(&data_root).unwrap();
    let request_id = request["request_id"].as_str().unwrap().to_owned();
    let append_path = source_path.clone();
    install_after_capture_scan_before_metadata_hook_for_test(move || {
        writeln!(
            OpenOptions::new().append(true).open(append_path).unwrap(),
            "{}",
            json!({
                "session_id": "fence-session",
                "ts": 2,
                "text": "postscanappendmarker",
            })
        )
        .unwrap();
    });
    let initial = coordinator.run_next(&data_root).expect("initial refresh");
    assert!(!initial.failed, "{:#}", initial.job);
    assert_eq!(initial.job["request_id"], request_id);
    assert!(
        complete_lexical_candidates(
            &open_verified_index(&index_root).unwrap(),
            "postscanappendmarker",
            10,
        )
        .unwrap()
        .is_empty(),
        "the post-scan append must not be present in the first generation"
    );
    drop(coordinator);

    // No watcher event is delivered. Restart must compare the exact target
    // against the pre-scan durable token and schedule the omitted append.
    let restarted = CoreRefreshEngine(super::super::CoreRefreshEngine::with_journal_for_test(
        journal as Arc<dyn RefreshJournal>,
        runtime as Arc<dyn RefreshRuntime>,
        make_executor(),
    ));
    assert!(!restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    restarted.initialize_watch_route_authority([route.clone()]);
    restarted.schedule_startup_route_observation(
        &catalog,
        EventWatermark::new(2, 0),
        ledger_now_ms().saturating_sub(1_000),
    );
    assert_eq!(
        restarted.scheduled_route_ids_for_test(),
        BTreeSet::from([route])
    );
    assert!(restarted
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());
    let follow_up = restarted.run_next(&data_root).expect("restart refresh");
    assert!(!follow_up.failed, "{:#}", follow_up.job);
    assert_eq!(
        complete_lexical_candidates(
            &open_verified_index(&index_root).unwrap(),
            "postscanappendmarker",
            10,
        )
        .unwrap()
        .len(),
        1
    );
}
