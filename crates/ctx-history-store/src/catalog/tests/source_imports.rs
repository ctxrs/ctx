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
fn catalog_observation_token_refreshes_legacy_row_once() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let observed_at_ms = timestamp_ms(fixed_time());
    let source_root = "/home/user/.codex/sessions";
    let source_path = "/home/user/.codex/sessions/legacy-token.jsonl";
    let legacy = catalog_session(source_path, source_path, observed_at_ms);
    upsert_catalog_inventory(&store, std::slice::from_ref(&legacy));
    store
        .upsert_session(&imported_session(source_path))
        .unwrap();
    store
        .mark_catalog_source_indexed(
            CaptureProvider::Codex,
            CatalogSourceIndexUpdate {
                source_root,
                source_path,
                file_size_bytes: legacy.file_size_bytes,
                file_modified_at_ms: legacy.file_modified_at_ms,
                import_revision: legacy.import_revision,
                inventory_generation: current_catalog_generation(
                    &store,
                    CaptureProvider::Codex,
                    source_root,
                ),
                file_sha256: None,
                event_count: Some(1),
                indexed_at_ms: observed_at_ms + 1,
            },
        )
        .unwrap();

    let mut observed = legacy.clone();
    observed.cataloged_at_ms += 2;
    observed.metadata["file_observation_token_v1"] = serde_json::json!("token-a");
    upsert_catalog_inventory(&store, std::slice::from_ref(&observed));
    let pending = store
        .list_catalog_import_work(
            CaptureProvider::Codex,
            source_root,
            ImportWorkClass::Fresh,
            10,
        )
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].reason, ImportPendingReason::FreshChanged);

    let generation = current_catalog_generation(&store, CaptureProvider::Codex, source_root);
    let update = CatalogSourceIndexUpdate {
        source_root,
        source_path,
        file_size_bytes: observed.file_size_bytes,
        file_modified_at_ms: observed.file_modified_at_ms,
        import_revision: observed.import_revision,
        inventory_generation: generation,
        file_sha256: None,
        event_count: Some(1),
        indexed_at_ms: observed_at_ms + 3,
    };
    assert_eq!(
        store
            .mark_catalog_source_indexed(CaptureProvider::Codex, update)
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .list_catalog_import_work(
                CaptureProvider::Codex,
                source_root,
                ImportWorkClass::Fresh,
                10,
            )
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .record_observed_catalog_source_import_result(
                CaptureProvider::Codex,
                update,
                &observed.metadata,
                CatalogIndexedStatus::Indexed,
                None,
            )
            .unwrap(),
        1
    );
    observed.cataloged_at_ms += 1;
    upsert_catalog_inventory(&store, &[observed]);
    assert!(store
        .list_catalog_import_work(
            CaptureProvider::Codex,
            source_root,
            ImportWorkClass::Fresh,
            10
        )
        .unwrap()
        .is_empty());
}

