//! Codex-union coverage owned by the refresh engine.

use super::*;

fn refresh_exact_source(
    discovery: &DiscoveryContext,
    authority: &ExplicitSourceCatalogAuthority,
    data_root: &Path,
    index_root: &Path,
    report_progress: &mut dyn FnMut(
        CaptureSourceBackedDetailedRefreshProgress,
    ) -> SourceBackedRouteResult<()>,
) -> Result<SourceBackedRefreshPublication> {
    let report = authority.admission_discovery_report(data_root)?;
    refresh_exact_source_from_report(
        discovery,
        authority,
        data_root,
        index_root,
        report,
        report_progress,
    )
}

fn refresh_exact_source_from_report(
    discovery: &DiscoveryContext,
    authority: &ExplicitSourceCatalogAuthority,
    data_root: &Path,
    index_root: &Path,
    report: DiscoveryReport,
    report_progress: &mut dyn FnMut(
        CaptureSourceBackedDetailedRefreshProgress,
    ) -> SourceBackedRouteResult<()>,
) -> Result<SourceBackedRefreshPublication> {
    let journal = TestRefreshJournal::default();
    let published_state = crate::orchestration::RetainedPublishedState { journal: &journal };
    let admitted = ctx_history_refresh_execution::source_backed_admitted_discovery_from_report(
        discovery,
        report.clone(),
        StdDuration::ZERO,
        data_root,
        ctx_history_refresh_execution::AdmittedRefreshCoverage::SelectedRoutes,
        Some(authority),
        &published_state,
    )?;
    let exact_routes = admitted.exact_routes().clone();
    assert!(!exact_routes.is_empty(), "exact fixture must admit a route");
    crate::orchestration::refresh_all_provider_sources_route_local(
        discovery,
        report,
        StdDuration::ZERO,
        "test-exact-source-refresh",
        SourceBackedRefreshOperation::Import,
        data_root,
        index_root,
        Some(authority),
        SourceBackedRefreshScope::Exact(exact_routes),
        &journal,
        report_progress,
    )
}

#[test]
fn automatic_and_explicit_empty_routes_preserve_generation_bound_authority() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let automatic_root = temp.path().join("automatic-codex-sessions");
    let explicit_root = temp.path().join("explicit-codex-sessions");
    for path in [&home, &cwd, &automatic_root, &explicit_root] {
        fs::create_dir_all(path).unwrap();
    }
    let automatic = provider_source_for_path(CaptureProvider::Codex, automatic_root);
    let explicit = provider_source_for_path(CaptureProvider::Codex, explicit_root);
    let upsert = crate::upsert_explicit_source(&data_root, &explicit).unwrap();
    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let mut progress =
        |_: CaptureSourceBackedDetailedRefreshProgress| Ok::<(), SourceBackedRouteError>(());

    let automatic_publication = refresh_all_provider_sources(
        &discovery,
        DiscoveryReport {
            sources: vec![automatic],
            issues: Vec::new(),
        },
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
    .unwrap();
    let publication = refresh_exact_source(
        &discovery,
        &upsert.authority,
        &data_root,
        &index_root,
        &mut progress,
    )
    .unwrap();

    assert_eq!(automatic_publication.route_results.len(), 1);
    assert_eq!(automatic_publication.zero_source_authority.len(), 1);
    assert_eq!(publication.route_results.len(), 1);
    assert!(!publication.zero_source_authority.is_empty());
    assert!(publication
        .zero_source_authority
        .iter()
        .all(|authority| authority.generation_id == publication.generation_id));
    assert!(
        verified_generation_is_query_ready(&VerifiedIndex::open_pinned(&index_root).unwrap())
            .unwrap()
    );
}

