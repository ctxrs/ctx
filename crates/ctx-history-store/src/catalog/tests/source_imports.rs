#[test]
fn catalog_upsert_invalidates_checkpoint_for_shrink_and_same_size_change() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = timestamp_ms(fixed_time());
    for (source_path, file_size_bytes) in [
        ("/home/user/.codex/sessions/2026/06/24/shrink.jsonl", 41_u64),
        (
            "/home/user/.codex/sessions/2026/06/24/same-size.jsonl",
            42_u64,
        ),
    ] {
        upsert_catalog_inventory(
            &store,
            &[catalog_session(source_path, source_path, cataloged_at_ms)],
        );
        store
            .upsert_session(&imported_session(source_path))
            .unwrap();
        store
            .mark_catalog_source_indexed(
                CaptureProvider::Codex,
                CatalogSourceIndexUpdate {
                    source_root: "/home/user/.codex/sessions",
                    source_path,
                    file_size_bytes: 42,
                    file_modified_at_ms: cataloged_at_ms,
                    import_revision: 1,
                    inventory_generation: current_catalog_generation(
                        &store,
                        CaptureProvider::Codex,
                        "/home/user/.codex/sessions",
                    ),
                    file_sha256: None,
                    event_count: Some(3),
                    indexed_at_ms: cataloged_at_ms + 10,
                },
            )
            .unwrap();

        let mut changed = catalog_session(source_path, source_path, cataloged_at_ms + 1);
        changed.file_size_bytes = file_size_bytes;
        upsert_catalog_inventory(&store, &[changed]);

        let (status, indexed_size, checkpoint_size): (String, Option<i64>, Option<i64>) =
            store
                .conn
                .query_row(
                    "SELECT indexed_status, indexed_file_size_bytes, last_imported_file_size_bytes FROM catalog_sessions WHERE source_path = ?1",
                    [source_path],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
        assert_eq!(status, CatalogIndexedStatus::Pending.as_str());
        assert_eq!(indexed_size, None);
        assert_eq!(checkpoint_size, None);
    }
}

#[test]
fn catalog_index_checkpoint_event_count_can_be_unknown() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = timestamp_ms(fixed_time());
    let source_path = "/home/user/.codex/sessions/2026/06/24/unknown-count.jsonl";
    upsert_catalog_inventory(
        &store,
        &[catalog_session(
            source_path,
            "codex-session-unknown-count",
            cataloged_at_ms,
        )],
    );
    store
        .mark_catalog_source_indexed(
            CaptureProvider::Codex,
            CatalogSourceIndexUpdate {
                source_root: "/home/user/.codex/sessions",
                source_path,
                file_size_bytes: 42,
                file_modified_at_ms: cataloged_at_ms,
                import_revision: 1,
                inventory_generation: current_catalog_generation(
                    &store,
                    CaptureProvider::Codex,
                    "/home/user/.codex/sessions",
                ),
                file_sha256: Some("abc123"),
                event_count: None,
                indexed_at_ms: cataloged_at_ms + 10,
            },
        )
        .unwrap();

    let checkpoint = store
        .catalog_source_index_state(
            CaptureProvider::Codex,
            "/home/user/.codex/sessions",
            source_path,
        )
        .unwrap()
        .unwrap();
    assert_eq!(checkpoint.last_imported_event_count, None);
    assert_eq!(
        checkpoint.last_imported_file_sha256.as_deref(),
        Some("abc123")
    );
}

#[test]
fn source_import_manifest_upsert_ignores_observed_at_for_unchanged_files() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let observed_at_ms = timestamp_ms(fixed_time());
    let mut file = SourceImportFile {
        provider: CaptureProvider::Claude,
        source_format: "claude_projects_jsonl_tree".into(),
        source_root: "/home/user/.claude/projects".into(),
        source_path: "/home/user/.claude/projects/session.jsonl".into(),
        file_size_bytes: 42,
        file_modified_at_ms: observed_at_ms,
        import_revision: 1,
        observed_at_ms,
        metadata: serde_json::json!({}),
    };
    let inventory_generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(inventory_generation, std::slice::from_ref(&file))
        .unwrap();
    store
        .mark_source_import_file_indexed(
            CaptureProvider::Claude,
            SourceImportFileIndexUpdate {
                source_root: "/home/user/.claude/projects",
                source_path: "/home/user/.claude/projects/session.jsonl",
                file_size_bytes: 42,
                file_modified_at_ms: observed_at_ms,
                import_revision: 1,
                inventory_generation,
                metadata: &file.metadata,
                indexed_at_ms: observed_at_ms + 10,
            },
        )
        .unwrap();
    let after_indexed: i64 = store
        .conn
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap();

    file.observed_at_ms += 1_000;
    store
        .upsert_source_import_files(inventory_generation, std::slice::from_ref(&file))
        .unwrap();
    let after_noop: i64 = store
        .conn
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap();
    assert_eq!(after_noop, after_indexed);
    assert!(store
        .list_pending_source_import_files(CaptureProvider::Claude, "/home/user/.claude/projects")
        .unwrap()
        .is_empty());
}