#[test]
fn source_import_inventory_pacing_precharges_exact_bounded_write_chunks() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let source_root = "/home/user/.claude/projects";
    let files = (0..65)
        .map(|index| {
            let source_path = format!("{source_root}/{index:03}.jsonl");
            let mut file = source_import_file(
                CaptureProvider::Claude,
                "claude_projects_jsonl_tree",
                source_root,
                &source_path,
                1_000 + index,
            );
            file.metadata = serde_json::json!({"inventory_index": index});
            file
        })
        .collect::<Vec<_>>();
    let current_paths = files
        .iter()
        .map(|file| file.source_path.clone())
        .collect::<Vec<_>>();
    let generation = store
        .allocate_source_import_inventory_generation(CaptureProvider::Claude, source_root)
        .unwrap();
    let row_bytes = files
        .iter()
        .map(|file| {
            let metadata_json = serde_json::to_string(&file.metadata).unwrap();
            [
                file.provider.as_str(),
                file.source_format.as_str(),
                file.source_root.as_str(),
                file.source_path.as_str(),
                metadata_json.as_str(),
            ]
            .into_iter()
            .fold(
                super::SOURCE_IMPORT_PERSIST_ROW_OVERHEAD_BYTES,
                |bytes, value| bytes.saturating_add(value.len() as u64),
            )
        })
        .collect::<Vec<_>>();
    let expected_upsert_charges = vec![
        row_bytes[..64].iter().sum::<u64>(),
        row_bytes[64..].iter().sum::<u64>(),
    ];
    let path_bytes = current_paths
        .iter()
        .map(|path| {
            super::SOURCE_IMPORT_PERSIST_ROW_OVERHEAD_BYTES.saturating_add(path.len() as u64)
        })
        .collect::<Vec<_>>();
    let expected_path_charges = vec![
        path_bytes[..64].iter().sum::<u64>(),
        path_bytes[64..].iter().sum::<u64>(),
    ];

    let mut upsert_charges = Vec::new();
    let mut rows_before_charge = Vec::new();
    assert_eq!(
        store
            .upsert_source_import_files_with_pacing(generation, &files, |bytes| {
                upsert_charges.push(bytes);
                rows_before_charge.push(
                    store
                        .conn
                        .query_row("SELECT COUNT(*) FROM source_import_files", [], |row| {
                            row.get::<_, usize>(0)
                        })
                        .unwrap(),
                );
            })
            .unwrap(),
        files.len()
    );
    assert_eq!(upsert_charges, expected_upsert_charges);
    assert_eq!(rows_before_charge, vec![0, 64]);

    let mut path_charges = Vec::new();
    let mut paths_before_charge = Vec::new();
    assert_eq!(
        store
            .mark_source_import_missing_paths_stale_with_pacing(
                CaptureProvider::Claude,
                source_root,
                &current_paths,
                2_000,
                generation,
                |bytes| {
                    path_charges.push(bytes);
                    paths_before_charge.push(
                        store
                            .conn
                            .query_row(
                                "SELECT COUNT(*) FROM temp_source_import_current_paths",
                                [],
                                |row| row.get::<_, usize>(0),
                            )
                            .unwrap(),
                    );
                },
            )
            .unwrap(),
        0
    );
    assert_eq!(path_charges, expected_path_charges);
    assert_eq!(paths_before_charge, vec![0, 64]);

    let mut repeated_upsert_charges = Vec::new();
    assert_eq!(
        store
            .upsert_source_import_files_with_pacing(generation, &files, |bytes| {
                repeated_upsert_charges.push(bytes);
            })
            .unwrap(),
        0
    );
    assert_eq!(repeated_upsert_charges, expected_upsert_charges);

    let mut repeated_path_charges = Vec::new();
    assert_eq!(
        store
            .mark_source_import_missing_paths_stale_with_pacing(
                CaptureProvider::Claude,
                source_root,
                &current_paths,
                3_000,
                generation,
                |bytes| repeated_path_charges.push(bytes),
            )
            .unwrap(),
        0
    );
    assert_eq!(repeated_path_charges, expected_path_charges);

    let mut stale_observations = Vec::new();
    assert_eq!(
        store
            .mark_source_import_missing_paths_stale_with_pacing(
                CaptureProvider::Claude,
                source_root,
                &current_paths[..64],
                4_000,
                generation,
                |bytes| {
                    let temp_paths = store
                        .conn
                        .query_row(
                            "SELECT COUNT(*) FROM temp_source_import_current_paths",
                            [],
                            |row| row.get::<_, usize>(0),
                        )
                        .unwrap();
                    let omitted_is_stale = store
                        .conn
                        .query_row(
                            "SELECT is_stale FROM source_import_files WHERE source_path = ?1",
                            params![&current_paths[64]],
                            |row| row.get::<_, bool>(0),
                        )
                        .unwrap();
                    stale_observations.push((bytes, temp_paths, omitted_is_stale));
                },
            )
            .unwrap(),
        1
    );
    assert_eq!(
        stale_observations,
        vec![
            (path_bytes[..64].iter().sum::<u64>(), 0, false),
            (row_bytes[64], 64, false),
        ]
    );

    store
        .conn
        .execute("UPDATE source_import_files SET is_stale = 0", [])
        .unwrap();
    let mut stale_batch_charges = Vec::new();
    let mut stale_rows_before_charge = Vec::new();
    assert_eq!(
        store
            .mark_source_import_missing_paths_stale_with_pacing(
                CaptureProvider::Claude,
                source_root,
                &[],
                5_000,
                generation,
                |bytes| {
                    stale_batch_charges.push(bytes);
                    stale_rows_before_charge.push(
                        store
                            .conn
                            .query_row(
                                "SELECT COUNT(*) FROM source_import_files WHERE is_stale != 0",
                                [],
                                |row| row.get::<_, usize>(0),
                            )
                            .unwrap(),
                    );
                },
            )
            .unwrap(),
        files.len()
    );
    assert_eq!(stale_batch_charges, expected_upsert_charges);
    assert_eq!(stale_rows_before_charge, vec![0, 64]);
    assert!(store
        .conn
        .query_row(
            "SELECT is_stale FROM source_import_files WHERE source_path = ?1",
            params![&current_paths[64]],
            |row| row.get::<_, bool>(0),
        )
        .unwrap());
}

