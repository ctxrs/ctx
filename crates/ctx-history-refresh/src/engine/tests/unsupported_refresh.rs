//! Zero-source publication-authority coverage owned by the refresh engine.

use super::*;

pub(super) fn discovery_fixture(root: &Path) -> (PathBuf, PathBuf, DiscoveryContext) {
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
    let path = root.join("present-unsupported.sqlite");
    fs::write(&path, b"unsupported fixture").unwrap();
    ProviderSource {
        provider: CaptureProvider::Warp,
        path,
        exists: true,
        source_format: "warp_sqlite",
        source_kind: ProviderSourceKind::DetectionOnly,
        import_support: ProviderImportSupport::Unsupported,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Unsupported,
        unsupported_reason: Some("fixture has no executable source-backed route"),
        route_provenance: Default::default(),
    }
}

pub(super) fn run_report(
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
        &mut progress,
    )
}

#[test]
fn present_unsupported_only_refresh_fails_cold_and_reports_against_a_warm_generation() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, discovery) = discovery_fixture(temp.path());
    let report = DiscoveryReport {
        sources: vec![unsupported_warp(temp.path())],
        issues: Vec::new(),
    };

    let cold_error = run_report(&discovery, report.clone(), &data_root, &index_root).unwrap_err();
    let cold_detail = format!("{cold_error:#}");
    assert!(
        cold_detail.contains("retained no usable source")
            && cold_detail.contains("unsupported provider schema"),
        "{cold_detail}"
    );
    assert!(matches!(
        VerifiedIndex::open_pinned(&index_root),
        Err(IndexError::MissingActiveGenerationPointer)
    ));

    let retained_generation = publish_pin_source(&index_root, publication_pin_source());
    let retained_nonempty = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert!(SourceBackedGenerationState::decode_from_verified_index(&retained_nonempty).is_ok());
    drop(retained_nonempty);
    let warm = run_report(&discovery, report, &data_root, &index_root).unwrap();
    assert_eq!(warm.generation_id, retained_generation);
    let [failed_route] = warm.route_results.as_slice() else {
        panic!("one failed unsupported route expected: {warm:#?}");
    };
    assert_eq!(failed_route.outcome.failure_class(), Some("incompatible"));
    assert!(!failed_route.outcome.is_success());
    let retained = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert_eq!(retained.generation_id(), retained_generation);
    assert_eq!(retained.manifest().sources.len(), 1);
}

#[test]
fn executable_empty_inventory_publishes_typed_state_and_identical_state_reuses() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
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

    let restarted = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert!(SourceBackedGenerationState::decode_from_verified_index(&restarted).is_ok());
    drop(restarted);

    assert!(pin_retained_generation(&data_root, &publication.generation_id).is_ok());
    let reused = run_report(&discovery, report, &data_root, &index_root).unwrap();
    assert_eq!(reused.generation_id, publication.generation_id);
    let reused = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert!(SourceBackedGenerationState::decode_from_verified_index(&reused).is_ok());
    assert!(pin_retained_generation(&data_root, &publication.generation_id).is_ok());
}

#[test]
fn genuinely_empty_catalog_publishes_a_verified_noop_generation() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, discovery) = discovery_fixture(temp.path());

    let publication = run_report(
        &discovery,
        DiscoveryReport {
            sources: Vec::new(),
            issues: Vec::new(),
        },
        &data_root,
        &index_root,
    )
    .unwrap();
    assert_eq!(publication.certified_source_count, 0);
    assert!(publication.route_results.is_empty());
    assert!(publication.zero_source_authority.is_empty());

    let restarted = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert!(
        SourceBackedGenerationState::decode_from_verified_index(&restarted)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn confirmed_deletion_can_publish_empty_but_mixed_unavailable_cannot() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
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
    assert!(format!("{mixed_error:#}")
        .contains(RefreshOutcomeCode::AllProviderTerminalCoverageUnavailable.as_str()));
    assert_eq!(
        VerifiedIndex::open_pinned(&index_root)
            .unwrap()
            .generation_id(),
        deletion_generation
    );
}
