//! Registry-policy coverage owned by the refresh execution crate.

use super::execution_path::CompleteLexicalSearch;
use super::*;
use super::{discovery_fixture, run_report};
use ctx_history_capture::legacy_automatic_source_backed_route_identity;
use rusqlite::Connection;
use sha2::{Digest, Sha256};

fn registry_policy_source(
    provider: CaptureProvider,
    path: PathBuf,
    source_format: &'static str,
    exists: bool,
) -> ProviderSource {
    ProviderSource {
        provider,
        path,
        exists,
        source_format,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::Native,
        status: if exists {
            ProviderSourceStatus::Available
        } else {
            ProviderSourceStatus::Missing
        },
        unsupported_reason: None,
        route_provenance: Default::default(),
    }
}

fn registry_policy_warp_source(path: PathBuf, exists: bool) -> ProviderSource {
    registry_policy_source(CaptureProvider::Warp, path, "warp_sqlite", exists)
}

fn registry_policy_unsupported_warp_source(path: PathBuf) -> ProviderSource {
    let mut source = registry_policy_warp_source(path, true);
    source.status = ProviderSourceStatus::Unsupported;
    source.unsupported_reason = Some("fixture Warp route is intentionally unsupported");
    source
}

fn registry_policy_unsupported_codex_source(path: PathBuf) -> ProviderSource {
    let mut source = registry_policy_source(
        CaptureProvider::Codex,
        path,
        "codex_session_jsonl_tree",
        true,
    );
    source.status = ProviderSourceStatus::Unsupported;
    source.unsupported_reason = Some("fixture alternate Codex root is intentionally unsupported");
    source
}

fn registry_policy_nanoclaw_source(path: PathBuf) -> ProviderSource {
    registry_policy_source(CaptureProvider::NanoClaw, path, "nanoclaw_project", true)
}

fn automatic_antigravity_source(root: PathBuf, surface: &[u8]) -> ProviderSource {
    ProviderSource {
        provider: CaptureProvider::Antigravity,
        path: root,
        exists: true,
        source_format: "antigravity_cli_transcript_jsonl_tree",
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::Native,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
        route_provenance: ProviderSourceRouteProvenance::Automatic {
            route_role: ProviderRouteRole::from_dynamic([b"surface".as_slice(), surface]).unwrap(),
        },
    }
}

fn write_antigravity_transcript(root: &Path, session: &str, body: &str) {
    let logs = root.join(session).join(".system_generated/logs");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(
        logs.join("transcript.jsonl"),
        format!(
            "{{\"type\":\"user\",\"content\":{body:?},\"created_at\":\"2026-08-24T12:00:00Z\"}}\n"
        ),
    )
    .unwrap();
}

#[test]
fn production_refresh_persists_both_split_publications_without_provider_roots() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, discovery) = discovery_fixture(temp.path());
    assert!(discovery.configured_provider_roots().is_empty());

    let cli_root = temp.path().join("antigravity-cli/brain");
    let ide_root = temp.path().join("antigravity-ide/brain");
    write_antigravity_transcript(&cli_root, "cli-session", "cli migration needle");
    write_antigravity_transcript(&ide_root, "ide-session", "ide migration needle");
    let cli = automatic_antigravity_source(cli_root, b"cli");
    let ide = automatic_antigravity_source(ide_root, b"ide");
    let legacy = legacy_automatic_source_backed_route_identity(&cli).unwrap();
    let successors = BTreeSet::from([
        automatic_source_backed_route_identity(&cli).unwrap(),
        automatic_source_backed_route_identity(&ide).unwrap(),
    ]);

    let mut writer = GenerationWriter::open(&index_root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer
        .set_present_source_routes(vec![ctx_history_index::SourceRouteSnapshot::present(
            legacy.clone(),
            Vec::new(),
        )
        .unwrap()])
        .unwrap();
    writer.commit(|_| true).unwrap();

    let report = || DiscoveryReport {
        sources: vec![cli.clone(), ide.clone()],
        issues: Vec::new(),
    };
    let bridge = run_report(&discovery, report(), &data_root, &index_root).unwrap();
    let bridge_index = bridge.verified_index.as_ref().unwrap();
    assert!(bridge_index.manifest().source_route(&legacy).is_some());
    let bridge_metadata = SourceBackedPublicationMetadata::decode(bridge_index).unwrap();
    assert!(bridge_metadata.route_controls.contains_key(&legacy));

    let successor = run_report(&discovery, report(), &data_root, &index_root).unwrap();
    let successor_index = successor.verified_index.as_ref().unwrap();
    let final_routes = successor_index
        .manifest()
        .source_routes()
        .iter()
        .map(|route| route.route_identity().clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(final_routes, successors);
    assert!(!final_routes.contains(&legacy));
    let successor_metadata = SourceBackedPublicationMetadata::decode(successor_index).unwrap();
    assert!(!successor_metadata.route_controls.contains_key(&legacy));
}

fn write_hermes_profile(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    Connection::open(path)
        .unwrap()
        .execute_batch(
            "create table sessions (
                 id text primary key, source text not null, started_at real not null,
                 message_count integer default 0
             );
             create table messages (
                 id integer primary key, session_id text not null, role text not null,
                 content text, timestamp real not null, active integer not null default 1,
                 compacted integer not null default 0
             );
             insert into sessions (id, source, started_at, message_count)
                 values ('profile-session', 'acp', 1782259200.0, 1);
             insert into messages (id, session_id, role, content, timestamp)
                 values (1, 'profile-session', 'assistant', 'profile rename needle', 1782259201.0);",
        )
        .unwrap();
}