#[test]
fn direct_batched_stale_marking_rolls_back_all_rows_on_a_late_error() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let source_root = "/home/user/.claude/projects";
    let files = (0..65)
        .map(|index| {
            let path = format!("{source_root}/{index:03}.jsonl");
            source_import_file(
                CaptureProvider::Claude,
                "claude_projects_jsonl_tree",
                source_root,
                &path,
                1_000 + index,
            )
        })
        .collect::<Vec<_>>();
    let source_generation = store
        .allocate_source_import_inventory_generation(CaptureProvider::Claude, source_root)
        .unwrap();
    store
        .upsert_source_import_files(source_generation, &files)
        .unwrap();
    store
        .conn
        .execute_batch(
            r#"
            CREATE TRIGGER fail_late_source_stale
            BEFORE UPDATE OF is_stale ON source_import_files
            WHEN NEW.is_stale = 1
             AND OLD.source_path = '/home/user/.claude/projects/064.jsonl'
            BEGIN
                SELECT RAISE(ABORT, 'late source stale failure');
            END;
            "#,
        )
        .unwrap();

    assert!(store
        .mark_source_import_missing_paths_stale(
            CaptureProvider::Claude,
            source_root,
            &[],
            2_000,
            source_generation,
        )
        .is_err());
    assert!(store.conn.is_autocommit());
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM source_import_files WHERE is_stale != 0",
                [],
                |row| row.get::<_, usize>(0),
            )
            .unwrap(),
        0
    );

    let catalog_root = "/home/user/.codex/sessions";
    let sessions = (0..65)
        .map(|index| {
            let path = format!("{catalog_root}/{index:03}.jsonl");
            catalog_session(&path, &format!("session-{index:03}"), 3_000 + index)
        })
        .collect::<Vec<_>>();
    let catalog_generation = store
        .allocate_catalog_inventory_generation(CaptureProvider::Codex, catalog_root)
        .unwrap();
    store
        .upsert_catalog_sessions(catalog_generation, &sessions)
        .unwrap();
    store
        .conn
        .execute_batch(
            r#"
            CREATE TRIGGER fail_late_catalog_stale
            BEFORE UPDATE OF is_stale ON catalog_sessions
            WHEN NEW.is_stale = 1
             AND OLD.source_path = '/home/user/.codex/sessions/064.jsonl'
            BEGIN
                SELECT RAISE(ABORT, 'late catalog stale failure');
            END;
            "#,
        )
        .unwrap();

    assert!(store
        .mark_catalog_source_missing_paths_stale(
            CaptureProvider::Codex,
            catalog_root,
            &[],
            4_000,
            catalog_generation,
        )
        .is_err());
    assert!(store.conn.is_autocommit());
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM catalog_sessions WHERE is_stale != 0",
                [],
                |row| row.get::<_, usize>(0),
            )
            .unwrap(),
        0
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
    let mut material_source = imported_source(new_id(), &file.source_root, "claude-session");
    material_source.descriptor.provider = CaptureProvider::Claude;
    material_source.descriptor.raw_source_path = Some(file.source_path.clone());
    material_source.descriptor.source_format = Some(file.source_format.clone());
    store.upsert_capture_source(&material_source).unwrap();
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
fn manifested_inventory_formats_map_to_their_canonical_material_formats() {
    for (index, mapping) in PROVIDER_MATERIAL_SOURCE_FORMATS.iter().enumerate() {
        let provider = mapping.provider;
        let inventory = mapping.inventory_source_format;
        let material = mapping.material_source_format;
        assert_eq!(
            expected_material_source_format(provider, inventory),
            material
        );
        let temp = tempdir();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let root = format!("/fixture/material-format/{index}");
        let path = format!("{root}/session.jsonl");
        let observed_at_ms = timestamp_ms(fixed_time());
        let mut file = source_import_file(provider, inventory, &root, &path, observed_at_ms);
        file.metadata = serde_json::json!({"inventory_unit": "logical_import_unit"});
        upsert_source_inventory(&store, std::slice::from_ref(&file));
        store
            .mark_source_import_file_indexed(
                provider,
                SourceImportFileIndexUpdate {
                    source_root: &root,
                    source_path: &path,
                    file_size_bytes: file.file_size_bytes,
                    file_modified_at_ms: file.file_modified_at_ms,
                    import_revision: file.import_revision,
                    inventory_generation: current_source_generation(&store, provider, &root),
                    metadata: &file.metadata,
                    indexed_at_ms: observed_at_ms + 1,
                },
            )
            .unwrap();
        let mut source = imported_source(new_id(), &path, "material-format-session");
        source.descriptor.provider = provider;
        source.descriptor.raw_source_path = Some(path.clone());
        source.descriptor.source_format = Some(material.into());
        store.upsert_capture_source(&source).unwrap();
        assert!(store.source_import_material_exists(&file).unwrap());

        file.observed_at_ms += 1;
        upsert_source_inventory(&store, std::slice::from_ref(&file));
        assert!(
            store
                .list_pending_source_import_files(provider, &root)
                .unwrap()
                .is_empty(),
            "SQL material mapping disagreed for {}:{inventory}",
            provider.as_str()
        );
    }
    assert_eq!(
        expected_material_source_format(CaptureProvider::Trae, "trae_state_vscdb"),
        "trae_state_vscdb"
    );
}