#[test]
fn successful_codex_route_derives_rejected_records_from_committed_core_sources() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let root = temp.path().join("codex-sessions");
    let session = "019fb600-0000-7000-8000-000000000010";
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    write_codex_rollout(&root, session, "validpeerrecordmarker");
    let rollout = root.join(format!("rollout-{session}.jsonl"));
    let mut bytes = fs::read(&rollout).unwrap();
    bytes.extend_from_slice(b"{not-json}\n");
    fs::write(&rollout, bytes).unwrap();
    let source = provider_source_for_path(CaptureProvider::Codex, root);
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
        &mut progress,
    )
    .unwrap();

    let [result] = publication.route_results.as_slice() else {
        panic!("one selected Codex route expected");
    };
    assert!(result.outcome.is_success());
    assert_eq!(result.rejected_record_total, 1);
    assert_eq!(publication.current.rejected_records, 1);
    assert_eq!(
        complete_lexical_candidates(
            &VerifiedIndex::open_pinned(&index_root).unwrap(),
            "validpeerrecordmarker",
            10,
        )
        .unwrap()
        .len(),
        1
    );
}

#[test]
fn long_codex_page_reports_parsing_before_durable_acceptance() {
    use std::io::{BufWriter, Write};

    const IGNORED_ROWS: usize = 4 * 1024;
    const IGNORED_PADDING_BYTES: usize = 8 * 1024;

    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let root = temp.path().join("long-codex-page");
    let session = "019fb600-0000-7000-8000-000000000011";
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    let rollout = write_codex_rollout(&root, session, "longpageactivitymarker");
    let file = fs::OpenOptions::new().append(true).open(&rollout).unwrap();
    let mut writer = BufWriter::new(file);
    let padding = "x".repeat(IGNORED_PADDING_BYTES);
    let ignored = format!(
        "{{\"timestamp\":\"2026-07-30T12:00:02Z\",\"type\":\"telemetry\\u005fignored\",\"payload\":{{\"padding\":\"{padding}\"}}}}\n"
    );
    for _ in 0..IGNORED_ROWS {
        writer.write_all(ignored.as_bytes()).unwrap();
    }
    writer.flush().unwrap();
    drop(writer);

    let report = DiscoveryReport {
        sources: vec![provider_source_for_path(CaptureProvider::Codex, root)],
        issues: Vec::new(),
    };
    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let mut updates = Vec::new();
    let mut progress = |update: CaptureSourceBackedDetailedRefreshProgress| {
        updates.push(update);
        Ok::<(), SourceBackedRouteError>(())
    };

    let publication = refresh_all_provider_sources(
        &discovery,
        report,
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
    .unwrap();

    assert_eq!(publication.current.indexed_documents, 1);
    let accepted = updates
        .iter()
        .position(|update| update.progress.processed_messages == 1)
        .expect("the retained Codex message must eventually be durably accepted");
    let parsing = updates
        .iter()
        .position(|update| {
            update.current_source_progress.is_some_and(|progress| {
                progress.stage
                    == ctx_history_capture::SourceBackedCurrentSourceProgressStage::Parsing
            })
        })
        .expect("missing Parsing activity for a long Codex page");
    assert!(
        parsing < accepted,
        "Parsing must precede durable acceptance"
    );
    assert_eq!(updates[parsing].progress.processed_sessions, 0);
    assert_eq!(updates[parsing].progress.processed_messages, 0);
    assert_eq!(updates[parsing].progress.processed_tool_calls, 0);
    assert_eq!(updates[parsing].progress.processed_bytes, 0);
}

#[test]
fn codex_nested_root_advisory_is_admitted_from_each_childs_own_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let sessions = temp.path().join("codex-nested-sessions");
    let root = "019fb600-0000-7000-8000-0000000032a0";
    let child = "019fb600-0000-7000-8000-0000000032a1";
    let grandchild = "019fb600-0000-7000-8000-0000000032a2";
    let great_grandchild = "019fb600-0000-7000-8000-0000000032a3";
    let root_marker = "nestedrootcanary328";
    let child_marker = "nestedchildcanary328";
    let grandchild_marker = "nestedgrandchildcanary328";
    let great_grandchild_marker = "nestedgreatgrandchildcanary328";
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    write_codex_rollout_with_lineage(&sessions, root, None, Some(root), root_marker);
    write_codex_root_advisory_rollout(&sessions, child, root, root, child_marker);
    write_codex_root_advisory_rollout(&sessions, grandchild, child, root, grandchild_marker);
    write_codex_root_advisory_rollout(
        &sessions,
        great_grandchild,
        grandchild,
        root,
        great_grandchild_marker,
    );
    let report = DiscoveryReport {
        sources: vec![provider_source_for_path(
            CaptureProvider::Codex,
            sessions.clone(),
        )],
        issues: Vec::new(),
    };
    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
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
        &mut progress,
    )
    .unwrap();

    let [result] = publication.route_results.as_slice() else {
        panic!("one selected Codex route expected");
    };
    assert!(result.outcome.is_success());
    assert_eq!(result.source_failure_total, 0);
    assert!(result.source_failures.is_empty());
    assert_eq!(result.rejected_record_total, 0);
    assert!(result.rejection_diagnostics.is_empty());
    assert_eq!(publication.current.source_count, 4);
    assert_eq!(publication.certified_source_count, 4);
    assert_eq!(publication.current.rejected_records, 0);
    let verified = VerifiedIndex::open_pinned(&index_root).unwrap();
    let records = codex_core_records(&verified);
    assert_eq!(records.len(), 4);
    for marker in [
        root_marker,
        child_marker,
        grandchild_marker,
        great_grandchild_marker,
    ] {
        assert_eq!(
            complete_lexical_candidates(&verified, marker, 10)
                .unwrap()
                .len(),
            1
        );
    }
    let by_marker = |marker: &str| {
        records
            .iter()
            .find(|record| record.content.normalized_body.as_deref() == Some(marker))
            .unwrap()
    };
    let root_record = by_marker(root_marker);
    let child_record = by_marker(child_marker);
    let grandchild_record = by_marker(grandchild_marker);
    let great_grandchild_record = by_marker(great_grandchild_marker);
    assert_eq!(root_record.provider_session_id.as_deref(), Some(root));
    assert_eq!(
        root_record
            .session_relationship
            .as_ref()
            .map(|relationship| relationship.as_str()),
        Some("root")
    );
    assert_eq!(root_record.root_session_id, Some(root_record.session_id));
    assert!(root_record.parent_session_id.is_none());
    assert_eq!(child_record.provider_session_id.as_deref(), Some(child));
    assert_eq!(child_record.parent_session_id, Some(root_record.session_id));
    assert_eq!(child_record.root_session_id, Some(root_record.session_id));
    assert_eq!(
        grandchild_record.provider_session_id.as_deref(),
        Some(grandchild)
    );
    assert_eq!(
        grandchild_record.parent_session_id,
        Some(child_record.session_id)
    );
    assert_eq!(
        grandchild_record.root_session_id,
        Some(root_record.session_id)
    );
    assert_eq!(
        great_grandchild_record.provider_session_id.as_deref(),
        Some(great_grandchild)
    );
    assert_eq!(
        great_grandchild_record.parent_session_id,
        Some(grandchild_record.session_id)
    );
    assert_eq!(
        great_grandchild_record.root_session_id,
        Some(root_record.session_id)
    );
}

