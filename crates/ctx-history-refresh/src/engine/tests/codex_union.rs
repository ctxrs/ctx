//! Codex-union coverage owned by the refresh engine.

use super::*;

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

fn write_codex_rollout(root: &Path, native_session_id: &str, text: &str) {
    fs::create_dir_all(root).unwrap();
    let session_meta = json!({
        "timestamp": "2026-07-30T12:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": native_session_id,
            "timestamp": "2026-07-30T12:00:00Z",
            "cwd": "/repo/union-route",
            "originator": "codex_cli_rs",
            "cli_version": "1.0.0",
            "source": "cli",
            "model_provider": "openai"
        }
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
    fs::write(
        root.join(format!("rollout-{native_session_id}.jsonl")),
        format!("{session_meta}\n{message}\n"),
    )
    .unwrap();
}
