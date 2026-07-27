use super::*;

#[test]
fn single_file_missing_reconciliation_stales_an_old_path_without_a_control_row() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let root = "/history/single-file-source";
    let old = SourceImportFile {
        provider: CaptureProvider::Custom,
        source_format: "single_file".into(),
        source_root: root.into(),
        source_path: "/history/old-file.json".into(),
        file_size_bytes: 10,
        file_modified_at_ms: 10,
        observed_at_ms: 10,
        metadata: serde_json::json!({}),
    };
    let mut new = SourceImportFile {
        source_path: "/history/new-file.json".into(),
        file_size_bytes: 20,
        file_modified_at_ms: 20,
        observed_at_ms: 20,
        ..old.clone()
    };
    store
        .upsert_source_import_files(&[old.clone(), new.clone()])
        .unwrap();

    assert_eq!(
        store
            .reconcile_source_import_single_file_missing_paths_page(
                CaptureProvider::Custom,
                root,
                20,
                None,
            )
            .unwrap(),
        Some(old.source_path.clone())
    );
    let stale_after_first_observation: i64 = store
        .conn
        .query_row(
            "SELECT is_stale FROM source_import_files WHERE source_path = ?1",
            [&old.source_path],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stale_after_first_observation, 0);

    new.observed_at_ms = 21;
    store
        .upsert_source_import_files(std::slice::from_ref(&new))
        .unwrap();
    assert_eq!(
        store
            .reconcile_source_import_single_file_missing_paths_page(
                CaptureProvider::Custom,
                root,
                21,
                None,
            )
            .unwrap(),
        Some(old.source_path.clone())
    );
    let rows = store
        .conn
        .prepare(
            "SELECT source_path, is_stale FROM source_import_files WHERE source_root = ?1 ORDER BY source_path",
        )
        .unwrap()
        .query_map([root], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(rows, vec![(new.source_path, 0), (old.source_path, 1)]);
}

#[test]
fn malformed_inventory_control_cannot_authorize_reconciliation() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let root = "/home/user/.claude/projects";
    let file = SourceImportFile {
        provider: CaptureProvider::Claude,
        source_format: "claude_projects_jsonl_tree".into(),
        source_root: root.into(),
        source_path: format!("{root}/session.jsonl"),
        file_size_bytes: 42,
        file_modified_at_ms: 1,
        observed_at_ms: 10,
        metadata: serde_json::json!({}),
    };
    store
        .upsert_source_import_files(std::slice::from_ref(&file))
        .unwrap();
    let mut malformed = complete_inventory_control(
        CaptureProvider::Claude,
        "claude_projects_jsonl_tree",
        root,
        20,
    );
    malformed.metadata["inventory_discovery_complete_v1"] = serde_json::json!("true");
    store
        .conn
        .execute(
            r#"
            INSERT INTO source_import_files (
                provider, source_format, source_root, source_path,
                file_size_bytes, file_modified_at_ms, observed_at_ms, metadata_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            params![
                malformed.provider.as_str(),
                malformed.source_format,
                malformed.source_root,
                malformed.source_path,
                malformed.file_size_bytes,
                malformed.file_modified_at_ms,
                malformed.observed_at_ms,
                serde_json::to_string(&malformed.metadata).unwrap(),
            ],
        )
        .unwrap();

    assert!(matches!(
        store.reconcile_source_import_missing_paths_page(CaptureProvider::Claude, root, 20, None,),
        Err(StoreError::Json(_))
    ));
    assert_eq!(
        store
            .source_import_file_stats_for_source(CaptureProvider::Claude, root)
            .unwrap(),
        (1, 42)
    );
    assert_eq!(store.source_import_file_counts().unwrap().stale, 0);
}

#[test]
fn source_import_page_queries_seek_from_the_path_keyset() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let queries = [
        (
            source_import_pending_page_sql(true),
            vec![
                SqlValue::Text("claude".to_owned()),
                SqlValue::Text("/history".to_owned()),
                SqlValue::Text("/history/session-064".to_owned()),
            ],
        ),
        (
            source_import_missing_page_sql(true),
            vec![
                SqlValue::Text("claude".to_owned()),
                SqlValue::Text("/history".to_owned()),
                SqlValue::Integer(100),
                SqlValue::Text("/history/session-064".to_owned()),
            ],
        ),
        (
            source_import_shadowed_page_sql(true),
            vec![
                SqlValue::Text("antigravity".to_owned()),
                SqlValue::Text("/history".to_owned()),
                SqlValue::Integer(100),
                SqlValue::Text("/history/session-064".to_owned()),
            ],
        ),
    ];

    for (query, parameters) in queries {
        let mut stmt = store
            .conn
            .prepare(format!("EXPLAIN QUERY PLAN {query}").as_str())
            .unwrap();
        let details = stmt
            .query_map(params_from_iter(parameters.iter()), |row| {
                row.get::<_, String>(3)
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("source_path>?")),
            "expected a source_path range seek, got {details:?}"
        );
        assert!(
            details
                .iter()
                .all(|detail| !detail.contains("USE TEMP B-TREE")),
            "keyset page should not materialize a sort: {details:?}"
        );
    }
}