#[test]
fn file_owned_source_import_material_does_not_match_a_sibling_capture_source() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let observed_at_ms = timestamp_ms(fixed_time());
    let root = "/home/user/.claude/projects";
    let mut file = source_import_file(
        CaptureProvider::Claude,
        "claude_projects_jsonl_tree",
        root,
        "/home/user/.claude/projects/owned.jsonl",
        observed_at_ms,
    );
    file.metadata = serde_json::json!({"inventory_unit": "logical_import_unit"});
    upsert_source_inventory(&store, std::slice::from_ref(&file));
    store
        .mark_source_import_file_indexed(
            file.provider,
            SourceImportFileIndexUpdate {
                source_root: root,
                source_path: &file.source_path,
                file_size_bytes: file.file_size_bytes,
                file_modified_at_ms: file.file_modified_at_ms,
                import_revision: file.import_revision,
                inventory_generation: current_source_generation(&store, file.provider, root),
                metadata: &file.metadata,
                indexed_at_ms: observed_at_ms + 1,
            },
        )
        .unwrap();
    let mut sibling = imported_source(new_id(), root, "sibling-session");
    sibling.descriptor.provider = file.provider;
    sibling.descriptor.raw_source_path = Some(format!("{root}/sibling.jsonl"));
    sibling.descriptor.source_format = Some(file.source_format.clone());
    store.upsert_capture_source(&sibling).unwrap();

    file.observed_at_ms += 1;
    upsert_source_inventory(&store, std::slice::from_ref(&file));
    let recovery = store
        .list_source_import_file_work(file.provider, root, ImportWorkClass::Recovery, 10)
        .unwrap();
    assert_eq!(recovery.len(), 1);
    assert_eq!(recovery[0].reason, ImportPendingReason::MissingMaterial);
}

