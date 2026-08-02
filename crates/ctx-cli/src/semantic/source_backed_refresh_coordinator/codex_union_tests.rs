use super::*;

#[test]
fn automatic_and_explicit_codex_roots_publish_through_one_union_route() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let automatic_root = temp.path().join("automatic-codex-sessions");
    let explicit_root = temp.path().join("explicit-codex-sessions");
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

    let explicit_source = provider_source_for_path(CaptureProvider::Codex, explicit_root.clone());
    let upsert =
        crate::commands::import::upsert_explicit_source(&data_root, &explicit_source).unwrap();
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

    let first = refresh_all_provider_sources(
        &discovery,
        report.clone(),
        StdDuration::ZERO,
        &data_root,
        &index_root,
        Some(&upsert.authority),
        &mut progress,
    )
    .unwrap();

    assert_eq!(first.scanned_routes, 1);
    assert_eq!(first.certified_source_count, 2);
    let verified = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(verified.manifest().sources.len(), 2);
    assert_eq!(
        verified
            .search_event_candidates("automaticrootmarker", 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        verified
            .search_event_candidates("explicitrootmarker", 10)
            .unwrap()
            .len(),
        1
    );
    drop(verified);

    let replay = refresh_all_provider_sources(
        &discovery,
        report,
        StdDuration::ZERO,
        &data_root,
        &index_root,
        Some(&upsert.authority),
        &mut progress,
    )
    .unwrap();
    assert_eq!(replay.generation_id, first.generation_id);
    assert_eq!(replay.scanned_routes, 1);
    assert_eq!(replay.certified_source_count, 2);
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