#[test]
fn warm_automatic_hermes_profile_rename_retires_the_old_route_and_remains_refreshable() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, discovery) = discovery_fixture(temp.path());
    let profiles = temp.path().join("profiles");
    let alpha = profiles.join("alpha/state.db");
    let beta = profiles.join("beta/state.db");
    write_hermes_profile(&alpha);
    let alpha_source = provider_source_for_path(CaptureProvider::Hermes, alpha.clone());
    let alpha_route = automatic_source_backed_route_identity(&alpha_source).unwrap();

    let cold = run_report(
        &discovery,
        DiscoveryReport {
            sources: vec![alpha_source],
            issues: Vec::new(),
        },
        &data_root,
        &index_root,
    )
    .unwrap();
    assert!(cold
        .verified_index
        .as_ref()
        .unwrap()
        .manifest()
        .source_route(&alpha_route)
        .is_some());

    std::fs::rename(alpha.parent().unwrap(), beta.parent().unwrap()).unwrap();
    let beta_source = provider_source_for_path(CaptureProvider::Hermes, beta);
    let beta_route = automatic_source_backed_route_identity(&beta_source).unwrap();
    let warm_report = || DiscoveryReport {
        sources: vec![beta_source.clone()],
        issues: Vec::new(),
    };
    let warm = run_report(&discovery, warm_report(), &data_root, &index_root).unwrap();
    let warm_index = warm.verified_index.as_ref().unwrap();
    assert!(warm_index.manifest().source_route(&alpha_route).is_none());
    assert!(warm_index.manifest().source_route(&beta_route).is_some());
    let warm_metadata = SourceBackedPublicationMetadata::decode(warm_index).unwrap();
    assert!(!warm_metadata.route_controls.contains_key(&alpha_route));
    assert!(warm_metadata.route_controls.contains_key(&beta_route));

    let subsequent = run_report(&discovery, warm_report(), &data_root, &index_root).unwrap();
    let subsequent = subsequent.verified_index.as_ref().unwrap();
    assert!(subsequent.manifest().source_route(&alpha_route).is_none());
    assert!(subsequent.manifest().source_route(&beta_route).is_some());
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

    let configured_conflict = SourceBackedAutomaticRegistryIssue::Discovery(DiscoveryIssue {
        provider: CaptureProvider::Claude,
        path: Some(PathBuf::from("/configured/claude")),
        kind: DiscoveryIssueKind::ConfiguredRootConflict,
        reason: "injected configured root conflict",
    });
    let error = reject_blocking_automatic_registry_issues(&[configured_conflict]).unwrap_err();
    assert!(format!("{error:#}").contains("injected configured root conflict"));
}

#[test]
fn systemic_registration_failures_keep_exact_admission_route_authority() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("shelley.db");
    std::fs::write(&database, b"fixture").unwrap();
    for kind in [
        SourceBackedRouteErrorKind::ResourceUnavailable,
        SourceBackedRouteErrorKind::Internal,
    ] {
        let source = provider_source_for_path(CaptureProvider::Shelley, database.clone());
        let expected_route = automatic_source_backed_route_identity(&source).unwrap();
        let issue = SourceBackedAutomaticRegistryIssue::Unavailable {
            source,
            reason: SourceBackedAutomaticUnavailableReason::RegistrationRejected {
                kind,
                detail: "injected Shelley registration systemic failure".to_owned(),
            },
        };

        assert!(
            automatic_registry_route_failures(std::slice::from_ref(&issue), None)
                .unwrap()
                .is_empty()
        );
        let failures = automatic_registry_admission_failures(
            std::slice::from_ref(&issue),
            AutomaticRegistryAdmissionFailurePolicy::SystemicOnly,
        )
        .unwrap()
        .expect("systemic admission failure");

        assert_eq!(failures.failures().len(), 1);
        assert_eq!(failures.failures()[0].route_identity(), &expected_route);
        assert_eq!(failures.failures()[0].kind(), kind);
        assert!(failures.failures()[0]
            .detail()
            .contains("registration systemic failure"));
        assert!(automatic_registry_admission_failures(
            std::slice::from_ref(&issue),
            AutomaticRegistryAdmissionFailurePolicy::ExactRoutes(&BTreeSet::from([
                expected_route.clone(),
            ])),
        )
        .unwrap()
        .is_some());
        let unrelated = SourceRouteIdentity::from_sha256("11".repeat(32)).unwrap();
        assert!(automatic_registry_admission_failures(
            std::slice::from_ref(&issue),
            AutomaticRegistryAdmissionFailurePolicy::ExactRoutes(&BTreeSet::from([unrelated])),
        )
        .unwrap()
        .is_none());
    }
}