#[test]
fn file_owned_source_import_material_accepts_an_exact_self_rooted_capture_source() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let observed_at_ms = timestamp_ms(fixed_time());
    let root = "/home/user/.mistral-vibe/logs";
    let mut file = source_import_file(
        CaptureProvider::MistralVibe,
        "mistral_vibe_session_jsonl_tree",
        root,
        "/home/user/.mistral-vibe/logs/session/messages.jsonl",
        observed_at_ms,
    );
    file.metadata = serde_json::json!({"inventory_unit": "logical_import_unit"});
    upsert_source_inventory(&store, std::slice::from_ref(&file));
    store
        .mark_source_import_file_indexed(
            file.provider,
            SourceImportFileIndexUpdate {
                source_root: root,
                source_path: &file.source_path,
                file_size_bytes: file.file_size_bytes,
                file_modified_at_ms: file.file_modified_at_ms,
                import_revision: file.import_revision,
                inventory_generation: current_source_generation(&store, file.provider, root),
                metadata: &file.metadata,
                indexed_at_ms: observed_at_ms + 1,
            },
        )
        .unwrap();
    let mut source = imported_source(new_id(), &file.source_path, "mistral-session");
    source.descriptor.provider = file.provider;
    source.descriptor.raw_source_path = Some(file.source_path.clone());
    source.descriptor.source_format = Some("mistral_vibe_session_jsonl".into());
    store.upsert_capture_source(&source).unwrap();
    let material: (String, String, Option<String>, Option<String>) = store
        .conn
        .query_row(
            "SELECT provider, source_format, source_root, raw_source_path FROM capture_sources WHERE id = ?1",
            params![source.id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        material,
        (
            file.provider.as_str().to_owned(),
            "mistral_vibe_session_jsonl".to_owned(),
            Some(file.source_path.clone()),
            Some(file.source_path.clone()),
        )
    );
    assert!(store.source_import_material_exists(&file).unwrap());

    file.observed_at_ms += 1;
    upsert_source_inventory(&store, std::slice::from_ref(&file));
    assert!(store
        .list_pending_source_import_files(file.provider, root)
        .unwrap()
        .is_empty());
}

#[test]
fn source_import_material_requires_expected_format_and_exact_root() {
    for scenario in ["wrong_format", "wrong_root"] {
        let temp = tempdir();
        let store = Store::open(temp.path().join(format!("{scenario}.sqlite"))).unwrap();
        let observed_at_ms = timestamp_ms(fixed_time());
        let root = "/fixture/pi";
        let mut file = source_import_file(
            CaptureProvider::Pi,
            "pi_session_jsonl",
            root,
            "/fixture/pi/session.jsonl",
            observed_at_ms,
        );
        file.metadata = serde_json::json!({"inventory_unit": "logical_import_unit"});
        upsert_source_inventory(&store, std::slice::from_ref(&file));
        store
            .mark_source_import_file_indexed(
                file.provider,
                SourceImportFileIndexUpdate {
                    source_root: root,
                    source_path: &file.source_path,
                    file_size_bytes: file.file_size_bytes,
                    file_modified_at_ms: file.file_modified_at_ms,
                    import_revision: file.import_revision,
                    inventory_generation: current_source_generation(&store, file.provider, root),
                    metadata: &file.metadata,
                    indexed_at_ms: observed_at_ms + 1,
                },
            )
            .unwrap();
        let mut source = imported_source(new_id(), root, "pi-session");
        source.descriptor.provider = file.provider;
        source.descriptor.raw_source_path = Some(file.source_path.clone());
        source.descriptor.source_format = Some(file.source_format.clone());
        if scenario == "wrong_format" {
            source.descriptor.source_format = Some("pi_session_json".into());
        } else {
            source.descriptor.source_root = Some("/fixture/pi-other".into());
        }
        store.upsert_capture_source(&source).unwrap();

        file.observed_at_ms += 1;
        upsert_source_inventory(&store, std::slice::from_ref(&file));
        let recovery = store
            .list_source_import_file_work(file.provider, root, ImportWorkClass::Recovery, 10)
            .unwrap();
        assert_eq!(
            recovery[0].reason,
            ImportPendingReason::MissingMaterial,
            "{scenario}"
        );
        let counts = store.source_import_file_counts().unwrap();
        assert_eq!(counts.indexed, 0, "{scenario}");
        assert_eq!(counts.pending, 1, "{scenario}");
    }
}