#[test]
fn source_root_inventory_change_token_marks_same_stat_source_pending() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let observed_at_ms = timestamp_ms(fixed_time());
    let root = "/home/user/.hermes/state.db";
    let mut file = SourceImportFile {
        provider: CaptureProvider::Hermes,
        source_format: "hermes_state_sqlite".into(),
        source_root: root.into(),
        source_path: root.into(),
        file_size_bytes: 42,
        file_modified_at_ms: observed_at_ms,
        import_revision: 1,
        observed_at_ms,
        metadata: serde_json::json!({
            "inventory_unit": "source_root",
            "source_files": 1,
            "change_token_v1": "before",
        }),
    };
    upsert_source_inventory(&store, std::slice::from_ref(&file));
    store
        .mark_source_import_file_indexed(
            CaptureProvider::Hermes,
            SourceImportFileIndexUpdate {
                source_root: root,
                source_path: root,
                file_size_bytes: file.file_size_bytes,
                file_modified_at_ms: file.file_modified_at_ms,
                import_revision: file.import_revision,
                inventory_generation: current_source_generation(
                    &store,
                    CaptureProvider::Hermes,
                    root,
                ),
                metadata: &file.metadata,
                indexed_at_ms: observed_at_ms + 1,
            },
        )
        .unwrap();
    assert!(store
        .list_pending_source_import_files(CaptureProvider::Hermes, root)
        .unwrap()
        .is_empty());

    file.metadata["change_token_v1"] = serde_json::json!("after");
    file.observed_at_ms += 1;
    upsert_source_inventory(&store, std::slice::from_ref(&file));

    assert_eq!(
        store
            .list_pending_source_import_files(CaptureProvider::Hermes, root)
            .unwrap(),
        vec![file]
    );
}

#[test]
fn source_import_format_change_marks_same_stat_source_pending() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let observed_at_ms = timestamp_ms(fixed_time());
    let root = "/home/user/agent/state.db";
    let mut file = SourceImportFile {
        provider: CaptureProvider::Custom,
        source_format: "old_format".into(),
        source_root: root.into(),
        source_path: root.into(),
        file_size_bytes: 42,
        file_modified_at_ms: observed_at_ms,
        import_revision: 1,
        observed_at_ms,
        metadata: serde_json::json!({}),
    };
    upsert_source_inventory(&store, std::slice::from_ref(&file));
    store
        .mark_source_import_file_indexed(
            CaptureProvider::Custom,
            SourceImportFileIndexUpdate {
                source_root: root,
                source_path: root,
                file_size_bytes: file.file_size_bytes,
                file_modified_at_ms: file.file_modified_at_ms,
                import_revision: file.import_revision,
                inventory_generation: current_source_generation(
                    &store,
                    CaptureProvider::Custom,
                    root,
                ),
                metadata: &file.metadata,
                indexed_at_ms: observed_at_ms + 1,
            },
        )
        .unwrap();

    file.source_format = "new_format".into();
    file.observed_at_ms += 1;
    upsert_source_inventory(&store, std::slice::from_ref(&file));

    assert_eq!(
        store
            .list_pending_source_import_files(CaptureProvider::Custom, root)
            .unwrap(),
        vec![file]
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
            import_revision: 1,
            observed_at_ms,
            metadata: serde_json::json!({}),
        })
        .collect::<Vec<_>>();

    upsert_source_inventory(&store, &files);
    store
        .mark_source_import_file_indexed(
            CaptureProvider::Claude,
            SourceImportFileIndexUpdate {
                source_root: root,
                source_path: &files[0].source_path,
                file_size_bytes: 42,
                file_modified_at_ms: observed_at_ms,
                import_revision: 1,
                inventory_generation: current_source_generation(
                    &store,
                    CaptureProvider::Claude,
                    root,
                ),
                metadata: &files[0].metadata,
                indexed_at_ms: observed_at_ms + 10,
            },
        )
        .unwrap();
    store
        .record_source_import_file_result(
            CaptureProvider::Claude,
            SourceImportFileIndexUpdate {
                source_root: root,
                source_path: &files[2].source_path,
                file_size_bytes: files[2].file_size_bytes,
                file_modified_at_ms: files[2].file_modified_at_ms,
                import_revision: files[2].import_revision,
                inventory_generation: current_source_generation(
                    &store,
                    CaptureProvider::Claude,
                    root,
                ),
                metadata: &files[2].metadata,
                indexed_at_ms: observed_at_ms + 20,
            },
            CatalogIndexedStatus::Failed,
            Some("bad json"),
        )
        .unwrap();
    store
        .mark_source_import_missing_paths_stale(
            CaptureProvider::Claude,
            root,
            &[files[0].source_path.clone(), files[2].source_path.clone()],
            observed_at_ms + 30,
            current_source_generation(&store, CaptureProvider::Claude, root),
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
    upsert_source_inventory(&store, &[changed_indexed]);

    let counts = store.source_import_file_counts().unwrap();
    assert_eq!(counts.total, 2);
    assert_eq!(counts.indexed, 0);
    assert_eq!(counts.pending, 2);
    assert_eq!(counts.failed, 1);
    assert_eq!(counts.stale, 1);
}

