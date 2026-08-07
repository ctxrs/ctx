//! Codex-union coverage owned by the refresh engine.

use super::*;

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

    let publication = refresh_all_provider_sources(
        &discovery,
        DiscoveryReport {
            sources: vec![automatic],
            issues: Vec::new(),
        },
        StdDuration::ZERO,
        &data_root,
        &index_root,
        Some(&upsert.authority),
        SourceBackedRefreshScope::All,
        &BTreeSet::new(),
        &mut progress,
    )
    .unwrap();

    assert_eq!(publication.route_results.len(), 2);
    assert_eq!(publication.zero_source_authority.len(), 2);
    assert!(publication
        .zero_source_authority
        .iter()
        .all(|authority| authority.generation_id == publication.generation_id));
    assert!(
        verified_generation_is_query_ready(&VerifiedIndex::open(&index_root).unwrap()).unwrap()
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
        &BTreeSet::new(),
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
        VerifiedIndex::open(&index_root)
            .unwrap()
            .search_event_candidates("validpeerrecordmarker", 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn codex_root_conflict_projects_source_failures_while_valid_peer_publishes() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let root = temp.path().join("private-codex-sessions");
    let root_a = "019fb600-0000-7000-8000-0000000032a0";
    let child_a = "019fb600-0000-7000-8000-0000000032a1";
    let root_b = "019fb600-0000-7000-8000-0000000032b0";
    let private_root_marker = "privaterootacanary328";
    let private_child_marker = "privatechildacanary328";
    let valid_marker = "validrootbpublicationmarker";
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    write_codex_rollout(&root, root_a, private_root_marker);
    write_codex_root_conflict_rollout(&root, child_a, root_a, root_b, private_child_marker);
    write_codex_rollout(&root, root_b, valid_marker);
    let report = DiscoveryReport {
        sources: vec![provider_source_for_path(
            CaptureProvider::Codex,
            root.clone(),
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
        &BTreeSet::new(),
        &mut progress,
    )
    .unwrap();

    let [result] = publication.route_results.as_slice() else {
        panic!("one selected Codex route expected");
    };
    assert!(result.outcome.is_success());
    assert_eq!(result.source_failure_total, 2);
    assert_eq!(result.source_failures.len(), 2);
    assert_eq!(result.rejected_record_total, 0);
    assert!(result.rejection_diagnostics.is_empty());
    assert_eq!(publication.current.source_count, 1);
    assert_eq!(publication.current.rejected_records, 0);
    for failure in &result.source_failures {
        assert!(failure.source_selector.starts_with("logical-source:"));
        for expected in [
            format!("computed_root_native_session_id={root_a}"),
            format!("conflicting_advisory_session_id={root_b}"),
            format!("evidence_source_record=session_meta:{child_a}"),
            format!("computed_root_source_record=session_meta:{root_a}"),
            format!("advisory_source_record=session_meta:{root_b}"),
        ] {
            assert!(failure.detail.contains(&expected), "{}", failure.detail);
        }
        assert!(!failure.detail.contains(root.to_str().unwrap()));
        assert!(!failure.detail.contains(private_root_marker));
        assert!(!failure.detail.contains(private_child_marker));
    }
    let verified = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(
        verified
            .search_event_candidates(valid_marker, 10)
            .unwrap()
            .len(),
        1
    );
    assert!(verified
        .search_event_candidates(private_root_marker, 10)
        .unwrap()
        .is_empty());
    assert!(verified
        .search_event_candidates(private_child_marker, 10)
        .unwrap()
        .is_empty());
}

#[test]
fn codex_root_conflict_receipt_keeps_exact_total_and_bounded_path_safe_diagnostics() {
    const CONFLICTING_CHILDREN: usize = 65;
    const REJECTED_SOURCES: usize = CONFLICTING_CHILDREN + 1;

    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let root = temp.path().join("private-bounded-codex-conflicts");
    let root_a = "019fb600-0000-7000-8000-000000003400";
    let root_b = "019fb600-0000-7000-8000-0000000034b0";
    let evidence_child = "019fb600-0000-7000-8001-000000000000";
    let private_marker = "private bounded root conflict content 328";
    let valid_marker = "bounded root conflict valid peer 328";
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    write_codex_rollout(&root, root_a, private_marker);
    for index in 0..CONFLICTING_CHILDREN {
        let child = format!("019fb600-0000-7000-8001-{index:012x}");
        if index == 0 {
            write_codex_root_conflict_rollout(&root, &child, root_a, root_b, private_marker);
        } else {
            write_codex_related_rollout(&root, &child, root_a, private_marker);
        }
    }
    write_codex_rollout(&root, root_b, valid_marker);
    let report = DiscoveryReport {
        sources: vec![provider_source_for_path(
            CaptureProvider::Codex,
            root.clone(),
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
        &BTreeSet::new(),
        &mut progress,
    )
    .unwrap();

    let [result] = publication.route_results.as_slice() else {
        panic!("one selected Codex route expected");
    };
    assert!(result.outcome.is_success());
    assert_eq!(result.source_failure_total, REJECTED_SOURCES);
    let bounded_diagnostics = result.source_failures.len();
    assert!((1..=64).contains(&bounded_diagnostics));
    assert!(bounded_diagnostics < REJECTED_SOURCES);
    assert_eq!(result.rejected_record_total, 0);
    assert!(result.rejection_diagnostics.is_empty());
    assert_eq!(publication.current.source_count, 1);
    assert_eq!(publication.current.rejected_records, 0);
    for failure in &result.source_failures {
        assert!(failure.source_selector.starts_with("logical-source:"));
        for expected in [
            format!("computed_root_native_session_id={root_a}"),
            format!("conflicting_advisory_session_id={root_b}"),
            format!("evidence_source_record=session_meta:{evidence_child}"),
            format!("computed_root_source_record=session_meta:{root_a}"),
            format!("advisory_source_record=session_meta:{root_b}"),
        ] {
            assert!(failure.detail.contains(&expected), "{}", failure.detail);
        }
        assert!(!failure.detail.contains(root.to_str().unwrap()));
        assert!(!failure.detail.contains(private_marker));
    }

    let receipt = SourceBackedRefreshReceipt::from_verified_publication(
        None,
        publication.generation_id.clone(),
        &publication,
    )
    .unwrap();
    assert_eq!(receipt.terminal_outcome(), "completed_with_source_failures");
    assert_eq!(receipt.source_failure_total(), REJECTED_SOURCES);
    assert_eq!(
        receipt.source_failure_diagnostic_count(),
        bounded_diagnostics
    );
    assert_eq!(
        receipt.source_failures_omitted(),
        REJECTED_SOURCES - bounded_diagnostics
    );
    assert_eq!(receipt.rejected_record_total(), 0);

    let envelope = receipt.to_json();
    assert_eq!(envelope["source_failure_total"], REJECTED_SOURCES);
    assert_eq!(envelope["rejected_record_total"], 0);
    let route = envelope["route_results"]
        .as_object()
        .and_then(|routes| routes.values().next())
        .and_then(Value::as_array)
        .expect("one compact route receipt");
    let transmitted = route[3]
        .as_array()
        .expect("bounded source failure diagnostics")
        .len();
    assert!(transmitted <= bounded_diagnostics);
    assert_eq!(
        envelope["source_failures_omitted"],
        REJECTED_SOURCES - transmitted
    );
    assert!(
        serde_json::to_vec(&envelope).unwrap().len() <= SOURCE_REFRESH_RECEIPT_JSON_BUDGET_BYTES
    );
    assert_eq!(
        VerifiedIndex::open(&index_root)
            .unwrap()
            .search_event_candidates(valid_marker, 10)
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
    write_codex_rollout(
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
        &BTreeSet::new(),
        &mut progress,
    )
    .unwrap();
    assert_eq!(parent_publication.route_results.len(), 1);
    assert!(parent_publication.route_results[0].outcome.is_success());
    let parent_generation = parent_publication.generation_id.clone();

    let explicit_source = provider_source_for_path(CaptureProvider::Codex, explicit_root.clone());
    let upsert = crate::upsert_explicit_source(&data_root, &explicit_source).unwrap();
    let first = refresh_all_provider_sources(
        &discovery,
        report.clone(),
        StdDuration::ZERO,
        &data_root,
        &index_root,
        Some(&upsert.authority),
        SourceBackedRefreshScope::All,
        &BTreeSet::new(),
        &mut progress,
    )
    .unwrap();

    assert_eq!(first.route_results.len(), 2);
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
    assert!(first.route_results.iter().any(|result| {
        result.route_identity == binding.route_identity && result.outcome.is_success()
    }));
    let verified = VerifiedIndex::open(&index_root).unwrap();
    assert!(!verified.manifest().sources.is_empty());
    assert_eq!(
        verified
            .search_event_candidates("automaticrootmarker", 10)
            .unwrap()
            .len(),
        1
    );
    // The parent automatic route retains its existing exact source ownership;
    // the successful exact overlay owns no duplicate sources.
    assert_eq!(
        verified
            .search_event_candidates("explicitrootmarker", 10)
            .unwrap()
            .len(),
        1
    );
    assert_ne!(first.generation_id, parent_generation);
    drop(verified);

    let replay = refresh_all_provider_sources(
        &discovery,
        report,
        StdDuration::ZERO,
        &data_root,
        &index_root,
        Some(&upsert.authority),
        SourceBackedRefreshScope::All,
        &BTreeSet::new(),
        &mut progress,
    )
    .unwrap();
    assert_eq!(replay.generation_id, first.generation_id);
    assert_eq!(replay.route_results.len(), 2);
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

    let publication = refresh_all_provider_sources(
        &discovery,
        report,
        StdDuration::ZERO,
        &data_root,
        &index_root,
        Some(&upsert.authority),
        SourceBackedRefreshScope::All,
        &BTreeSet::new(),
        &mut progress,
    )
    .unwrap();

    assert_eq!(publication.route_results.len(), 2);
    assert!(publication
        .route_results
        .iter()
        .all(|result| result.outcome.is_success()));
    assert_eq!(publication.certified_source_count, 2);
    assert_codex_parent_child_records(
        &VerifiedIndex::open(&index_root).unwrap(),
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

    let publication = refresh_all_provider_sources(
        &discovery,
        report,
        StdDuration::ZERO,
        &data_root,
        &index_root,
        Some(&upsert.authority),
        SourceBackedRefreshScope::All,
        &BTreeSet::new(),
        &mut progress,
    )
    .unwrap();

    assert_eq!(publication.route_results.len(), 2);
    assert!(publication
        .route_results
        .iter()
        .all(|result| result.outcome.is_success()));
    assert_eq!(publication.certified_source_count, 2);
    assert_codex_parent_child_records(
        &VerifiedIndex::open(&index_root).unwrap(),
        "automaticparentfilemarker",
        "explicitfilechildmarker",
    );
}

#[test]
fn explicit_child_without_selected_parent_is_rejected_and_never_rerooted() {
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

    let publication = refresh_all_provider_sources(
        &discovery,
        report,
        StdDuration::ZERO,
        &data_root,
        &index_root,
        Some(&upsert.authority),
        SourceBackedRefreshScope::All,
        &BTreeSet::new(),
        &mut progress,
    )
    .unwrap();

    assert_eq!(publication.route_results.len(), 2);
    let [binding] = publication.catalog_route_bindings.as_slice() else {
        panic!("one explicit child route binding expected");
    };
    let failed_child_route = publication
        .route_results
        .iter()
        .find(|result| result.route_identity == binding.route_identity)
        .unwrap();
    assert!(failed_child_route.outcome.is_failure());
    assert_eq!(
        failed_child_route.outcome.failure_class(),
        Some("unreadable")
    );
    assert_eq!(failed_child_route.source_failure_total, 1);
    assert_eq!(
        publication
            .route_results
            .iter()
            .filter(|result| result.outcome.is_success())
            .count(),
        1
    );
    let records = codex_core_records(&VerifiedIndex::open(&index_root).unwrap());
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].session_relationship.as_str(), "root");
    assert_eq!(records[0].root_session_id, records[0].session_id);
    assert!(records[0].parent_session_id.is_none());
    assert_eq!(
        records[0].content.normalized_body.as_deref(),
        Some("selectedunrelatedrootmarker")
    );
    assert!(records.iter().all(|record| {
        !matches!(
            record.content.normalized_body.as_deref(),
            Some("missingparentchildmarker" | "unselectedparentmarker")
        )
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
    assert_eq!(parent.session_relationship.as_str(), "root");
    assert!(parent.parent_session_id.is_none());
    assert_eq!(parent.root_session_id, parent.session_id);
    assert_eq!(child.session_relationship.as_str(), "forked");
    assert_eq!(child.parent_session_id, Some(parent.session_id));
    assert_eq!(child.root_session_id, parent.session_id);
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

fn write_codex_root_conflict_rollout(
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