#[test]
fn source_import_missing_path_reconciliation_is_bounded_and_idempotent() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let root = "/home/user/.claude/projects";
    let mut files = (0..130)
        .map(|index| SourceImportFile {
            provider: CaptureProvider::Claude,
            source_format: "claude_projects_jsonl_tree".into(),
            source_root: root.into(),
            source_path: format!("{root}/session-{index:03}.jsonl"),
            file_size_bytes: index,
            file_modified_at_ms: index as i64,
            observed_at_ms: 10,
            metadata: serde_json::json!({}),
        })
        .collect::<Vec<_>>();
    files.push(complete_inventory_control(
        CaptureProvider::Claude,
        "claude_projects_jsonl_tree",
        root,
        20,
    ));
    store.upsert_source_import_files(&files).unwrap();

    let mut after_source_path = None;
    let mut pages = 0;
    while let Some(next) = store
        .reconcile_source_import_missing_paths_page(
            CaptureProvider::Claude,
            root,
            20,
            after_source_path.as_deref(),
        )
        .unwrap()
    {
        pages += 1;
        after_source_path = Some(next);
    }

    assert_eq!(pages, 3);
    assert_eq!(store.source_import_file_counts().unwrap().stale, 0);
    assert!(store
        .list_pending_source_import_files(CaptureProvider::Claude, root)
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .reconcile_source_import_missing_paths_page(CaptureProvider::Claude, root, 20, None,)
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .source_import_file_stats_for_source(CaptureProvider::Claude, root)
            .unwrap(),
        (0, 0)
    );

    store
        .upsert_source_import_files(&[complete_inventory_control(
            CaptureProvider::Claude,
            "claude_projects_jsonl_tree",
            root,
            21,
        )])
        .unwrap();
    let mut after_source_path = None;
    let mut confirmation_pages = 0;
    while let Some(next) = store
        .reconcile_source_import_missing_paths_page(
            CaptureProvider::Claude,
            root,
            21,
            after_source_path.as_deref(),
        )
        .unwrap()
    {
        confirmation_pages += 1;
        after_source_path = Some(next);
    }
    assert_eq!(confirmation_pages, 3);
    assert_eq!(store.source_import_file_counts().unwrap().stale, 130);
}

#[test]
fn source_import_preference_reconciliation_pages_shadowed_rows() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let root = "/home/user/.gemini/antigravity-cli/brain";
    let mut files = (0..70)
        .flat_map(|index| {
            ["transcript.jsonl", "transcript_full.jsonl"].map(|name| SourceImportFile {
                provider: CaptureProvider::Antigravity,
                source_format: "antigravity_cli_transcript_jsonl_tree".into(),
                source_root: root.into(),
                source_path: format!("{root}/session-{index:03}/logs/{name}"),
                file_size_bytes: 1,
                file_modified_at_ms: 1,
                observed_at_ms: 10,
                metadata: serde_json::json!({
                    "inventory_preferred_path_v1": (name == "transcript.jsonl").then(|| {
                        format!("{root}/session-{index:03}/logs/transcript_full.jsonl")
                    }),
                }),
            })
        })
        .collect::<Vec<_>>();
    files.push(complete_inventory_control(
        CaptureProvider::Antigravity,
        "antigravity_cli_transcript_jsonl_tree",
        root,
        10,
    ));
    store.upsert_source_import_files(&files).unwrap();

    let mut after_source_path = None;
    let mut pages = 0;
    while let Some(next) = store
        .mark_source_import_shadowed_paths_stale_page(
            CaptureProvider::Antigravity,
            root,
            10,
            after_source_path.as_deref(),
        )
        .unwrap()
    {
        pages += 1;
        after_source_path = Some(next);
    }

    assert_eq!(pages, 2);
    assert_eq!(
        store
            .source_import_file_stats_for_source(CaptureProvider::Antigravity, root)
            .unwrap(),
        (70, 70)
    );
    assert_eq!(store.source_import_file_counts().unwrap().stale, 70);
}