#[test]
fn codex_deep_root_advisory_chain_has_exact_linear_cardinality() {
    const DESCENDANTS: usize = 65;
    const SOURCES: usize = DESCENDANTS + 1;

    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let sessions = temp.path().join("deep-codex-lineage");
    let root = "019fb600-0000-7000-8000-000000003400";
    let root_marker = "deeprootadvisoryuniquetoken328";
    let deepest_marker = "deepestrootadvisoryuniquetokenxyz328";
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    write_codex_rollout_with_lineage(&sessions, root, None, Some(root), root_marker);
    let mut parent = root.to_owned();
    for index in 0..DESCENDANTS {
        let child = format!("019fb600-0000-7000-8001-{index:012x}");
        let marker = if index + 1 == DESCENDANTS {
            deepest_marker
        } else {
            "intermediaterootadvisorytoken328"
        };
        write_codex_root_advisory_rollout(&sessions, &child, &parent, root, marker);
        parent = child;
    }
    let report = DiscoveryReport {
        sources: vec![provider_source_for_path(
            CaptureProvider::Codex,
            sessions.clone(),
        )],
        issues: Vec::new(),
    };
    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
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
        &mut progress,
    )
    .unwrap();

    let [result] = publication.route_results.as_slice() else {
        panic!("one selected Codex route expected");
    };
    assert!(result.outcome.is_success());
    assert_eq!(result.source_failure_total, 0);
    assert!(result.source_failures.is_empty());
    assert_eq!(result.rejected_record_total, 0);
    assert!(result.rejection_diagnostics.is_empty());
    assert_eq!(publication.current.source_count, SOURCES);
    assert_eq!(publication.certified_source_count, SOURCES);
    assert_eq!(publication.current.rejected_records, 0);
    let verified = VerifiedIndex::open_pinned(&index_root).unwrap();
    let records = codex_core_records(&verified);
    assert_eq!(records.len(), SOURCES);
    assert_eq!(
        records
            .iter()
            .filter_map(|record| record.provider_session_id.as_deref())
            .collect::<BTreeSet<_>>()
            .len(),
        SOURCES
    );
    assert_eq!(
        complete_lexical_candidates(&verified, deepest_marker, 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn registered_codex_parent_and_exact_subdir_share_route_scoped_ownership() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let automatic_root = temp.path().join("codex-sessions");
    let explicit_root = automatic_root.join("2026/08/02");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    write_codex_rollout(
        &automatic_root,
        "019fb600-0000-7000-8000-000000000001",
        "automaticrootmarker",
    );
    let explicit_rollout = write_codex_rollout(
        &explicit_root,
        "019fb600-0000-7000-8000-000000000002",
        "explicitrootmarker",
    );

    let automatic_source = provider_source_for_path(CaptureProvider::Codex, automatic_root.clone());
    let report = DiscoveryReport {
        sources: vec![automatic_source],
        issues: Vec::new(),
    };
    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let mut progress =
        |_: CaptureSourceBackedDetailedRefreshProgress| Ok::<(), SourceBackedRouteError>(());

    let parent_publication = refresh_all_provider_sources(
        &discovery,
        report.clone(),
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
    .unwrap();
    assert_eq!(parent_publication.route_results.len(), 1);
    assert!(parent_publication.route_results[0].outcome.is_success());
    let parent_generation = parent_publication.generation_id.clone();
    let automatic_catalog =
        ctx_history_capture::build_automatic_source_backed_registry_from_report(
            &discovery,
            &data_root,
            report.clone(),
        )
        .registry
        .watch_catalog();

    let explicit_source = provider_source_for_path(CaptureProvider::Codex, explicit_root.clone());
    let upsert = crate::upsert_explicit_source(&data_root, &explicit_source).unwrap();
    let exact_report = upsert
        .authority
        .admission_discovery_report_with_automatic_catalog(&data_root, &automatic_catalog)
        .unwrap();
    let first = refresh_exact_source_from_report(
        &discovery,
        &upsert.authority,
        &data_root,
        &index_root,
        exact_report,
        &mut progress,
    )
    .unwrap();

    assert_eq!(first.route_results.len(), 1);
    assert!(first.certified_source_count >= 1);
    assert_eq!(
        first
            .route_results
            .iter()
            .map(|result| result.source_failure_total)
            .sum::<usize>(),
        0
    );
    assert!(first.published_explicit_source_catalog.is_some());
    let [binding] = first.catalog_route_bindings.as_slice() else {
        panic!("successful explicit request should retain one route binding");
    };
    assert_eq!(
        first.route_results[0].route_identity,
        binding.route_identity
    );
    assert_eq!(
        first.route_results[0].route_identity,
        parent_publication.route_results[0].route_identity
    );
    assert!(first.route_results[0].outcome.is_success());
    let verified = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert!(!verified.manifest().sources.is_empty());
    assert_eq!(
        complete_lexical_candidates(&verified, "automaticrootmarker", 10)
            .unwrap()
            .len(),
        1
    );
    // The parent automatic route retains its existing exact source ownership;
    // the successful exact import owns no duplicate sources.
    assert_eq!(
        complete_lexical_candidates(&verified, "explicitrootmarker", 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(first.generation_id, parent_generation);
    drop(verified);

    append_codex_message(&explicit_rollout, "explicitappendafterimportmarker");
    let replay_report = upsert
        .authority
        .admission_discovery_report_with_automatic_catalog(&data_root, &automatic_catalog)
        .unwrap();
    let replay = refresh_exact_source_from_report(
        &discovery,
        &upsert.authority,
        &data_root,
        &index_root,
        replay_report,
        &mut progress,
    )
    .unwrap();
    assert_ne!(replay.generation_id, first.generation_id);
    assert_eq!(replay.route_results.len(), 1);
    assert!(replay.published_explicit_source_catalog.is_some());
    assert_eq!(replay.catalog_route_bindings.len(), 1);
    assert!(replay.certified_source_count >= 1);
    assert_eq!(
        replay
            .route_results
            .iter()
            .map(|result| result.source_failure_total)
            .sum::<usize>(),
        0
    );
    assert_eq!(
        complete_lexical_candidates(
            &VerifiedIndex::open_pinned(&index_root).unwrap(),
            "explicitappendafterimportmarker",
            10,
        )
        .unwrap()
        .len(),
        1
    );
}

#[test]
fn automatic_parent_and_explicit_directory_child_normalize_in_one_generation() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let automatic_root = temp.path().join("automatic-codex-sessions");
    let explicit_root = temp.path().join("explicit-codex-sessions");
    let parent = "019fb600-0000-7000-8000-000000000101";
    let child = "019fb600-0000-7000-8000-000000000102";
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    write_codex_rollout(&automatic_root, parent, "automaticparentdirectorymarker");
    write_codex_related_rollout(
        &explicit_root,
        child,
        parent,
        "explicitdirectorychildmarker",
    );

    let report = DiscoveryReport {
        sources: vec![provider_source_for_path(
            CaptureProvider::Codex,
            automatic_root,
        )],
        issues: Vec::new(),
    };
    let explicit_source = provider_source_for_path(CaptureProvider::Codex, explicit_root);
    let upsert = crate::upsert_explicit_source(&data_root, &explicit_source).unwrap();
    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let mut progress =
        |_: CaptureSourceBackedDetailedRefreshProgress| Ok::<(), SourceBackedRouteError>(());

    let parent_publication = refresh_all_provider_sources(
        &discovery,
        report,
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
    .unwrap();
    assert_eq!(parent_publication.route_results.len(), 1);
    let publication = refresh_exact_source(
        &discovery,
        &upsert.authority,
        &data_root,
        &index_root,
        &mut progress,
    )
    .unwrap();

    assert_eq!(publication.route_results.len(), 1);
    assert!(publication
        .route_results
        .iter()
        .all(|result| result.outcome.is_success()));
    assert_eq!(publication.certified_source_count, 2);
    assert_codex_parent_child_records(
        &VerifiedIndex::open_pinned(&index_root).unwrap(),
        "automaticparentdirectorymarker",
        "explicitdirectorychildmarker",
    );
}

#[test]
fn automatic_parent_and_explicit_file_child_normalize_in_one_generation() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let automatic_root = temp.path().join("automatic-codex-sessions");
    let explicit_root = temp.path().join("explicit-codex-session");
    let parent = "019fb600-0000-7000-8000-000000000111";
    let child = "019fb600-0000-7000-8000-000000000112";
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    write_codex_rollout(&automatic_root, parent, "automaticparentfilemarker");
    let child_path =
        write_codex_related_rollout(&explicit_root, child, parent, "explicitfilechildmarker");

    let report = DiscoveryReport {
        sources: vec![provider_source_for_path(
            CaptureProvider::Codex,
            automatic_root,
        )],
        issues: Vec::new(),
    };
    let explicit_source = provider_source_for_path(CaptureProvider::Codex, child_path);
    let upsert = crate::upsert_explicit_source(&data_root, &explicit_source).unwrap();
    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let mut progress =
        |_: CaptureSourceBackedDetailedRefreshProgress| Ok::<(), SourceBackedRouteError>(());

    let parent_publication = refresh_all_provider_sources(
        &discovery,
        report,
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
    .unwrap();
    assert_eq!(parent_publication.route_results.len(), 1);
    let publication = refresh_exact_source(
        &discovery,
        &upsert.authority,
        &data_root,
        &index_root,
        &mut progress,
    )
    .unwrap();

    assert_eq!(publication.route_results.len(), 1);
    assert!(publication
        .route_results
        .iter()
        .all(|result| result.outcome.is_success()));
    assert_eq!(publication.certified_source_count, 2);
    assert_codex_parent_child_records(
        &VerifiedIndex::open_pinned(&index_root).unwrap(),
        "automaticparentfilemarker",
        "explicitfilechildmarker",
    );
}

#[test]
fn explicit_child_without_selected_parent_publishes_unresolved_and_never_reroots() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let automatic_root = temp.path().join("automatic-codex-sessions");
    let unselected_parent_root = temp.path().join("unselected-codex-sessions");
    let explicit_root = temp.path().join("explicit-codex-session");
    let selected_root = "019fb600-0000-7000-8000-000000000121";
    let unselected_parent = "019fb600-0000-7000-8000-000000000129";
    let child = "019fb600-0000-7000-8000-000000000122";
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    write_codex_rollout(
        &automatic_root,
        selected_root,
        "selectedunrelatedrootmarker",
    );
    write_codex_rollout(
        &unselected_parent_root,
        unselected_parent,
        "unselectedparentmarker",
    );
    let child_path = write_codex_related_rollout(
        &explicit_root,
        child,
        unselected_parent,
        "missingparentchildmarker",
    );

    let report = DiscoveryReport {
        sources: vec![provider_source_for_path(
            CaptureProvider::Codex,
            automatic_root,
        )],
        issues: Vec::new(),
    };
    let explicit_source = provider_source_for_path(CaptureProvider::Codex, child_path);
    let upsert = crate::upsert_explicit_source(&data_root, &explicit_source).unwrap();
    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let mut progress =
        |_: CaptureSourceBackedDetailedRefreshProgress| Ok::<(), SourceBackedRouteError>(());

    let selected_root_publication = refresh_all_provider_sources(
        &discovery,
        report,
        StdDuration::ZERO,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
    .unwrap();
    assert_eq!(selected_root_publication.route_results.len(), 1);
    let publication = refresh_exact_source(
        &discovery,
        &upsert.authority,
        &data_root,
        &index_root,
        &mut progress,
    )
    .unwrap();

    assert_eq!(publication.route_results.len(), 1);
    let [binding] = publication.catalog_route_bindings.as_slice() else {
        panic!("one explicit child route binding expected");
    };
    let child_route = publication
        .route_results
        .iter()
        .find(|result| result.route_identity == binding.route_identity)
        .unwrap();
    assert!(child_route.outcome.is_success());
    assert_eq!(child_route.source_failure_total, 0);
    assert!(publication
        .route_results
        .iter()
        .all(|result| result.outcome.is_success()));
    let records = codex_core_records(&VerifiedIndex::open_pinned(&index_root).unwrap());
    assert_eq!(records.len(), 2);
    let selected = records
        .iter()
        .find(|record| {
            record.content.normalized_body.as_deref() == Some("selectedunrelatedrootmarker")
        })
        .unwrap();
    let child = records
        .iter()
        .find(|record| {
            record.content.normalized_body.as_deref() == Some("missingparentchildmarker")
        })
        .unwrap();
    assert_eq!(
        selected
            .session_relationship
            .as_ref()
            .map(|relationship| relationship.as_str()),
        Some("root")
    );
    assert_eq!(selected.root_session_id, Some(selected.session_id));
    assert!(selected.parent_session_id.is_none());
    assert_eq!(
        child
            .session_relationship
            .as_ref()
            .map(|relationship| relationship.as_str()),
        Some("forked")
    );
    let unresolved_parent = child.parent_session_id.expect("direct parent claim");
    assert_eq!(child.root_session_id, Some(unresolved_parent));
    assert_ne!(unresolved_parent, selected.session_id);
    assert!(records
        .iter()
        .all(|record| record.session_id != unresolved_parent));
    assert!(records.iter().all(|record| {
        record.content.normalized_body.as_deref() != Some("unselectedparentmarker")
    }));
}

fn assert_codex_parent_child_records(
    index: &VerifiedIndex,
    parent_marker: &str,
    child_marker: &str,
) {
    let records = codex_core_records(index);
    assert_eq!(records.len(), 2);
    let parent = records
        .iter()
        .find(|record| record.content.normalized_body.as_deref() == Some(parent_marker))
        .unwrap();
    let child = records
        .iter()
        .find(|record| record.content.normalized_body.as_deref() == Some(child_marker))
        .unwrap();
    assert_eq!(
        parent
            .session_relationship
            .as_ref()
            .map(|relationship| relationship.as_str()),
        Some("root")
    );
    assert!(parent.parent_session_id.is_none());
    assert_eq!(parent.root_session_id, Some(parent.session_id));
    assert_eq!(
        child
            .session_relationship
            .as_ref()
            .map(|relationship| relationship.as_str()),
        Some("forked")
    );
    assert_eq!(child.parent_session_id, Some(parent.session_id));
    assert_eq!(child.root_session_id, Some(parent.session_id));
    assert_ne!(child.session_id, parent.session_id);
}

fn codex_core_records(index: &VerifiedIndex) -> Vec<CoreRecord> {
    let mut records = Vec::new();
    for source in &index.manifest().sources {
        let page = index
            .source_event_page(source.observation().source(), None, 256)
            .unwrap();
        assert!(page.next_cursor.is_none());
        for event in page.items {
            records.push(
                index
                    .core_record_by_id(event.event_id.as_uuid())
                    .unwrap()
                    .unwrap(),
            );
        }
    }
    records
}

pub(super) fn write_codex_rollout(root: &Path, native_session_id: &str, text: &str) -> PathBuf {
    write_codex_rollout_with_parent(root, native_session_id, None, text)
}

fn append_codex_message(path: &Path, text: &str) {
    use std::io::Write;

    let message = json!({
        "timestamp": "2026-07-30T12:00:02Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": text
            }]
        }
    });
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{message}").unwrap();
}

fn write_codex_related_rollout(
    root: &Path,
    native_session_id: &str,
    parent_native_session_id: &str,
    text: &str,
) -> PathBuf {
    write_codex_rollout_with_parent(
        root,
        native_session_id,
        Some(parent_native_session_id),
        text,
    )
}

fn write_codex_root_advisory_rollout(
    root: &Path,
    native_session_id: &str,
    parent_native_session_id: &str,
    advisory_session_id: &str,
    text: &str,
) -> PathBuf {
    write_codex_rollout_with_lineage(
        root,
        native_session_id,
        Some(parent_native_session_id),
        Some(advisory_session_id),
        text,
    )
}

fn write_codex_rollout_with_parent(
    root: &Path,
    native_session_id: &str,
    parent_native_session_id: Option<&str>,
    text: &str,
) -> PathBuf {
    write_codex_rollout_with_lineage(
        root,
        native_session_id,
        parent_native_session_id,
        parent_native_session_id,
        text,
    )
}

fn write_codex_rollout_with_lineage(
    root: &Path,
    native_session_id: &str,
    parent_native_session_id: Option<&str>,
    advisory_session_id: Option<&str>,
    text: &str,
) -> PathBuf {
    fs::create_dir_all(root).unwrap();
    let mut payload = json!({
        "id": native_session_id,
        "timestamp": "2026-07-30T12:00:00Z",
        "cwd": "/repo/union-route",
        "originator": "codex_cli_rs",
        "cli_version": "1.0.0",
        "source": "cli",
        "model_provider": "openai"
    });
    if let Some(parent) = parent_native_session_id {
        payload["forked_from_id"] = json!(parent);
    }
    if let Some(advisory) = advisory_session_id {
        // `payload.session_id` is advisory. The structural parent marker
        // remains authoritative for the native child identity and edge.
        payload["session_id"] = json!(advisory);
    }
    let session_meta = json!({
        "timestamp": "2026-07-30T12:00:00Z",
        "type": "session_meta",
        "payload": payload,
    });
    let message = json!({
        "timestamp": "2026-07-30T12:00:01Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": text
            }]
        }
    });
    let path = root.join(format!("rollout-{native_session_id}.jsonl"));
    fs::write(&path, format!("{session_meta}\n{message}\n")).unwrap();
    path
}