#[test]
fn reversed_catalog_generations_fence_stale_upsert_completion_and_finalization() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let older = Store::open(&db_path).unwrap();
    let newer = Store::open(&db_path).unwrap();
    let source_root = "/home/user/.codex/sessions";
    let source_path = "/home/user/.codex/sessions/reversed.jsonl";
    let observed_at_ms = timestamp_ms(fixed_time());
    let older_generation = older
        .allocate_catalog_inventory_generation(CaptureProvider::Codex, source_root)
        .unwrap();
    let mut older_session = catalog_session(source_path, "reversed", observed_at_ms);
    older_session.metadata = serde_json::json!({"inventory": "older"});

    let newer_generation = newer
        .allocate_catalog_inventory_generation(CaptureProvider::Codex, source_root)
        .unwrap();
    let mut newer_session = older_session.clone();
    newer_session.cataloged_at_ms += 1;
    newer_session.metadata = serde_json::json!({"inventory": "newer"});
    assert_eq!(
        newer
            .upsert_catalog_sessions(newer_generation, &[newer_session.clone()])
            .unwrap(),
        1
    );

    assert_eq!(
        older
            .upsert_catalog_sessions(older_generation, &[older_session.clone()])
            .unwrap(),
        0
    );
    assert_eq!(
        older
            .record_catalog_source_import_result(
                CaptureProvider::Codex,
                CatalogSourceIndexUpdate {
                    source_root,
                    source_path,
                    file_size_bytes: older_session.file_size_bytes,
                    file_modified_at_ms: older_session.file_modified_at_ms,
                    import_revision: older_session.import_revision,
                    inventory_generation: older_generation,
                    file_sha256: None,
                    event_count: Some(1),
                    indexed_at_ms: observed_at_ms + 2,
                },
                CatalogIndexedStatus::Rejected,
                Some("late older result"),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        older
            .mark_catalog_source_missing_paths_stale(
                CaptureProvider::Codex,
                source_root,
                &[],
                observed_at_ms + 3,
                older_generation,
            )
            .unwrap(),
        0
    );

    let stored = newer
        .list_catalog_sessions_for_source(CaptureProvider::Codex, source_root)
        .unwrap();
    assert_eq!(stored, vec![newer_session]);
    assert_eq!(newer.catalog_session_counts().unwrap().stale, 0);
}

#[test]
fn catalog_missing_path_staling_is_idempotent_across_inventory_generations() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let source_root = "/home/user/.codex/sessions";
    let source_path = "/home/user/.codex/sessions/deleted.jsonl";
    let observed_at_ms = timestamp_ms(fixed_time());
    let session = catalog_session(source_path, "deleted", observed_at_ms);
    let first_generation = store
        .allocate_catalog_inventory_generation(CaptureProvider::Codex, source_root)
        .unwrap();
    store
        .upsert_catalog_sessions(first_generation, &[session])
        .unwrap();

    assert_eq!(
        store
            .mark_catalog_source_missing_paths_stale(
                CaptureProvider::Codex,
                source_root,
                &[],
                observed_at_ms + 1,
                first_generation,
            )
            .unwrap(),
        1
    );

    let second_generation = store
        .allocate_catalog_inventory_generation(CaptureProvider::Codex, source_root)
        .unwrap();
    assert_eq!(
        store
            .mark_catalog_source_missing_paths_stale(
                CaptureProvider::Codex,
                source_root,
                &[],
                observed_at_ms + 2,
                second_generation,
            )
            .unwrap(),
        0
    );
    let cataloged_at_ms: i64 = store
        .conn
        .query_row(
            "SELECT cataloged_at_ms FROM catalog_sessions WHERE source_path = ?1",
            [source_path],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cataloged_at_ms, observed_at_ms + 1);
}

