//! Registry-policy coverage owned by the refresh engine.

use super::*;
use sha2::{Digest, Sha256};

fn registry_policy_warp_source(path: PathBuf, exists: bool) -> ProviderSource {
    ProviderSource {
        provider: CaptureProvider::Warp,
        path,
        exists,
        source_format: "warp_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::Native,
        status: if exists {
            ProviderSourceStatus::Available
        } else {
            ProviderSourceStatus::Missing
        },
        unsupported_reason: None,
    }
}

fn registry_policy_nanoclaw_source(path: PathBuf) -> ProviderSource {
    ProviderSource {
        provider: CaptureProvider::NanoClaw,
        path,
        exists: true,
        source_format: "nanoclaw_project",
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::Native,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    }
}

fn registry_policy_automatic_route_identity(source: &ProviderSource) -> SourceRouteIdentity {
    ctx_history_capture::SourceBackedRoute::automatic(
        source.clone(),
        SourceBackedSelectorAuthority::CatalogLineage,
        ctx_history_capture::SourceBackedRouteDriver::new(|_| Ok(()), |_| false, |_| true),
    )
    .unwrap()
    .metadata()
    .route_identity
    .clone()
    .unwrap()
}

#[test]
fn only_unscopable_registry_safety_issues_block_globally() {
    let missing_source =
        registry_policy_warp_source(PathBuf::from("/unavailable/warp.sqlite"), false);
    let missing = SourceBackedAutomaticRegistryIssue::Unavailable {
        source: missing_source,
        reason: SourceBackedAutomaticUnavailableReason::SourceStatus(ProviderSourceStatus::Missing),
    };
    assert!(reject_blocking_automatic_registry_issues(&[missing]).is_ok());

    let selector_source = registry_policy_warp_source(PathBuf::from("/detected/warp.sqlite"), true);
    let selector_gap = SourceBackedAutomaticRegistryIssue::Unavailable {
        source: selector_source.clone(),
        reason: SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
            detail: "injected selector gap",
        },
    };
    assert!(reject_blocking_automatic_registry_issues(std::slice::from_ref(&selector_gap)).is_ok());
    let failures = automatic_registry_route_failures(&[selector_gap], None).unwrap();
    assert_eq!(failures.len(), 1);
    assert_eq!(
        failures[0].class,
        SourceBackedSourceFailureClass::Incompatible
    );
    assert!(!failures[0].carried_forward);

    let unsafe_overlap = SourceBackedAutomaticRegistryIssue::Unavailable {
        source: selector_source,
        reason: SourceBackedAutomaticUnavailableReason::UnsafeRootOverlap {
            detail: "injected unsafe root overlap".to_owned(),
        },
    };
    let error = reject_blocking_automatic_registry_issues(&[unsafe_overlap]).unwrap_err();
    assert!(format!("{error:#}").contains("injected unsafe root overlap"));
}

#[test]
fn registry_failure_identity_uses_the_canonical_certified_format() {
    let path = PathBuf::from("/detected/codex-sessions");
    let source = ProviderSource {
        provider: CaptureProvider::Codex,
        path: path.clone(),
        exists: true,
        source_format: "codex_session_jsonl_tree",
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::Native,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    };
    let issue = SourceBackedAutomaticRegistryIssue::Unavailable {
        source,
        reason: SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
            detail: "injected selector gap",
        },
    };
    let failures = automatic_registry_route_failures(&[issue], None).unwrap();
    let mut digest = Sha256::new();
    digest.update(b"ctx.source-failure-identity-v1\0");
    digest.update(b"codex\0codex_session_jsonl\0");
    let encoded_path = path.as_os_str().as_encoded_bytes();
    digest.update((encoded_path.len() as u64).to_be_bytes());
    digest.update(encoded_path);

    assert_eq!(failures.len(), 1);
    assert_eq!(
        failures[0].source_identity,
        format!("{:x}", digest.finalize())
    );
}

