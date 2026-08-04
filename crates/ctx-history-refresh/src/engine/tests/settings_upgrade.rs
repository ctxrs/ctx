//! Settings-upgrade coverage owned by the refresh engine.

use super::*;

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
