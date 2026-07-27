use super::*;

#[test]
fn catalog_sessions_count_indexed_and_stale_rows() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = timestamp_ms(fixed_time());
    let session = catalog_session(
        "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
        "codex-session-1",
        cataloged_at_ms,
    );
    store
        .upsert_catalog_sessions(std::slice::from_ref(&session))
        .unwrap();

    let counts = store.catalog_session_counts().unwrap();
    assert_eq!(counts.total, 1);
    assert_eq!(counts.indexed, 0);
    assert_eq!(counts.stale, 0);
    assert_eq!(counts.pending, 1);
    assert_eq!(counts.failed, 0);
    assert_eq!(
        store
            .catalog_source_stale_session_count(
                CaptureProvider::Codex,
                "/home/user/.codex/sessions"
            )
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .list_pending_catalog_sessions(CaptureProvider::Codex, "/home/user/.codex/sessions")
            .unwrap()
            .len(),
        1
    );

    store
        .upsert_session(&imported_session("codex-session-1"))
        .unwrap();
    store
        .mark_catalog_source_observation_indexed(&session, None, Some(3), cataloged_at_ms + 10)
        .unwrap();
    let counts = store.catalog_session_counts().unwrap();
    assert_eq!(counts.indexed, 1);
    assert_eq!(counts.pending, 0);

    store
        .mark_catalog_source_stale(
            CaptureProvider::Codex,
            "/home/user/.codex/sessions",
            cataloged_at_ms + 1,
        )
        .unwrap();
    let counts = store.catalog_session_counts().unwrap();
    assert_eq!(counts.total, 0);
    assert_eq!(counts.indexed, 0);
    assert_eq!(counts.stale, 1);
    assert_eq!(counts.pending, 0);
    assert_eq!(
        store
            .catalog_source_stale_session_count(
                CaptureProvider::Codex,
                "/home/user/.codex/sessions"
            )
            .unwrap(),
        1
    );
}

#[test]
fn source_import_file_counts_track_pending_indexed_failed_and_stale() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let observed_at_ms = timestamp_ms(fixed_time());
    let root = "/home/user/.claude/projects";
    let files = ["indexed.jsonl", "pending.jsonl", "failed.jsonl"]
        .into_iter()
        .map(|name| SourceImportFile {
            provider: CaptureProvider::Claude,
            source_format: "claude_projects_jsonl_tree".into(),
            source_root: root.into(),
            source_path: format!("{root}/{name}"),
            file_size_bytes: 42,
            file_modified_at_ms: observed_at_ms,
            observed_at_ms,
            metadata: serde_json::json!({}),
        })
        .collect::<Vec<_>>();

    store.upsert_source_import_files(&files).unwrap();
    store
        .mark_source_import_file_indexed(&files[0], observed_at_ms + 10)
        .unwrap();
    store
        .mark_source_import_file_failed(&files[2], "bad json", observed_at_ms + 20)
        .unwrap();
    store
        .mark_source_import_missing_paths_stale(
            CaptureProvider::Claude,
            root,
            &[files[0].source_path.clone(), files[2].source_path.clone()],
            observed_at_ms + 30,
        )
        .unwrap();

    let counts = store.source_import_file_counts().unwrap();
    assert_eq!(counts.total, 2);
    assert_eq!(counts.indexed, 1);
    assert_eq!(counts.pending, 1);
    assert_eq!(counts.failed, 1);
    assert_eq!(counts.stale, 1);

    let mut changed_indexed = files[0].clone();
    changed_indexed.file_size_bytes = 43;
    changed_indexed.observed_at_ms = observed_at_ms + 40;
    store
        .upsert_source_import_files(&[changed_indexed])
        .unwrap();

    let counts = store.source_import_file_counts().unwrap();
    assert_eq!(counts.total, 2);
    assert_eq!(counts.indexed, 0);
    assert_eq!(counts.pending, 2);
    assert_eq!(counts.failed, 1);
    assert_eq!(counts.stale, 1);
}