#[test]
fn distinct_nanoclaw_registry_failures_match_retained_automatic_routes() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let first_checkout = temp.path().join("nanoclaw-first");
    let second_checkout = temp.path().join("nanoclaw-second");
    std::fs::create_dir_all(&first_checkout).unwrap();
    std::fs::create_dir_all(&second_checkout).unwrap();
    let sources = [
        registry_policy_nanoclaw_source(first_checkout),
        registry_policy_nanoclaw_source(second_checkout),
    ];
    let expected_route_ids = sources
        .iter()
        .map(registry_policy_automatic_route_identity)
        .collect::<BTreeSet<_>>();
    assert_eq!(expected_route_ids.len(), 2);

    let mut writer =
        ctx_history_index::GenerationWriter::open(&index_root, WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
    let mut retained_routes = Vec::new();
    for (route_identity, anchor) in expected_route_ids.iter().zip([0xa1, 0xa2]) {
        let retained_source = publication_pin_source_with_anchor(anchor);
        writer.begin_source(retained_source.clone()).unwrap();
        writer
            .add_core_record(publication_pin_record(&retained_source))
            .unwrap();
        writer
            .certify_source(publication_pin_certificate(&retained_source))
            .unwrap();
        retained_routes.push(
            ctx_history_index::SourceRouteSnapshot::present(
                route_identity.clone(),
                vec![retained_source],
            )
            .unwrap(),
        );
    }
    writer.set_present_source_routes(retained_routes).unwrap();
    writer.commit(|_| true).unwrap();
    let retained = VerifiedIndex::open(&index_root).unwrap();

    let issues = sources.map(|source| SourceBackedAutomaticRegistryIssue::Unavailable {
        source,
        reason: SourceBackedAutomaticUnavailableReason::RegistrationRejected {
            detail: "injected NanoClaw registration failure".to_owned(),
        },
    });
    let failures = automatic_registry_route_failures(&issues, Some(&retained)).unwrap();
    let failed_route_ids = failures
        .iter()
        .map(|failure| failure.route_identity.clone())
        .collect::<BTreeSet<_>>();

    assert_eq!(failures.len(), 2);
    assert_eq!(failed_route_ids, expected_route_ids);
    assert!(failures.iter().all(|failure| failure.carried_forward));
}

#[test]
fn mixed_valid_and_invalid_registry_routes_publish_with_a_typed_failure() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let codex_root = temp.path().join("codex-sessions");
    let invalid_warp = temp.path().join("unselected-warp.sqlite");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    write_registry_policy_codex_rollout(&codex_root);
    std::fs::write(&invalid_warp, b"not selected by Warp discovery").unwrap();
    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let report = DiscoveryReport {
        sources: vec![
            provider_source_for_path(CaptureProvider::Codex, codex_root),
            registry_policy_warp_source(invalid_warp, true),
        ],
        issues: Vec::new(),
    };
    let mut progress =
        |_: CaptureSourceBackedDetailedRefreshProgress| Ok::<(), SourceBackedRouteError>(());

    let publication = refresh_all_provider_sources(
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

    assert_eq!(publication.route_results.len(), 2);
    assert_eq!(publication.unsupported_routes, 1);
    assert_eq!(publication.certified_source_count, 1);
    assert_eq!(
        publication
            .route_results
            .iter()
            .filter(|result| result.outcome.is_success())
            .count(),
        1
    );
    let failures = publication
        .route_results
        .iter()
        .flat_map(|result| result.source_failures.iter())
        .collect::<Vec<_>>();
    assert_eq!(failures.len(), 1);
    let failure = failures[0];
    assert_eq!(failure.provider, "warp");
    assert_eq!(failure.class, "incompatible");
    assert!(!failure.carried_forward);
    assert!(is_sha256_identity(&failure.route_identity));
    assert!(is_sha256_identity(&failure.source_identity));
    let verified = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(verified.manifest().sources.len(), 1);
    assert_eq!(
        verified
            .search_event_candidates("registrypolicyvalidmarker", 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn registry_issue_only_cold_refresh_retains_the_all_fail_guard() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let invalid_warp = temp.path().join("unselected-warp.sqlite");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(&invalid_warp, b"not selected by Warp discovery").unwrap();
    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let report = DiscoveryReport {
        sources: vec![registry_policy_warp_source(invalid_warp, true)],
        issues: Vec::new(),
    };
    let mut progress =
        |_: CaptureSourceBackedDetailedRefreshProgress| Ok::<(), SourceBackedRouteError>(());

    let error = refresh_all_provider_sources(
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
    .unwrap_err();
    let failed_routes = error
        .chain()
        .find_map(|cause| {
            let SourceBackedCoordinatorError::NoUsableSourceRoutes { failed_routes } =
                cause.downcast_ref::<SourceBackedCoordinatorError>()?
            else {
                return None;
            };
            Some(failed_routes)
        })
        .expect("typed all-fail-cold error");
    assert_eq!(failed_routes.len(), 1);
    assert_eq!(
        failed_routes[0].class,
        SourceBackedSourceFailureClass::Incompatible
    );
    assert!(!index_root.exists());
}

fn write_registry_policy_codex_rollout(root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    let session_meta = json!({
        "timestamp": "2026-08-02T12:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": "019fb700-0000-7000-8000-000000000001",
            "timestamp": "2026-08-02T12:00:00Z",
            "cwd": "/repo/registry-policy",
            "originator": "codex_cli_rs",
            "cli_version": "1.0.0",
            "source": "cli",
            "model_provider": "openai"
        }
    });
    let message = json!({
        "timestamp": "2026-08-02T12:00:01Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": "registrypolicyvalidmarker"
            }]
        }
    });
    std::fs::write(
        root.join("rollout-019fb700-0000-7000-8000-000000000001.jsonl"),
        format!("{session_meta}\n{message}\n"),
    )
    .unwrap();
}
