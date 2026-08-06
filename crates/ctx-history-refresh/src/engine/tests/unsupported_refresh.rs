//! Zero-source publication-authority coverage owned by the refresh engine.

use super::*;

fn discovery_fixture(root: &Path) -> (PathBuf, PathBuf, DiscoveryContext) {
    let home = root.join("home");
    let cwd = root.join("cwd");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    (home, cwd, discovery)
}

fn unsupported_warp(root: &Path) -> ProviderSource {
    ProviderSource {
        provider: CaptureProvider::Warp,
        path: root.join("missing-unsupported.sqlite"),
        exists: false,
        source_format: "warp_sqlite",
        source_kind: ProviderSourceKind::DetectionOnly,
        import_support: ProviderImportSupport::Unsupported,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Unsupported,
        unsupported_reason: Some("fixture has no executable source-backed route"),
    }
}

fn run_report(
    discovery: &DiscoveryContext,
    report: DiscoveryReport,
    data_root: &Path,
    index_root: &Path,
) -> Result<SourceBackedRefreshPublication> {
    let mut progress =
        |_: CaptureSourceBackedDetailedRefreshProgress| Ok::<(), SourceBackedRouteError>(());
    refresh_all_provider_sources(
        discovery,
        report,
        StdDuration::ZERO,
        data_root,
        index_root,
        None,
        SourceBackedRefreshScope::All,
        &BTreeSet::new(),
        &mut progress,
    )
}

fn replace_metadata_version(index_root: &Path, version: u64) -> VerifiedIndex {
    let current = VerifiedIndex::open(index_root).unwrap();
    let generation_id = current.generation_id().to_owned();
    let mut metadata: Value =
        serde_json::from_slice(current.publication_metadata().unwrap()).unwrap();
    metadata["version"] = json!(version);
    metadata
        .get_mut("receipt")
        .and_then(Value::as_object_mut)
        .unwrap()
        .remove("zero_source_authority");
    drop(current);
    let writer = ctx_history_index::GenerationWriter::open(index_root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer
        .republish_current_publication_metadata(
            &generation_id,
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap()
}

#[test]
fn unsupported_only_refresh_fails_closed_cold_and_retains_warm_generation() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, discovery) = discovery_fixture(temp.path());
    let report = DiscoveryReport {
        sources: vec![unsupported_warp(temp.path())],
        issues: Vec::new(),
    };

    let cold_error = run_report(&discovery, report.clone(), &data_root, &index_root).unwrap_err();
    assert!(format!("{cold_error:#}").contains(TERMINAL_COVERAGE_ERROR_CODE));
    assert!(matches!(
        VerifiedIndex::open(&index_root),
        Err(IndexError::MissingActiveGenerationPointer)
    ));

    let retained_generation = publish_pin_source(&index_root, publication_pin_source());
    let warm_error = run_report(&discovery, report, &data_root, &index_root).unwrap_err();
    assert!(format!("{warm_error:#}").contains(TERMINAL_COVERAGE_ERROR_CODE));
    let retained = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(retained.generation_id(), retained_generation);
    assert_eq!(retained.manifest().sources.len(), 1);
}

#[test]
fn executable_empty_inventory_publishes_v2_authority_and_survives_restart() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, discovery) = discovery_fixture(temp.path());
    let source_root = temp.path().join("empty-codex-sessions");
    fs::create_dir_all(&source_root).unwrap();
    let report = DiscoveryReport {
        sources: vec![provider_source_for_path(
            CaptureProvider::Codex,
            source_root,
        )],
        issues: Vec::new(),
    };

    let publication = run_report(&discovery, report.clone(), &data_root, &index_root).unwrap();
    assert_eq!(publication.certified_source_count, 0);
    let [authority] = publication.zero_source_authority.as_slice() else {
        panic!("one executable empty route authority expected");
    };
    assert_eq!(
        authority.kind,
        SourceBackedZeroSourceAuthorityKind::CompleteEmptyInventory
    );
    assert_eq!(authority.generation_id, publication.generation_id);

    let restarted = VerifiedIndex::open(&index_root).unwrap();
    assert!(verified_generation_is_query_ready(&restarted).unwrap());
    let metadata = SourceBackedPublicationMetadata::decode(&restarted).unwrap();
    assert_eq!(
        metadata.version,
        SOURCE_REFRESH_PUBLICATION_METADATA_VERSION
    );
    assert!(metadata.certifies_generation(&restarted));
    drop(restarted);

    let legacy = replace_metadata_version(&index_root, 1);
    assert!(!verified_generation_is_query_ready(&legacy).unwrap());
    assert_eq!(
        SourceBackedPublicationMetadata::decode(&legacy)
            .unwrap()
            .version,
        1
    );
    assert!(pin_retained_generation(&data_root, &publication.generation_id).is_err());
    drop(legacy);
    let recertified = run_report(&discovery, report, &data_root, &index_root).unwrap();
    assert_eq!(recertified.generation_id, publication.generation_id);
    let recertified = VerifiedIndex::open(&index_root).unwrap();
    assert!(verified_generation_is_query_ready(&recertified).unwrap());
    assert!(pin_retained_generation(&data_root, &publication.generation_id).is_ok());
    assert_eq!(
        SourceBackedPublicationMetadata::decode(&recertified)
            .unwrap()
            .version,
        SOURCE_REFRESH_PUBLICATION_METADATA_VERSION
    );
    drop(recertified);

    let unknown = replace_metadata_version(&index_root, 99);
    assert!(SourceBackedPublicationMetadata::decode(&unknown).is_err());
    assert!(verified_generation_is_query_ready(&unknown).is_err());
}

#[test]
fn confirmed_deletion_can_publish_empty_but_mixed_unavailable_cannot() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, discovery) = discovery_fixture(temp.path());
    let source_root = temp.path().join("codex-sessions");
    let session = "019fb600-0000-7000-8000-000000000011";
    super::codex_union_tests::write_codex_rollout(&source_root, session, "deletionmarker");
    let source = provider_source_for_path(CaptureProvider::Codex, source_root.clone());
    let report = DiscoveryReport {
        sources: vec![source.clone()],
        issues: Vec::new(),
    };
    let first = run_report(&discovery, report, &data_root, &index_root).unwrap();
    assert_eq!(first.certified_source_count, 1);
    let legacy_nonempty = replace_metadata_version(&index_root, 1);
    assert!(verified_generation_is_query_ready(&legacy_nonempty).unwrap());
    drop(legacy_nonempty);

    fs::remove_file(source_root.join(format!("rollout-{session}.jsonl"))).unwrap();
    let deletion = run_report(
        &discovery,
        DiscoveryReport {
            sources: vec![source],
            issues: Vec::new(),
        },
        &data_root,
        &index_root,
    )
    .unwrap();
    assert_eq!(deletion.certified_source_count, 0);
    assert!(deletion.zero_source_authority.iter().all(|authority| {
        authority.kind == SourceBackedZeroSourceAuthorityKind::ConfirmedDeletion
    }));
    let deletion_generation = deletion.generation_id.clone();

    let mixed_error = run_report(
        &discovery,
        DiscoveryReport {
            sources: vec![
                provider_source_for_path(CaptureProvider::Codex, source_root),
                unsupported_warp(temp.path()),
            ],
            issues: Vec::new(),
        },
        &data_root,
        &index_root,
    )
    .unwrap_err();
    assert!(format!("{mixed_error:#}").contains(TERMINAL_COVERAGE_ERROR_CODE));
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        deletion_generation
    );
}
