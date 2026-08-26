//! Physical settings-upgrade coverage owned by refresh execution.

use super::*;

#[test]
fn automatic_execution_replaces_an_incompatible_settings_generation() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, discovery) = discovery_fixture(temp.path());
    let empty_sessions = temp.path().join("empty-sessions");
    std::fs::create_dir_all(&empty_sessions).unwrap();
    let report = DiscoveryReport {
        sources: vec![
            provider_source_for_path(CaptureProvider::Codex, empty_sessions),
            ProviderSource {
                provider: CaptureProvider::Warp,
                path: temp.path().join("missing-unsupported.sqlite"),
                exists: false,
                source_format: "warp_sqlite",
                source_kind: ProviderSourceKind::DetectionOnly,
                import_support: ProviderImportSupport::Unsupported,
                catalog_support: ProviderCatalogSupport::None,
                status: ProviderSourceStatus::Unsupported,
                unsupported_reason: Some("fixture has no executable source-backed route"),
                route_provenance: Default::default(),
            },
        ],
        issues: vec![
            DiscoveryIssue {
                provider: CaptureProvider::Warp,
                path: None,
                kind: DiscoveryIssueKind::NoDiskHistory,
                reason: "fixture has no provider history",
            },
            DiscoveryIssue {
                provider: CaptureProvider::Warp,
                path: None,
                kind: DiscoveryIssueKind::InsufficientOfficialEvidence,
                reason: "fixture has no officially supported source",
            },
        ],
    };
    let baseline = run_report(&discovery, report.clone(), &data_root, &index_root).unwrap();
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

    let rebuilt = run_report(&discovery, report, &data_root, &index_root).unwrap();

    assert_eq!(rebuilt.generation_id, baseline.generation_id);
    assert_ne!(std::fs::read(&pointer_path).unwrap(), pointer_before);
    assert!(!old_generation_path.exists());
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        baseline.generation_id
    );
}