#[test]
fn reversed_source_generations_and_metadata_fence_stale_results() {
    let temp = tempdir();
    let db_path = temp.path().join("work.sqlite");
    let older = Store::open(&db_path).unwrap();
    let newer = Store::open(&db_path).unwrap();
    let source_root = "/home/user/.hermes/state.db";
    let source_path = source_root;
    let observed_at_ms = timestamp_ms(fixed_time());
    let older_generation = older
        .allocate_source_import_inventory_generation(CaptureProvider::Hermes, source_root)
        .unwrap();
    let mut older_file = source_import_file(
        CaptureProvider::Hermes,
        "hermes_state_sqlite",
        source_root,
        source_path,
        observed_at_ms,
    );
    older_file.metadata = serde_json::json!({
        "inventory_unit": "source_root",
        "change_token_v1": "older",
    });

    let newer_generation = newer
        .allocate_source_import_inventory_generation(CaptureProvider::Hermes, source_root)
        .unwrap();
    let mut newer_file = older_file.clone();
    newer_file.observed_at_ms += 1;
    newer_file.metadata["change_token_v1"] = serde_json::json!("newer");
    assert_eq!(
        newer
            .upsert_source_import_files(newer_generation, &[newer_file.clone()])
            .unwrap(),
        1
    );
    assert_eq!(
        older
            .upsert_source_import_files(older_generation, &[older_file.clone()])
            .unwrap(),
        0
    );

    let stale_update = SourceImportFileIndexUpdate {
        source_root,
        source_path,
        file_size_bytes: older_file.file_size_bytes,
        file_modified_at_ms: older_file.file_modified_at_ms,
        import_revision: older_file.import_revision,
        inventory_generation: older_generation,
        metadata: &older_file.metadata,
        indexed_at_ms: observed_at_ms + 2,
    };
    assert_eq!(
        older
            .record_source_import_file_result(
                CaptureProvider::Hermes,
                stale_update,
                CatalogIndexedStatus::Rejected,
                Some("late older result"),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        newer
            .record_source_import_file_result(
                CaptureProvider::Hermes,
                SourceImportFileIndexUpdate {
                    inventory_generation: newer_generation,
                    ..stale_update
                },
                CatalogIndexedStatus::Rejected,
                Some("wrong metadata"),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        older
            .mark_source_import_missing_paths_stale(
                CaptureProvider::Hermes,
                source_root,
                &[],
                observed_at_ms + 3,
                older_generation,
            )
            .unwrap(),
        0
    );

    assert_eq!(
        newer
            .record_source_import_file_result(
                CaptureProvider::Hermes,
                SourceImportFileIndexUpdate {
                    source_root,
                    source_path,
                    file_size_bytes: newer_file.file_size_bytes,
                    file_modified_at_ms: newer_file.file_modified_at_ms,
                    import_revision: newer_file.import_revision,
                    inventory_generation: newer_generation,
                    metadata: &newer_file.metadata,
                    indexed_at_ms: observed_at_ms + 4,
                },
                CatalogIndexedStatus::Rejected,
                Some("current deterministic rejection"),
            )
            .unwrap(),
        1
    );
    let stored_metadata: String = newer
        .conn
        .query_row(
            "SELECT metadata_json FROM source_import_files WHERE provider = ?1 AND source_root = ?2 AND source_path = ?3",
            params![CaptureProvider::Hermes.as_str(), source_root, source_path],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stored_metadata).unwrap(),
        newer_file.metadata
    );
    assert_eq!(newer.source_import_file_counts().unwrap().stale, 0);
}

#[test]
fn completed_with_rejections_missing_session_is_pending_for_repair() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let source_root = "/home/user/.codex/sessions";
    let source_path = "/home/user/.codex/sessions/missing-mixed.jsonl";
    let observed_at_ms = timestamp_ms(fixed_time());
    let session = catalog_session(source_path, "missing-mixed", observed_at_ms);
    upsert_catalog_inventory(&store, &[session.clone()]);
    store
        .record_catalog_source_import_result(
            CaptureProvider::Codex,
            CatalogSourceIndexUpdate {
                source_root,
                source_path,
                file_size_bytes: session.file_size_bytes,
                file_modified_at_ms: session.file_modified_at_ms,
                import_revision: session.import_revision,
                inventory_generation: current_catalog_generation(
                    &store,
                    CaptureProvider::Codex,
                    source_root,
                ),
                file_sha256: None,
                event_count: Some(1),
                indexed_at_ms: observed_at_ms + 1,
            },
            CatalogIndexedStatus::CompletedWithRejections,
            Some("one malformed record"),
        )
        .unwrap();

    assert_eq!(
        store
            .list_pending_catalog_sessions(CaptureProvider::Codex, source_root)
            .unwrap(),
        vec![session]
    );
    let counts = store.catalog_session_counts().unwrap();
    assert_eq!(counts.pending, 1);
    assert_eq!(counts.indexed, 0);
    assert_eq!(counts.completed_with_rejections, 1);
}