#[test]
fn admission_registration_failures_enforce_the_durable_route_bound() {
    let failures = (0..=SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT).map(|index| {
        SourceBackedAdmissionRouteFailure::new(
            SourceRouteIdentity::from_sha256(format!("{index:064x}")).unwrap(),
            SourceBackedRouteErrorKind::ResourceUnavailable,
            "bounded resource failure",
        )
    });

    let error = SourceBackedAdmissionRouteFailures::try_from_failures(failures).unwrap_err();

    assert!(
        format!("{error:#}").contains("exceed the terminal route limit"),
        "{error:#}"
    );
}

#[test]
fn registry_failure_identity_uses_the_canonical_certified_format() {
    let path = PathBuf::from("/detected/codex-sessions");
    let source = registry_policy_source(
        CaptureProvider::Codex,
        path.clone(),
        "codex_session_jsonl_tree",
        true,
    );
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
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
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
            kind: SourceBackedRouteErrorKind::Unsupported,
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
fn mixed_codex_and_unsupported_warp_routes_continue_with_typed_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (home, _, discovery) = discovery_fixture(temp.path());
    let codex_root = temp.path().join("codex-sessions");
    let unsupported_warp = home.join("unsupported-warp.sqlite");
    write_registry_policy_codex_rollout(&codex_root);
    std::fs::write(&unsupported_warp, b"unsupported content must not be parsed").unwrap();
    let publication = run_report(
        &discovery,
        DiscoveryReport {
            sources: vec![
                provider_source_for_path(CaptureProvider::Codex, codex_root),
                registry_policy_unsupported_warp_source(unsupported_warp),
            ],
            issues: Vec::new(),
        },
        &data_root,
        &index_root,
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
    assert_eq!(
        failure.detail,
        "fixture Warp route is intentionally unsupported"
    );
    assert!(is_sha256_identity(&failure.route_identity));
    assert!(is_sha256_identity(&failure.source_identity));
    let verified = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(verified.manifest().sources.len(), 1);
    assert_eq!(
        verified
            .complete_lexical_search("registrypolicyvalidmarker", 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn successful_discovered_winner_owns_a_colliding_unusable_candidate_outcome() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, discovery) = discovery_fixture(temp.path());
    let codex_root = temp.path().join("codex-sessions");
    let unsupported_codex_root = temp.path().join("unsupported-codex-sessions");
    write_registry_policy_codex_rollout(&codex_root);
    std::fs::create_dir_all(&unsupported_codex_root).unwrap();
    let codex_source = provider_source_for_path(CaptureProvider::Codex, codex_root);
    let unsupported_codex_source = registry_policy_unsupported_codex_source(unsupported_codex_root);
    assert_eq!(
        automatic_source_backed_route_identity(&codex_source).unwrap(),
        automatic_source_backed_route_identity(&unsupported_codex_source).unwrap(),
    );

    let publication = run_report(
        &discovery,
        DiscoveryReport {
            sources: vec![codex_source, unsupported_codex_source],
            issues: Vec::new(),
        },
        &data_root,
        &index_root,
    )
    .unwrap();

    assert_eq!(publication.route_results.len(), 1);
    assert!(publication.route_results[0].outcome.is_success());
    assert!(publication.route_results[0].source_failures.is_empty());
    // The losing candidate remains counted as unsupported inventory without
    // becoming a second terminal outcome for the successful logical route.
    assert_eq!(publication.unsupported_routes, 1);
    assert_eq!(publication.certified_source_count, 1);
}

#[test]
fn all_route_execution_preserves_a_healthy_peer_during_source_local_registration_failure() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, cwd, discovery) = discovery_fixture(temp.path());
    let codex_root = temp.path().join("codex-sessions");
    let malformed_shelley = cwd.join("shelley.db");
    write_registry_policy_codex_rollout(&codex_root);
    std::fs::write(&malformed_shelley, b"not sqlite").unwrap();

    let publication = run_report(
        &discovery,
        DiscoveryReport {
            sources: vec![
                provider_source_for_path(CaptureProvider::Codex, codex_root),
                provider_source_for_path(CaptureProvider::Shelley, malformed_shelley),
            ],
            issues: Vec::new(),
        },
        &data_root,
        &index_root,
    )
    .unwrap();

    assert_eq!(publication.route_results.len(), 2);
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
    assert_eq!(failures[0].provider, "shelley");
    assert_eq!(failures[0].class, "unreadable");
    assert!(failures[0].detail.contains("not a database"));
    let verified = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(verified.manifest().sources.len(), 1);
    assert_eq!(
        verified
            .complete_lexical_search("registrypolicyvalidmarker", 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn unsupported_warp_preserves_same_epoch_last_good_route_as_stale() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (home, _, discovery) = discovery_fixture(temp.path());
    let database = home.join("unsupported-warp.sqlite");
    std::fs::create_dir_all(database.parent().unwrap()).unwrap();
    std::fs::write(&database, b"not a database and must not be opened").unwrap();
    let source = registry_policy_unsupported_warp_source(database);
    let route_identity = automatic_source_backed_route_identity(&source).unwrap();

    let retained_source = SourceKey::derive(
        CaptureProvider::Warp.as_str(),
        "warp_sqlite",
        "session",
        1,
        SourceAnchor::CatalogLineage([0x8d; 32]),
    )
    .unwrap();
    let mut writer =
        ctx_history_index::GenerationWriter::open(&index_root, WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
    writer.begin_source(retained_source.clone()).unwrap();
    writer
        .add_core_record(publication_pin_record(&retained_source))
        .unwrap();
    writer
        .certify_source(publication_pin_certificate(&retained_source))
        .unwrap();
    writer
        .set_present_source_routes(vec![ctx_history_index::SourceRouteSnapshot::present(
            route_identity.clone(),
            vec![retained_source],
        )
        .unwrap()])
        .unwrap();
    let retained_generation = writer.commit(|_| true).unwrap().generation_id;

    let publication = run_report(
        &discovery,
        DiscoveryReport {
            sources: vec![source],
            issues: Vec::new(),
        },
        &data_root,
        &index_root,
    )
    .unwrap();

    assert_eq!(publication.generation_id, retained_generation);
    let [result] = publication.route_results.as_slice() else {
        panic!("one unsupported Warp route result expected: {publication:#?}");
    };
    assert_eq!(result.route_identity, route_identity.as_str());
    assert_eq!(result.outcome.failure_class(), Some("incompatible"));
    let [failure] = result.source_failures.as_slice() else {
        panic!("one unsupported Warp source failure expected: {result:#?}");
    };
    assert!(failure.carried_forward);
    assert_eq!(
        failure.detail,
        "fixture Warp route is intentionally unsupported"
    );
    let retained = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(retained.generation_id(), retained_generation);
    assert!(retained.manifest().source_route(&route_identity).is_some());
    assert_eq!(retained.manifest().sources.len(), 1);
    assert_eq!(
        retained.manifest().sources[0]
            .observation()
            .source()
            .provider(),
        CaptureProvider::Warp.as_str()
    );
}

#[test]
fn registry_issue_only_cold_refresh_retains_the_all_fail_guard() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, discovery) = discovery_fixture(temp.path());
    let invalid_warp = temp.path().join("unselected-warp.sqlite");
    std::fs::write(&invalid_warp, b"not selected by Warp discovery").unwrap();
    let error = run_report(
        &discovery,
        DiscoveryReport {
            sources: vec![registry_policy_warp_source(invalid_warp, true)],
            issues: Vec::new(),
        },
        &data_root,
        &index_root,
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

#[test]
fn scoped_admission_preserves_typed_registration_failure_details() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, cwd, discovery) = discovery_fixture(temp.path());
    let database = cwd.join("shelley.db");
    std::fs::write(&database, b"not sqlite").unwrap();
    let source = provider_source_for_path(CaptureProvider::Shelley, database);

    let error = source_backed_admitted_discovery_from_report(
        &discovery,
        DiscoveryReport {
            sources: vec![source],
            issues: Vec::new(),
        },
        StdDuration::ZERO,
        &data_root,
        AdmittedRefreshCoverage::SelectedRoutes,
        None,
        &TestPublishedState,
    )
    .unwrap_err();
    let registration_failures = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<SourceBackedAdmissionRouteFailures>())
        .expect("typed scoped registration error");
    assert_eq!(registration_failures.failures().len(), 1);
    let failure = &registration_failures.failures()[0];
    assert_eq!(failure.kind(), SourceBackedRouteErrorKind::InvalidSource);
    assert!(
        failure.detail().contains("not a database"),
        "unexpected Shelley failure detail: {failure:?}"
    );
    assert!(
        failure
            .detail()
            .contains("source-backed route registration failed for shelley"),
        "unexpected Shelley failure phase: {failure:?}"
    );
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
