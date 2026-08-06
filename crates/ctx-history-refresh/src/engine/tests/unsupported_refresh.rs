//! Unsupported-refresh coverage owned by the refresh engine.

use super::*;

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
    assert!(first.route_results.is_empty());
    assert_eq!(first.unsupported_routes, 1);
    assert_eq!(first.certified_source_count, 0);
    assert_eq!(first.published_explicit_source_catalog, None);
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
    assert!(replay.route_results.is_empty());
    assert_eq!(replay.unsupported_routes, 1);
    assert_eq!(replay.published_explicit_source_catalog, None);

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
    assert_eq!(run.job["source_count"], 0);
    assert_eq!(run.job["scanned_routes"], 0);
    assert_eq!(run.job["unsupported_routes"], 1);
    assert!(run.job.get("published_explicit_source_catalog").is_none());
    assert!(run.job["receipt"]
        .get("published_explicit_source_catalog")
        .is_none());
    assert_eq!(
        coordinator.status(&request_id).unwrap()["request_state"],
        "published"
    );
}