#[test]
fn source_root_import_material_accepts_a_capture_source_for_the_same_root() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let observed_at_ms = timestamp_ms(fixed_time());
    let root = "/home/user/.hermes";
    let mut file = source_import_file(
        CaptureProvider::Hermes,
        "hermes_state_sqlite",
        root,
        "/home/user/.hermes/state.db",
        observed_at_ms,
    );
    file.metadata = serde_json::json!({"inventory_unit": "source_root"});
    upsert_source_inventory(&store, std::slice::from_ref(&file));
    store
        .mark_source_import_file_indexed(
            file.provider,
            SourceImportFileIndexUpdate {
                source_root: root,
                source_path: &file.source_path,
                file_size_bytes: file.file_size_bytes,
                file_modified_at_ms: file.file_modified_at_ms,
                import_revision: file.import_revision,
                inventory_generation: current_source_generation(&store, file.provider, root),
                metadata: &file.metadata,
                indexed_at_ms: observed_at_ms + 1,
            },
        )
        .unwrap();
    let mut root_source = imported_source(new_id(), root, "root-session");
    root_source.descriptor.provider = file.provider;
    root_source.descriptor.raw_source_path = Some(format!("{root}/sibling.db"));
    root_source.descriptor.source_format = Some(file.source_format.clone());
    store.upsert_capture_source(&root_source).unwrap();

    file.observed_at_ms += 1;
    upsert_source_inventory(&store, std::slice::from_ref(&file));
    assert!(store
        .list_pending_source_import_files(file.provider, root)
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
    upsert_source_material(&store, &file);
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
fn logical_import_unit_change_token_marks_same_owner_stat_pending() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let observed_at_ms = timestamp_ms(fixed_time());
    let root = "/home/user/.local/share/opencode/opencode.db";
    let mut file = SourceImportFile {
        provider: CaptureProvider::OpenCode,
        source_format: "opencode_sqlite".into(),
        source_root: root.into(),
        source_path: root.into(),
        file_size_bytes: 42,
        file_modified_at_ms: observed_at_ms,
        import_revision: 1,
        observed_at_ms,
        metadata: serde_json::json!({
            "inventory_unit": "logical_import_unit",
            "change_token_v1": "before",
            "dependencies": ["opencode.db-wal"],
        }),
    };
    upsert_source_inventory(&store, std::slice::from_ref(&file));
    store
        .mark_source_import_file_indexed(
            CaptureProvider::OpenCode,
            SourceImportFileIndexUpdate {
                source_root: root,
                source_path: root,
                file_size_bytes: file.file_size_bytes,
                file_modified_at_ms: file.file_modified_at_ms,
                import_revision: file.import_revision,
                inventory_generation: current_source_generation(
                    &store,
                    CaptureProvider::OpenCode,
                    root,
                ),
                metadata: &file.metadata,
                indexed_at_ms: observed_at_ms + 1,
            },
        )
        .unwrap();
    upsert_source_material(&store, &file);
    assert!(store
        .list_pending_source_import_files(CaptureProvider::OpenCode, root)
        .unwrap()
        .is_empty());

    file.metadata["change_token_v1"] = serde_json::json!("after");
    file.observed_at_ms += 1;
    upsert_source_inventory(&store, std::slice::from_ref(&file));

    assert_eq!(
        store
            .list_pending_source_import_files(CaptureProvider::OpenCode, root)
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
    upsert_source_material(&store, &files[0]);
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
fn catalog_inventory_generation_currentness_tracks_allocation_order() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let source_root = "/tmp/codex-generation-currentness";
    let first = store
        .allocate_catalog_inventory_generation(CaptureProvider::Codex, source_root)
        .unwrap();
    assert!(store
        .catalog_inventory_generation_is_current(CaptureProvider::Codex, source_root, first,)
        .unwrap());
    assert!(!store
        .catalog_inventory_generation_is_complete(CaptureProvider::Codex, source_root, first)
        .unwrap());
    assert!(store
        .complete_catalog_inventory_generation(CaptureProvider::Codex, source_root, first)
        .unwrap());
    assert!(store
        .catalog_inventory_generation_is_complete_without_pending(
            CaptureProvider::Codex,
            source_root,
            first,
        )
        .unwrap());

    let second = store
        .allocate_catalog_inventory_generation(CaptureProvider::Codex, source_root)
        .unwrap();
    assert!(!store
        .catalog_inventory_generation_is_current(CaptureProvider::Codex, source_root, first,)
        .unwrap());
    assert!(store
        .catalog_inventory_generation_is_current(CaptureProvider::Codex, source_root, second,)
        .unwrap());
    assert!(!store
        .catalog_inventory_generation_is_complete_without_pending(
            CaptureProvider::Codex,
            source_root,
            second,
        )
        .unwrap());
    assert!(!store
        .complete_catalog_inventory_generation(CaptureProvider::Codex, source_root, first)
        .unwrap());
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
