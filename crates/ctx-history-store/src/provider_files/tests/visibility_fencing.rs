#[test]
fn pending_owner_is_hidden_from_views_external_hydration_and_export() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let store = Store::open(&path).unwrap();
    let file = source_file(20, 100);
    let generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    let source = Uuid::from_u128(70);
    let session = Uuid::from_u128(71);
    let event = Uuid::from_u128(72);
    let touched = Uuid::from_u128(73);
    insert_capture_source(&store, source, PATH_A, "visibility-session");
    insert_raw_session(&store, session, source, "visibility-session");
    store
        .conn
        .execute(
            r#"
            INSERT INTO events
                (id, seq, session_id, event_type, role, occurred_at_ms,
                 capture_source_id, payload_json)
            VALUES (?1, 1, ?2, 'message', 'user', 1, ?3, '{"text":"hidden"}')
            "#,
            params![event.to_string(), session.to_string(), source.to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO files_touched (id, event_id, path, created_at_ms, updated_at_ms, source_id) VALUES (?1, ?2, 'hidden.rs', 1, 1, ?3)",
            params![touched.to_string(), event.to_string(), source.to_string()],
        )
        .unwrap();
    store.rebuild_search_projection().unwrap();

    let outcome = source_outcome(&file, generation, 120);
    let scope = store
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            110,
        )
        .unwrap();
    let observer = Store::open(&path).unwrap();
    for view in ["ctx_sessions", "ctx_events", "ctx_files_touched"] {
        let count: i64 = observer
            .conn
            .query_row(&format!("SELECT COUNT(*) FROM {view}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "{view} exposed pending owner material");
    }
    assert!(observer
        .session_by_external_session(CaptureProvider::Claude, "visibility-session")
        .unwrap()
        .is_none());
    assert!(observer
        .session_by_capture_source_and_external_session(
            source,
            CaptureProvider::Claude,
            "visibility-session",
        )
        .unwrap()
        .is_none());
    assert!(observer
        .capture_source_by_external_session(CaptureProvider::Claude, "visibility-session")
        .unwrap()
        .is_none());
    assert!(!observer.file_touched_exists(touched).unwrap());
    let archive = observer.export_archive().unwrap();
    assert!(archive.capture_sources.is_empty());
    assert!(archive.sessions.is_empty());
    assert!(archive.events.is_empty());
    assert!(archive.files_touched.is_empty());

    store
        .track_provider_file_publication_session(session)
        .unwrap();
    store.track_provider_file_publication_event(event).unwrap();
    store
        .track_provider_file_publication_file_touched(touched)
        .unwrap();
    reconcile_all(&store, &scope, 10);
    store
        .finalize_provider_file_publication(
            scope,
            outcome,
            ProviderFilePublicationCommit::Replacement(None),
        )
        .unwrap();
    assert_eq!(observer.list_sessions().unwrap().len(), 1);
    assert_eq!(observer.list_events().unwrap().len(), 1);
    assert!(observer.file_touched_exists(touched).unwrap());
}
#[test]
fn durable_marker_blocks_cross_connection_entity_and_archive_contamination() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let store = Store::open(&path).unwrap();
    let file = source_file(20, 100);
    let generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    let owner_source = Uuid::from_u128(74_000);
    let owner_session = Uuid::from_u128(74_001);
    let owner_event = Uuid::from_u128(74_002);
    let owner_artifact = Uuid::from_u128(74_004);
    insert_capture_source(&store, owner_source, PATH_A, "mutation-owner");
    insert_raw_session(&store, owner_session, owner_source, "mutation-owner");
    insert_raw_event(&store, owner_event, 74_002, owner_source, "owned event");
    store
        .conn
        .execute(
            "UPDATE events SET session_id = ?1 WHERE id = ?2",
            params![owner_session.to_string(), owner_event.to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO artifacts (id, kind, blob_hash, blob_path, byte_size, created_at_ms, updated_at_ms, source_id) VALUES (?1, 'transcript', ?2, 'objects/mutation', 1, 1, 1, ?3)",
            params![owner_artifact.to_string(), "4".repeat(64), owner_source.to_string()],
        )
        .unwrap();
    let mut owner_record = ctx_history_core::HistoryRecord::new(
        "owner record",
        "must remain stable",
        Vec::new(),
        "note",
        None,
    );
    owner_record.id = Uuid::from_u128(74_003);
    store.insert_record(&owner_record).unwrap();
    store
        .assign_session_to_record(owner_session, owner_record.id)
        .unwrap();

    let sibling_source = Uuid::from_u128(74_010);
    let sibling_session = Uuid::from_u128(74_011);
    insert_capture_source(
        &store,
        sibling_source,
        "/history/claude/projects/sibling.jsonl",
        "mutation-sibling",
    );
    insert_raw_session(&store, sibling_session, sibling_source, "mutation-sibling");
    let mut sibling_update = store.get_session(sibling_session).unwrap();
    sibling_update.role_hint = Some("safe sibling update".to_owned());
    let mut cross_owner_session_update = sibling_update.clone();
    cross_owner_session_update.transcript_blob_id = Some(owner_artifact);
    let mut cross_owner_event = event_fixture(
        Uuid::from_u128(74_012),
        74_012,
        sibling_source,
        "cross-owner-event".to_owned(),
        "must be rejected",
    );
    cross_owner_event.dedupe_key = None;
    cross_owner_event.session_id = Some(owner_session);
    let archive = store.export_archive().unwrap();
    let archive_source = archive
        .capture_sources
        .iter()
        .find(|source| source.id == owner_source)
        .unwrap()
        .descriptor
        .clone();
    let owner_update = store.get_session(owner_session).unwrap();

    let outcome = source_outcome(&file, generation, 120);
    let scope = store
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            110,
        )
        .unwrap();
    let mut observer = Store::open(&path).unwrap();
    for error in [
        observer.upsert_session(&owner_update).unwrap_err(),
        observer
            .upsert_session(&cross_owner_session_update)
            .unwrap_err(),
        observer.upsert_event(&cross_owner_event).unwrap_err(),
        observer
            .assign_session_to_record(owner_session, owner_record.id)
            .unwrap_err(),
        observer.delete_orphan_record(owner_record.id).unwrap_err(),
        observer.import_archive(&archive, true).unwrap_err(),
        observer
            .import_archive_from_capture_source(
                &archive,
                owner_source,
                &archive_source,
                DateTime::parse_from_rfc3339("2026-07-14T12:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                Fidelity::Imported,
                true,
            )
            .unwrap_err(),
    ] {
        assert!(matches!(
            error,
            StoreError::ProviderFileReplacementBusy { .. }
        ));
    }
    observer.upsert_session(&sibling_update).unwrap();
    assert_eq!(
        observer
            .get_session(sibling_session)
            .unwrap()
            .role_hint
            .as_deref(),
        Some("safe sibling update")
    );
    assert_eq!(
        store
            .provider_file_publication_session(&scope, owner_session)
            .unwrap()
            .history_record_id,
        Some(owner_record.id)
    );
    store.abandon_provider_file_publication(scope).unwrap();
}
#[test]
fn raw_sql_rejects_base_table_reads_while_crash_marker_fences_owner() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let store = Store::open(&path).unwrap();
    let file = source_file(20, 100);
    let generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    let source = Uuid::from_u128(75_000);
    let session = Uuid::from_u128(75_001);
    let event = Uuid::from_u128(75_002);
    let file_id = Uuid::from_u128(75_003);
    insert_capture_source(&store, source, PATH_A, "raw-sql-owner");
    insert_raw_session(&store, session, source, "raw-sql-owner");
    insert_raw_event(&store, event, 75_002, source, "secret payload");
    store
        .conn
        .execute(
            "INSERT INTO files_touched (id, event_id, path, created_at_ms, updated_at_ms, source_id) VALUES (?1, ?2, 'secret.rs', 1, 1, ?3)",
            params![file_id.to_string(), event.to_string(), source.to_string()],
        )
        .unwrap();
    let outcome = source_outcome(&file, generation, 120);
    let scope = store
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            110,
        )
        .unwrap();
    drop(scope);

    let observer = Store::open(&path).unwrap();
    for sql in [
        "SELECT payload_json FROM events",
        "SELECT external_session_id FROM sessions",
        "SELECT path FROM files_touched",
    ] {
        assert!(matches!(
            observer
                .raw_sql_query(sql, crate::RawSqlOptions::default())
                .unwrap_err(),
            StoreError::ProviderFileReplacementBusy { .. }
        ));
    }
}
#[test]
fn first_owner_write_in_pre_marker_window_is_serialized_into_reconciliation() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let setup = Store::open(&path).unwrap();
    let file = source_file(20, 100);
    let generation = setup
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    setup
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    drop(setup);

    let (window_tx, window_rx) = mpsc::channel();
    let (continue_tx, continue_rx) = mpsc::channel();
    let publication_path = path.clone();
    let publication = thread::spawn(move || {
        let store = Store::open(publication_path).unwrap();
        let file = source_file(20, 100);
        let outcome = source_outcome(&file, generation, 120);
        let scope = store
            .begin_provider_file_publication_inner(
                file.provider,
                outcome.observation,
                MATERIAL_FORMAT,
                ProviderFilePublicationKind::Replacement,
                110,
                || {
                    window_tx.send(()).unwrap();
                    continue_rx.recv().unwrap();
                },
            )
            .unwrap();
        assert!(scope.tracks_prior_material);
        prepare_all(&store, &scope, 2);
        reconcile_all(&store, &scope, 2);
        store
            .finalize_provider_file_publication(
                scope,
                outcome,
                ProviderFilePublicationCommit::Replacement(None),
            )
            .unwrap()
            .reconciliation
    });

    window_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let writer = Store::open(&path).unwrap();
    let source_id = Uuid::from_u128(75_100);
    let session_id = Uuid::from_u128(75_101);
    writer
        .upsert_capture_source(&capture_source_fixture(
            source_id,
            PATH_A,
            "pre-marker-owner",
        ))
        .unwrap();
    writer
        .upsert_session(&session_fixture(session_id, source_id, "pre-marker-owner"))
        .unwrap();
    continue_tx.send(()).unwrap();

    let counts = publication.join().unwrap();
    assert_eq!(counts.sessions_tombstoned, 1);
    assert!(session_deleted_at(&writer, session_id).is_some());
}
#[test]
fn catalog_results_are_fenced_across_connections_and_unmutated_supersession_releases_them() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let source_store = Store::open(&path).unwrap();
    let file = source_file(20, 100);
    let source_generation = source_store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    source_store
        .upsert_source_import_files(source_generation, std::slice::from_ref(&file))
        .unwrap();
    let pending_source_outcome = source_outcome(&file, source_generation, 120);
    let source_scope = source_store
        .begin_provider_file_publication(
            file.provider,
            pending_source_outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            110,
        )
        .unwrap();
    drop(source_scope);

    let observer = Store::open(&path).unwrap();
    assert!(matches!(
        observer
            .record_source_import_file_result(
                file.provider,
                match pending_source_outcome.observation {
                    ProviderFileInventoryObservation::SourceImport { update, .. } => update,
                    ProviderFileInventoryObservation::Catalog { .. } => unreachable!(),
                },
                CatalogIndexedStatus::Failed,
                Some("must remain pending"),
            )
            .unwrap_err(),
        StoreError::ProviderFileReplacementBusy { .. }
    ));
    let source_status: String = observer
        .conn
        .query_row(
            "SELECT indexed_status FROM source_import_files WHERE source_path = ?1",
            params![PATH_A],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(source_status, "pending");

    let next_source_generation = observer
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    observer
        .upsert_source_import_files(next_source_generation, std::slice::from_ref(&file))
        .unwrap();
    let next_source_update = match source_outcome(&file, next_source_generation, 130).observation {
        ProviderFileInventoryObservation::SourceImport { update, .. } => update,
        ProviderFileInventoryObservation::Catalog { .. } => unreachable!(),
    };
    assert_eq!(
        observer
            .record_source_import_file_result(
                file.provider,
                next_source_update,
                CatalogIndexedStatus::Failed,
                Some("new generation result"),
            )
            .unwrap(),
        1
    );

    let catalog = CatalogSession {
        provider: CaptureProvider::Codex,
        source_format: "codex_session_jsonl".to_owned(),
        source_root: "/history/codex".to_owned(),
        source_path: "/history/codex/result-fence.jsonl".to_owned(),
        external_session_id: Some("result-fence".to_owned()),
        parent_external_session_id: None,
        agent_type: AgentType::Primary,
        role_hint: None,
        external_agent_id: None,
        cwd: None,
        session_started_at_ms: Some(1),
        file_size_bytes: 10,
        file_modified_at_ms: 100,
        import_revision: 1,
        cataloged_at_ms: 101,
        metadata: json!({}),
    };
    let catalog_generation = observer
        .allocate_catalog_inventory_generation(catalog.provider, &catalog.source_root)
        .unwrap();
    observer
        .upsert_catalog_sessions(catalog_generation, std::slice::from_ref(&catalog))
        .unwrap();
    assert!(observer
        .catalog_inventory_generation_is_current(
            catalog.provider,
            &catalog.source_root,
            catalog_generation,
        )
        .unwrap());
    let catalog_update = CatalogSourceIndexUpdate {
        source_root: &catalog.source_root,
        source_path: &catalog.source_path,
        file_size_bytes: catalog.file_size_bytes,
        file_modified_at_ms: catalog.file_modified_at_ms,
        import_revision: catalog.import_revision,
        inventory_generation: catalog_generation,
        file_sha256: None,
        event_count: Some(0),
        indexed_at_ms: 120,
    };
    let catalog_outcome = ProviderFileImportOutcome {
        provider: catalog.provider,
        observation: ProviderFileInventoryObservation::Catalog {
            source_format: &catalog.source_format,
            update: catalog_update,
        },
        status: CatalogIndexedStatus::Indexed,
        error: None,
    };
    let catalog_scope = observer
        .begin_provider_file_publication(
            catalog.provider,
            catalog_outcome.observation,
            &catalog.source_format,
            ProviderFilePublicationKind::Replacement,
            110,
        )
        .unwrap();
    drop(catalog_scope);

    let catalog_writer = Store::open(&path).unwrap();
    assert!(!catalog_writer
        .catalog_inventory_generation_is_current(
            catalog.provider,
            &catalog.source_root,
            catalog_generation,
        )
        .unwrap());
    assert!(matches!(
        catalog_writer
            .record_catalog_source_import_result(
                catalog.provider,
                catalog_update,
                CatalogIndexedStatus::Failed,
                Some("must remain pending"),
            )
            .unwrap_err(),
        StoreError::ProviderFileReplacementBusy { .. }
    ));
    assert_eq!(
        catalog_writer
            .catalog_source_index_state(
                catalog.provider,
                &catalog.source_root,
                &catalog.source_path,
            )
            .unwrap(),
        None
    );

    let next_catalog_generation = catalog_writer
        .allocate_catalog_inventory_generation(catalog.provider, &catalog.source_root)
        .unwrap();
    catalog_writer
        .upsert_catalog_sessions(next_catalog_generation, std::slice::from_ref(&catalog))
        .unwrap();
    assert!(catalog_writer
        .catalog_inventory_generation_is_current(
            catalog.provider,
            &catalog.source_root,
            next_catalog_generation,
        )
        .unwrap());
    let next_catalog_update = CatalogSourceIndexUpdate {
        inventory_generation: next_catalog_generation,
        indexed_at_ms: 130,
        ..catalog_update
    };
    assert_eq!(
        catalog_writer
            .record_catalog_source_import_result(
                catalog.provider,
                next_catalog_update,
                CatalogIndexedStatus::Failed,
                Some("new generation result"),
            )
            .unwrap(),
        1
    );
}
#[test]
fn source_import_publication_blocks_cross_family_catalog_status_and_legacy_cursor() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let store = Store::open(&path).unwrap();
    let file = source_file(20, 100);
    let source_generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(source_generation, std::slice::from_ref(&file))
        .unwrap();
    let catalog = catalog_file(20, 100);
    let catalog_generation = store
        .allocate_catalog_inventory_generation(catalog.provider, &catalog.source_root)
        .unwrap();
    store
        .upsert_catalog_sessions(catalog_generation, std::slice::from_ref(&catalog))
        .unwrap();
    let source_scope = store
        .begin_provider_file_publication(
            file.provider,
            source_outcome(&file, source_generation, 120).observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            110,
        )
        .unwrap();
    drop(source_scope);

    let observer = Store::open(&path).unwrap();
    let update = CatalogSourceIndexUpdate {
        source_root: &catalog.source_root,
        source_path: &catalog.source_path,
        file_size_bytes: catalog.file_size_bytes,
        file_modified_at_ms: catalog.file_modified_at_ms,
        import_revision: catalog.import_revision,
        inventory_generation: catalog_generation,
        file_sha256: Some("must-not-advance"),
        event_count: Some(9),
        indexed_at_ms: 130,
    };
    assert!(matches!(
        observer
            .record_catalog_source_import_result(
                catalog.provider,
                update,
                CatalogIndexedStatus::Indexed,
                None,
            )
            .unwrap_err(),
        StoreError::ProviderFileReplacementBusy { .. }
    ));
    let (status, cursor): (String, CatalogLegacyCursor) = observer
        .conn
        .query_row(
            "SELECT indexed_status, last_imported_at_ms, last_imported_file_size_bytes, last_imported_file_modified_at_ms, last_imported_file_sha256, last_imported_event_count FROM catalog_sessions WHERE source_path = ?1",
            params![&catalog.source_path],
            |row| {
                Ok((
                    row.get(0)?,
                    (row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?),
                ))
            },
        )
        .unwrap();
    assert_eq!(status, "pending");
    assert_eq!(cursor, (None, None, None, None, None));
}
#[test]
fn catalog_publication_blocks_cross_family_source_import_status() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let store = Store::open(&path).unwrap();
    let file = source_file(20, 100);
    let source_generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(source_generation, std::slice::from_ref(&file))
        .unwrap();
    let catalog = catalog_file(20, 100);
    let catalog_generation = store
        .allocate_catalog_inventory_generation(catalog.provider, &catalog.source_root)
        .unwrap();
    store
        .upsert_catalog_sessions(catalog_generation, std::slice::from_ref(&catalog))
        .unwrap();
    let catalog_update = CatalogSourceIndexUpdate {
        source_root: &catalog.source_root,
        source_path: &catalog.source_path,
        file_size_bytes: catalog.file_size_bytes,
        file_modified_at_ms: catalog.file_modified_at_ms,
        import_revision: catalog.import_revision,
        inventory_generation: catalog_generation,
        file_sha256: None,
        event_count: Some(0),
        indexed_at_ms: 120,
    };
    let catalog_scope = store
        .begin_provider_file_publication(
            catalog.provider,
            ProviderFileInventoryObservation::Catalog {
                source_format: &catalog.source_format,
                update: catalog_update,
            },
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            110,
        )
        .unwrap();
    drop(catalog_scope);

    let observer = Store::open(&path).unwrap();
    let source_update = match source_outcome(&file, source_generation, 130).observation {
        ProviderFileInventoryObservation::SourceImport { update, .. } => update,
        ProviderFileInventoryObservation::Catalog { .. } => unreachable!(),
    };
    assert!(matches!(
        observer
            .record_source_import_file_result(
                file.provider,
                source_update,
                CatalogIndexedStatus::Indexed,
                None,
            )
            .unwrap_err(),
        StoreError::ProviderFileReplacementBusy { .. }
    ));
    let status: String = observer
        .conn
        .query_row(
            "SELECT indexed_status FROM source_import_files WHERE provider = ?1 AND source_root = ?2 AND source_path = ?3",
            params![file.provider.as_str(), &file.source_root, &file.source_path],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "pending");
}
#[test]
fn superseded_mutated_catalog_publication_keeps_new_generation_noncurrent() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let catalog = catalog_file(20, 100);
    let first_generation = store
        .allocate_catalog_inventory_generation(catalog.provider, &catalog.source_root)
        .unwrap();
    store
        .upsert_catalog_sessions(first_generation, std::slice::from_ref(&catalog))
        .unwrap();
    let first_update = CatalogSourceIndexUpdate {
        source_root: &catalog.source_root,
        source_path: &catalog.source_path,
        file_size_bytes: catalog.file_size_bytes,
        file_modified_at_ms: catalog.file_modified_at_ms,
        import_revision: catalog.import_revision,
        inventory_generation: first_generation,
        file_sha256: None,
        event_count: Some(0),
        indexed_at_ms: 110,
    };
    let scope = store
        .begin_provider_file_publication(
            catalog.provider,
            ProviderFileInventoryObservation::Catalog {
                source_format: &catalog.source_format,
                update: first_update,
            },
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            105,
        )
        .unwrap();
    let source = capture_source_fixture(
        Uuid::from_u128(75_300),
        PATH_A,
        "mutated-catalog-generation",
    );
    store
        .with_provider_file_publication_writes(&scope, |store| store.upsert_capture_source(&source))
        .unwrap();

    let second_generation = store
        .allocate_catalog_inventory_generation(catalog.provider, &catalog.source_root)
        .unwrap();
    store
        .upsert_catalog_sessions(second_generation, std::slice::from_ref(&catalog))
        .unwrap();
    assert!(!store
        .catalog_inventory_generation_is_current(
            catalog.provider,
            &catalog.source_root,
            second_generation,
        )
        .unwrap());
    drop(scope);
}
#[test]
fn obsolete_unmutated_catalog_publication_does_not_fence_new_generation() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let catalog = catalog_file(20, 100);
    let first_generation = store
        .allocate_catalog_inventory_generation(catalog.provider, &catalog.source_root)
        .unwrap();
    store
        .upsert_catalog_sessions(first_generation, std::slice::from_ref(&catalog))
        .unwrap();
    let first_update = CatalogSourceIndexUpdate {
        source_root: &catalog.source_root,
        source_path: &catalog.source_path,
        file_size_bytes: catalog.file_size_bytes,
        file_modified_at_ms: catalog.file_modified_at_ms,
        import_revision: catalog.import_revision,
        inventory_generation: first_generation,
        file_sha256: None,
        event_count: Some(0),
        indexed_at_ms: 110,
    };
    let scope = store
        .begin_provider_file_publication(
            catalog.provider,
            ProviderFileInventoryObservation::Catalog {
                source_format: &catalog.source_format,
                update: first_update,
            },
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            105,
        )
        .unwrap();

    let second_generation = store
        .allocate_catalog_inventory_generation(catalog.provider, &catalog.source_root)
        .unwrap();
    store
        .upsert_catalog_sessions(second_generation, std::slice::from_ref(&catalog))
        .unwrap();
    assert!(store
        .catalog_inventory_generation_is_current(
            catalog.provider,
            &catalog.source_root,
            second_generation,
        )
        .unwrap());
    drop(scope);
}
#[test]
fn obsolete_unmutated_marker_does_not_block_raw_sql_or_unrelated_archive() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let store = Store::open(&path).unwrap();
    let unrelated_source_id = Uuid::from_u128(75_200);
    let unrelated_session_id = Uuid::from_u128(75_201);
    store
        .upsert_capture_source(&capture_source_fixture(
            unrelated_source_id,
            "/history/claude/projects/unrelated.jsonl",
            "unrelated-archive",
        ))
        .unwrap();
    store
        .upsert_session(&session_fixture(
            unrelated_session_id,
            unrelated_source_id,
            "unrelated-archive",
        ))
        .unwrap();
    let unrelated_archive = store.export_archive().unwrap();

    let file = source_file(20, 100);
    let generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    let outcome = source_outcome(&file, generation, 120);
    let scope = store
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            110,
        )
        .unwrap();
    assert!(!scope.tracks_prior_material);

    let mut observer = Store::open(&path).unwrap();
    assert!(matches!(
        observer
            .raw_sql_query("SELECT id FROM sessions", crate::RawSqlOptions::default())
            .unwrap_err(),
        StoreError::ProviderFileReplacementBusy { .. }
    ));
    observer.import_archive(&unrelated_archive, true).unwrap();
    let mut owner_archive = unrelated_archive.clone();
    owner_archive.capture_sources[0].descriptor.raw_source_path = Some(PATH_A.to_owned());
    owner_archive.capture_sources[0].descriptor.source_root = Some(ROOT.to_owned());
    owner_archive.capture_sources[0].descriptor.source_format = Some(MATERIAL_FORMAT.to_owned());
    assert!(matches!(
        observer.import_archive(&owner_archive, true).unwrap_err(),
        StoreError::ProviderFileReplacementBusy { .. }
    ));

    drop(scope);
    let tombstone_generation = observer
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    observer
        .mark_source_import_missing_paths_stale(
            file.provider,
            &file.source_root,
            &[],
            130,
            tombstone_generation,
        )
        .unwrap();
    let raw = observer
        .raw_sql_query(
            "SELECT external_session_id FROM sessions",
            crate::RawSqlOptions::default(),
        )
        .unwrap();
    assert_eq!(raw.returned_rows, 1);
    observer.import_archive(&unrelated_archive, true).unwrap();
}
