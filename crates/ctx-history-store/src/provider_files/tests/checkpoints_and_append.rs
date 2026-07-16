#[test]
fn checkpoint_round_trip_preserves_exact_boundary_data_and_version() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = source_file(10, 100);
    let generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    let mut expected = checkpoint(10, 3, "unix:2049:881", 110);
    expected.resume_state = Some(vec![0, 1, 2, 0xff]);

    store
        .upsert_provider_file_checkpoint(source_outcome(&file, generation, 110), &expected)
        .unwrap();

    let actual = store
        .provider_file_checkpoint(expected.key())
        .unwrap()
        .unwrap();
    assert_eq!(actual, expected);
    assert_eq!(actual.checkpoint_version, 1);
    assert_eq!(actual.resume_state.as_deref(), Some(&[0, 1, 2, 0xff][..]));
    let mut invalid_version = expected.clone();
    invalid_version.checkpoint_version = 0;
    let error = store
        .upsert_provider_file_checkpoint(source_outcome(&file, generation, 111), &invalid_version)
        .unwrap_err();
    assert!(matches!(
        error,
        StoreError::InvalidProviderFileCheckpoint("checkpoint version must be positive")
    ));

    for invalid_state in [
        Vec::new(),
        vec![b'x'; PROVIDER_FILE_CHECKPOINT_RESUME_STATE_MAX_BYTES + 1],
    ] {
        let mut invalid = expected.clone();
        invalid.resume_state = Some(invalid_state);
        let error = store
            .upsert_provider_file_checkpoint(source_outcome(&file, generation, 112), &invalid)
            .unwrap_err();
        assert!(matches!(
            error,
            StoreError::InvalidProviderFileCheckpoint(
                "resume state must not be empty" | "resume state exceeds the maximum encoded size"
            )
        ));
        assert_eq!(
            store.provider_file_checkpoint(expected.key()).unwrap(),
            Some(expected.clone())
        );
    }
}

#[test]
fn exact_observation_survives_newer_scan_generation_and_advances_checkpoint() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = source_file(10, 100);
    let stale_generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(stale_generation, std::slice::from_ref(&file))
        .unwrap();
    let current_generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(current_generation, std::slice::from_ref(&file))
        .unwrap();
    let checkpoint = checkpoint(10, 3, "unix:2049:881", 110);

    store
        .upsert_provider_file_checkpoint(source_outcome(&file, stale_generation, 110), &checkpoint)
        .unwrap();
    assert_eq!(
        store.provider_file_checkpoint(checkpoint.key()).unwrap(),
        Some(checkpoint)
    );
    let status: String = store
        .conn
        .query_row(
            "SELECT indexed_status FROM source_import_files WHERE provider = 'claude' AND source_root = ?1 AND source_path = ?2",
            params![ROOT, PATH_A],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "indexed");
}

#[test]
fn imported_content_without_finalization_replays_before_checkpoint_advances() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let original = source_file(10, 100);
    let first_generation = store
        .allocate_source_import_inventory_generation(original.provider, &original.source_root)
        .unwrap();
    store
        .upsert_source_import_files(first_generation, std::slice::from_ref(&original))
        .unwrap();
    let first_checkpoint = checkpoint(10, 3, "unix:2049:881", 110);
    store
        .upsert_provider_file_checkpoint(
            source_outcome(&original, first_generation, 110),
            &first_checkpoint,
        )
        .unwrap();

    let appended = source_file(20, 120);
    let second_generation = store
        .allocate_source_import_inventory_generation(appended.provider, &appended.source_root)
        .unwrap();
    store
        .upsert_source_import_files(second_generation, std::slice::from_ref(&appended))
        .unwrap();
    store
        .conn
        .execute_batch(
            "CREATE TABLE replay_probe(value TEXT); INSERT INTO replay_probe VALUES ('materialized');",
        )
        .unwrap();

    assert_eq!(
        store
            .provider_file_checkpoint(first_checkpoint.key())
            .unwrap()
            .unwrap()
            .committed_byte_offset,
        10
    );

    let second_checkpoint = checkpoint(20, 5, "unix:2049:881", 130);
    let mut second_checkpoint = second_checkpoint;
    second_checkpoint.head_sha256 = "f".repeat(64);
    store
        .upsert_provider_file_checkpoint(
            source_outcome(&appended, second_generation, 130),
            &second_checkpoint,
        )
        .unwrap();
    assert_eq!(
        store
            .provider_file_checkpoint(second_checkpoint.key())
            .unwrap()
            .unwrap(),
        second_checkpoint
    );
}

#[test]
fn deferred_partial_completion_retains_checkpoint_and_completes_exact_observation() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let original = source_file(10, 100);
    let first_generation = store
        .allocate_source_import_inventory_generation(original.provider, &original.source_root)
        .unwrap();
    store
        .upsert_source_import_files(first_generation, std::slice::from_ref(&original))
        .unwrap();
    let original_checkpoint = checkpoint(10, 3, "unix:2049:partial", 110);
    store
        .upsert_provider_file_checkpoint(
            source_outcome(&original, first_generation, 110),
            &original_checkpoint,
        )
        .unwrap();

    let partial = source_file(15, 120);
    let second_generation = store
        .allocate_source_import_inventory_generation(partial.provider, &partial.source_root)
        .unwrap();
    store
        .upsert_source_import_files(second_generation, std::slice::from_ref(&partial))
        .unwrap();
    store
        .complete_provider_file_observation_retaining_checkpoint(source_outcome(
            &partial,
            second_generation,
            130,
        ))
        .unwrap();

    assert_eq!(
        store
            .provider_file_checkpoint(original_checkpoint.key())
            .unwrap()
            .unwrap(),
        original_checkpoint
    );
    let state: (String, i64, i64) = store
        .conn
        .query_row(
            "SELECT indexed_status, indexed_file_size_bytes, indexed_file_modified_at_ms FROM source_import_files WHERE provider = 'claude' AND source_root = ?1 AND source_path = ?2",
            params![ROOT, PATH_A],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(state, ("indexed".to_owned(), 15, 120));
}

#[test]
fn changed_checkpoint_version_requires_replacement_without_completing_observation() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let original = source_file(10, 100);
    let first_generation = store
        .allocate_source_import_inventory_generation(original.provider, &original.source_root)
        .unwrap();
    store
        .upsert_source_import_files(first_generation, std::slice::from_ref(&original))
        .unwrap();
    let original_checkpoint = checkpoint(10, 3, "unix:2049:version", 110);
    store
        .upsert_provider_file_checkpoint(
            source_outcome(&original, first_generation, 110),
            &original_checkpoint,
        )
        .unwrap();

    let appended = source_file(20, 120);
    let second_generation = store
        .allocate_source_import_inventory_generation(appended.provider, &appended.source_root)
        .unwrap();
    store
        .upsert_source_import_files(second_generation, std::slice::from_ref(&appended))
        .unwrap();
    let mut incompatible = checkpoint(20, 5, "unix:2049:version", 130);
    incompatible.checkpoint_version = 2;
    let error = store
        .upsert_provider_file_checkpoint(
            source_outcome(&appended, second_generation, 130),
            &incompatible,
        )
        .unwrap_err();

    assert!(matches!(
        error,
        StoreError::ProviderFileCheckpointRequiresReplacement { .. }
    ));
    assert_eq!(
        store
            .provider_file_checkpoint(original_checkpoint.key())
            .unwrap()
            .unwrap(),
        original_checkpoint
    );
    let status: String = store
        .conn
        .query_row(
            "SELECT indexed_status FROM source_import_files WHERE provider = 'claude' AND source_root = ?1 AND source_path = ?2",
            params![ROOT, PATH_A],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "pending");
}

#[test]
fn catalog_observation_can_finalize_a_provider_file_checkpoint() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let catalog = CatalogSession {
        provider: CaptureProvider::Codex,
        source_format: "codex_session_jsonl".to_owned(),
        source_root: "/history/codex".to_owned(),
        source_path: "/history/codex/session.jsonl".to_owned(),
        external_session_id: Some("session".to_owned()),
        parent_external_session_id: None,
        agent_type: AgentType::Primary,
        role_hint: None,
        external_agent_id: None,
        cwd: None,
        session_started_at_ms: Some(1),
        file_size_bytes: 12,
        file_modified_at_ms: 20,
        import_revision: 3,
        cataloged_at_ms: 21,
        metadata: json!({"file_observation_token_v1": "checkpoint-catalog-token"}),
    };
    let generation = store
        .allocate_catalog_inventory_generation(catalog.provider, &catalog.source_root)
        .unwrap();
    store
        .upsert_catalog_sessions(generation, std::slice::from_ref(&catalog))
        .unwrap();
    let checkpoint = ProviderFileCheckpoint {
        provider: catalog.provider,
        source_format: catalog.source_format.clone(),
        source_root: catalog.source_root.clone(),
        source_path: catalog.source_path.clone(),
        import_revision: catalog.import_revision,
        checkpoint_version: 1,
        stable_file_identity: "unix:2049:991".to_owned(),
        committed_byte_offset: 12,
        committed_complete_line_count: 2,
        head_sha256: "d".repeat(64),
        boundary_sha256: "e".repeat(64),
        resume_state: None,
        updated_at_ms: 30,
    };
    let outcome = ProviderFileImportOutcome {
        provider: catalog.provider,
        observation: ProviderFileInventoryObservation::ObservedCatalog {
            source_format: &catalog.source_format,
            update: CatalogSourceIndexUpdate {
                source_root: &catalog.source_root,
                source_path: &catalog.source_path,
                file_size_bytes: catalog.file_size_bytes,
                file_modified_at_ms: catalog.file_modified_at_ms,
                import_revision: catalog.import_revision,
                inventory_generation: generation,
                file_sha256: None,
                event_count: Some(2),
                indexed_at_ms: 30,
            },
            metadata: &catalog.metadata,
        },
        status: CatalogIndexedStatus::Indexed,
        error: None,
    };

    store
        .upsert_provider_file_checkpoint(outcome, &checkpoint)
        .unwrap();

    assert_eq!(
        store
            .provider_file_checkpoint(checkpoint.key())
            .unwrap()
            .unwrap(),
        checkpoint
    );
}

#[test]
fn catalog_publication_requires_an_observation_token() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let mut catalog = catalog_file(12, 20);
    catalog.metadata = json!({});
    let generation = store
        .allocate_catalog_inventory_generation(catalog.provider, &catalog.source_root)
        .unwrap();
    store
        .upsert_catalog_sessions(generation, std::slice::from_ref(&catalog))
        .unwrap();
    let update = CatalogSourceIndexUpdate {
        source_root: &catalog.source_root,
        source_path: &catalog.source_path,
        file_size_bytes: catalog.file_size_bytes,
        file_modified_at_ms: catalog.file_modified_at_ms,
        import_revision: catalog.import_revision,
        inventory_generation: generation,
        file_sha256: None,
        event_count: None,
        indexed_at_ms: 30,
    };

    assert!(matches!(
        store
            .record_observed_catalog_source_import_result(
                catalog.provider,
                update,
                &catalog.metadata,
                CatalogIndexedStatus::Indexed,
                None,
            )
            .unwrap_err(),
        StoreError::InvalidProviderFileCheckpoint("catalog observation token is required")
    ));

    assert!(matches!(
        store
            .begin_provider_file_publication(
                catalog.provider,
                ProviderFileInventoryObservation::ObservedCatalog {
                    source_format: &catalog.source_format,
                    update,
                    metadata: &catalog.metadata,
                },
                &catalog.source_format,
                ProviderFilePublicationKind::Replacement,
                30,
            )
            .unwrap_err(),
        StoreError::InvalidProviderFileCheckpoint("catalog observation token is required")
    ));
}

#[test]
fn catalog_append_completion_preserves_rejections_and_accumulates_event_count() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let mut catalog = CatalogSession {
        provider: CaptureProvider::Codex,
        source_format: "codex_session_jsonl".to_owned(),
        source_root: "/history/codex".to_owned(),
        source_path: "/history/codex/session.jsonl".to_owned(),
        external_session_id: Some("cumulative-session".to_owned()),
        parent_external_session_id: None,
        agent_type: AgentType::Primary,
        role_hint: None,
        external_agent_id: None,
        cwd: None,
        session_started_at_ms: Some(1),
        file_size_bytes: 10,
        file_modified_at_ms: 100,
        import_revision: 3,
        cataloged_at_ms: 101,
        metadata: json!({"file_observation_token_v1": "append-catalog-token"}),
    };
    let first_generation = store
        .allocate_catalog_inventory_generation(catalog.provider, &catalog.source_root)
        .unwrap();
    store
        .upsert_catalog_sessions(first_generation, std::slice::from_ref(&catalog))
        .unwrap();
    let first_update = CatalogSourceIndexUpdate {
        source_root: &catalog.source_root,
        source_path: &catalog.source_path,
        file_size_bytes: 10,
        file_modified_at_ms: 100,
        import_revision: 3,
        inventory_generation: first_generation,
        file_sha256: None,
        event_count: Some(5),
        indexed_at_ms: 110,
    };
    let first_outcome = ProviderFileImportOutcome {
        provider: catalog.provider,
        observation: ProviderFileInventoryObservation::ObservedCatalog {
            source_format: &catalog.source_format,
            update: first_update,
            metadata: &catalog.metadata,
        },
        status: CatalogIndexedStatus::CompletedWithRejections,
        error: Some("one malformed event"),
    };
    let mut first_checkpoint = ProviderFileCheckpoint {
        provider: catalog.provider,
        source_format: catalog.source_format.clone(),
        source_root: catalog.source_root.clone(),
        source_path: catalog.source_path.clone(),
        import_revision: 3,
        checkpoint_version: 1,
        stable_file_identity: "unix:2049:catalog".to_owned(),
        committed_byte_offset: 10,
        committed_complete_line_count: 2,
        head_sha256: "d".repeat(64),
        boundary_sha256: "e".repeat(64),
        resume_state: None,
        updated_at_ms: 110,
    };
    store
        .upsert_provider_file_checkpoint(first_outcome, &first_checkpoint)
        .unwrap();

    catalog.file_size_bytes = 20;
    catalog.file_modified_at_ms = 120;
    catalog.cataloged_at_ms = 121;
    let second_generation = store
        .allocate_catalog_inventory_generation(catalog.provider, &catalog.source_root)
        .unwrap();
    store
        .upsert_catalog_sessions(second_generation, std::slice::from_ref(&catalog))
        .unwrap();
    let preserved_pending_status: String = store
        .conn
        .query_row(
            "SELECT indexed_status FROM catalog_sessions WHERE source_path = ?1",
            params![&catalog.source_path],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(preserved_pending_status, "completed_with_rejections");

    let second_update = CatalogSourceIndexUpdate {
        source_root: &catalog.source_root,
        source_path: &catalog.source_path,
        file_size_bytes: 20,
        file_modified_at_ms: 120,
        import_revision: 3,
        inventory_generation: second_generation,
        file_sha256: None,
        event_count: Some(2),
        indexed_at_ms: 130,
    };
    first_checkpoint.committed_byte_offset = 20;
    first_checkpoint.committed_complete_line_count = 4;
    first_checkpoint.boundary_sha256 = "f".repeat(64);
    first_checkpoint.updated_at_ms = 130;
    store
        .upsert_provider_file_checkpoint(
            ProviderFileImportOutcome {
                provider: catalog.provider,
                observation: ProviderFileInventoryObservation::ObservedCatalog {
                    source_format: &catalog.source_format,
                    update: second_update,
                    metadata: &catalog.metadata,
                },
                status: CatalogIndexedStatus::Indexed,
                error: None,
            },
            &first_checkpoint,
        )
        .unwrap();

    let state: (String, Option<String>, i64, i64) = store
        .conn
        .query_row(
            "SELECT indexed_status, indexed_error, indexed_event_count, last_imported_event_count FROM catalog_sessions WHERE source_path = ?1",
            params![&catalog.source_path],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        state,
        (
            "completed_with_rejections".to_owned(),
            Some("one malformed event".to_owned()),
            7,
            7,
        )
    );
}

#[test]
fn append_and_retained_tail_share_the_owner_lease_and_atomic_publication() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let store = Store::open(&path).unwrap();
    let original = source_file(10, 100);
    let first_generation = store
        .allocate_source_import_inventory_generation(original.provider, &original.source_root)
        .unwrap();
    store
        .upsert_source_import_files(first_generation, std::slice::from_ref(&original))
        .unwrap();
    let first_checkpoint = checkpoint(10, 3, "unix:2049:append-lease", 110);
    store
        .upsert_provider_file_checkpoint(
            source_outcome(&original, first_generation, 110),
            &first_checkpoint,
        )
        .unwrap();
    let owner_source = Uuid::from_u128(40_000);
    let sibling_source = Uuid::from_u128(40_001);
    let old_event = Uuid::from_u128(40_002);
    insert_capture_source(&store, owner_source, PATH_A, "append-owner");
    insert_capture_source(
        &store,
        sibling_source,
        "/history/claude/projects/b.jsonl",
        "append-sibling",
    );
    insert_raw_event(&store, old_event, 1, owner_source, "old append material");

    let appended = source_file(20, 120);
    let second_generation = store
        .allocate_source_import_inventory_generation(appended.provider, &appended.source_root)
        .unwrap();
    store
        .upsert_source_import_files(second_generation, std::slice::from_ref(&appended))
        .unwrap();
    let outcome = source_outcome(&appended, second_generation, 130);
    let scope = store
        .begin_provider_file_publication(
            appended.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Incremental,
            125,
        )
        .unwrap();
    assert!(
        !store
            .provider_file_publication
            .borrow()
            .as_ref()
            .unwrap()
            .attached
    );
    let observer = Store::open(&path).unwrap();
    assert!(matches!(
        observer
            .begin_provider_file_publication(
                appended.provider,
                outcome.observation,
                MATERIAL_FORMAT,
                ProviderFilePublicationKind::Incremental,
                126,
            )
            .unwrap_err(),
        StoreError::ProviderFileReplacementBusy { .. }
    ));
    assert!(observer.list_events().unwrap().is_empty());

    let owner_event = event_fixture(
        Uuid::from_u128(40_003),
        2,
        owner_source,
        "append-owner-event".to_owned(),
        "new append material",
    );
    assert!(matches!(
        store.upsert_event(&owner_event).unwrap_err(),
        StoreError::ProviderFileReplacementBusy { .. }
    ));
    let sibling_event = event_fixture(
        Uuid::from_u128(40_004),
        3,
        sibling_source,
        "append-sibling-event".to_owned(),
        "must not join",
    );
    assert!(matches!(
        store
            .with_provider_file_publication_writes(&scope, |store| {
                store.upsert_event(&sibling_event)
            })
            .unwrap_err(),
        StoreError::ProviderFilePublicationOwnerMismatch { .. }
    ));
    store
        .with_provider_file_publication_writes(&scope, |store| store.upsert_event(&owner_event))
        .unwrap();
    let second_checkpoint = checkpoint(20, 5, "unix:2049:append-lease", 130);
    store
        .finalize_provider_file_publication(
            scope,
            outcome,
            ProviderFilePublicationCommit::Append(&second_checkpoint),
        )
        .unwrap();
    assert_eq!(observer.list_events().unwrap().len(), 2);
    assert_eq!(
        store
            .provider_file_checkpoint(second_checkpoint.key())
            .unwrap(),
        Some(second_checkpoint.clone())
    );

    let partial = source_file(25, 140);
    let third_generation = store
        .allocate_source_import_inventory_generation(partial.provider, &partial.source_root)
        .unwrap();
    store
        .upsert_source_import_files(third_generation, std::slice::from_ref(&partial))
        .unwrap();
    let partial_outcome = source_outcome(&partial, third_generation, 150);
    let retained = store
        .begin_provider_file_publication(
            partial.provider,
            partial_outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Incremental,
            145,
        )
        .unwrap();
    assert!(observer.list_events().unwrap().is_empty());
    store
        .finalize_provider_file_publication(
            retained,
            partial_outcome,
            ProviderFilePublicationCommit::RetainCheckpoint,
        )
        .unwrap();
    assert_eq!(observer.list_events().unwrap().len(), 2);
    assert_eq!(
        store
            .provider_file_checkpoint(second_checkpoint.key())
            .unwrap(),
        Some(second_checkpoint)
    );
}

#[test]
fn initial_import_uses_zero_seen_staging_rows() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = source_file(10, 100);
    let generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    let outcome = source_outcome(&file, generation, 110);

    let scope = store
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            100,
        )
        .unwrap();

    assert!(!scope.tracks_prior_material);
    assert!(store.provider_file_publication.borrow().is_some());
    assert!(
        store
            .provider_file_publication
            .borrow()
            .as_ref()
            .unwrap()
            .attached
    );
    assert_eq!(staged_seen_count(&store), 0);
    assert!(store.has_pending_provider_file_publications().unwrap());
    assert!(matches!(
        store
            .reconcile_provider_file_publication_slice(&scope, usize::MAX)
            .unwrap_err(),
        StoreError::ProviderFileReconciliationLimitOutOfRange {
            value: usize::MAX,
            max: PROVIDER_FILE_RECONCILIATION_MAX_ROWS,
        }
    ));
    let counts = store
        .finalize_provider_file_publication(
            scope,
            outcome,
            ProviderFilePublicationCommit::Replacement(Some(&checkpoint(
                10,
                3,
                "unix:2049:first",
                110,
            ))),
        )
        .unwrap();
    assert_eq!(
        counts.reconciliation,
        ProviderFileReconciliationCounts::default()
    );
    assert_eq!(store.semantic_replacement_revision().unwrap(), 0);
}

#[test]
fn provider_publications_are_globally_capped_and_exactly_resumable() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let files = (0..129)
        .map(|index| {
            let mut file = source_file(10, 100);
            file.source_path = format!("{ROOT}/{index:03}.jsonl");
            file
        })
        .collect::<Vec<_>>();
    let generation;
    {
        let store = Store::open(&path).unwrap();
        generation = store
            .allocate_source_import_inventory_generation(files[0].provider, ROOT)
            .unwrap();
        store
            .upsert_source_import_files(generation, &files)
            .unwrap();
        let outcome = source_outcome(&files[0], generation, 110);
        let scope = store
            .begin_provider_file_publication(
                files[0].provider,
                outcome.observation,
                MATERIAL_FORMAT,
                ProviderFilePublicationKind::Replacement,
                105,
            )
            .unwrap();
        drop(scope);
    }

    for file in &files[1..] {
        let observer = Store::open(&path).unwrap();
        let outcome = source_outcome(file, generation, 110);
        assert!(matches!(
            observer
                .begin_provider_file_publication(
                    file.provider,
                    outcome.observation,
                    MATERIAL_FORMAT,
                    ProviderFilePublicationKind::Replacement,
                    106,
                )
                .unwrap_err(),
            StoreError::ProviderFileReplacementBusy { .. }
        ));
        let marker_count: usize = observer
            .conn
            .query_row(
                "SELECT COUNT(*) FROM provider_file_publications",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(marker_count, 1);
    }

    let reopened = Store::open(&path).unwrap();
    let outcome = source_outcome(&files[0], generation, 110);
    let adopted = reopened
        .begin_provider_file_publication(
            files[0].provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            107,
        )
        .unwrap();
    assert!(matches!(
        reopened.abort_provider_file_publication(adopted).unwrap(),
        std::ops::ControlFlow::Continue(None)
    ));
    assert!(!reopened.has_pending_provider_file_publications().unwrap());
}

#[test]
fn first_rejected_replacement_finalizes_without_a_checkpoint() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = source_file(10, 100);
    let generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    let outcome = ProviderFileImportOutcome {
        provider: file.provider,
        observation: ProviderFileInventoryObservation::SourceImport {
            source_format: &file.source_format,
            update: SourceImportFileIndexUpdate {
                source_root: &file.source_root,
                source_path: &file.source_path,
                file_size_bytes: file.file_size_bytes,
                file_modified_at_ms: file.file_modified_at_ms,
                import_revision: file.import_revision,
                inventory_generation: generation,
                metadata: &file.metadata,
                indexed_at_ms: 110,
            },
        },
        status: CatalogIndexedStatus::Rejected,
        error: Some("all source content was rejected"),
    };
    let scope = store
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            105,
        )
        .unwrap();

    store
        .finalize_provider_file_publication(
            scope,
            outcome,
            ProviderFilePublicationCommit::Replacement(None),
        )
        .unwrap();

    let state: (String, Option<String>) = store
        .conn
        .query_row(
            "SELECT indexed_status, indexed_error FROM source_import_files WHERE provider = ?1 AND source_root = ?2 AND source_path = ?3",
            params![file.provider.as_str(), &file.source_root, &file.source_path],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(state.0, "rejected");
    assert_eq!(state.1.as_deref(), outcome.error);
    assert!(!store.has_pending_provider_file_publications().unwrap());
    assert!(store
        .provider_file_checkpoint(ProviderFileCheckpointKey {
            provider: file.provider,
            source_format: &file.source_format,
            source_root: &file.source_root,
            source_path: &file.source_path,
        })
        .unwrap()
        .is_none());
}

#[test]
fn rejected_replacement_cannot_discard_prior_material() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = source_file(10, 100);
    let generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    let prior_source = Uuid::from_u128(901);
    insert_capture_source(&store, prior_source, PATH_A, "prior-material");
    insert_raw_event(
        &store,
        Uuid::from_u128(903),
        1,
        prior_source,
        "prior material",
    );
    let mut outcome = source_outcome(&file, generation, 110);
    outcome.status = CatalogIndexedStatus::Rejected;
    outcome.error = Some("replacement rejected");
    let scope = store
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            105,
        )
        .unwrap();
    assert!(scope.tracks_prior_material());
    prepare_all(&store, &scope, 8);

    assert!(matches!(
        store
            .finalize_provider_file_publication(
                scope,
                outcome,
                ProviderFilePublicationCommit::Replacement(None),
            )
            .unwrap_err(),
        StoreError::InvalidProviderFileCheckpoint(_)
    ));
}

#[test]
fn rejected_replacement_cannot_publish_staged_material() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = source_file(10, 100);
    let generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    let mut outcome = source_outcome(&file, generation, 110);
    outcome.status = CatalogIndexedStatus::Rejected;
    outcome.error = Some("replacement rejected");
    let scope = store
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            105,
        )
        .unwrap();
    let mut record = ctx_history_core::HistoryRecord::new(
        "staged record",
        "must not escape a rejected publication",
        Vec::new(),
        "note",
        None,
    );
    record.id = Uuid::from_u128(902);
    store
        .with_provider_file_publication_writes(&scope, |store| store.upsert_record(&record))
        .unwrap();

    assert!(matches!(
        store
            .finalize_provider_file_publication(
                scope,
                outcome,
                ProviderFilePublicationCommit::Replacement(None),
            )
            .unwrap_err(),
        StoreError::InvalidProviderFileCheckpoint(_)
    ));
}

#[test]
fn rejected_replacement_cannot_publish_partial_capture_source() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let store = Store::open(&path).unwrap();
    let file = source_file(10, 100);
    let generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    let mut outcome = source_outcome(&file, generation, 110);
    outcome.status = CatalogIndexedStatus::Rejected;
    outcome.error = Some("source was written before the import was rejected");
    let scope = store
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            105,
        )
        .unwrap();
    let source_id = Uuid::from_u128(905);
    let source = capture_source_fixture(source_id, PATH_A, "rejected-partial-source");
    store
        .with_provider_file_publication_writes(&scope, |store| store.upsert_capture_source(&source))
        .unwrap();
    let observer = Store::open(&path).unwrap();
    assert!(matches!(
        observer.get_capture_source(source_id),
        Err(StoreError::NotFound(id)) if id == source_id
    ));

    assert!(matches!(
        store
            .finalize_provider_file_publication(
                scope,
                outcome,
                ProviderFilePublicationCommit::Replacement(None),
            )
            .unwrap_err(),
        StoreError::InvalidProviderFileCheckpoint(_)
    ));
    assert!(store.has_pending_provider_file_publications().unwrap());
    assert!(matches!(
        observer.get_capture_source(source_id),
        Err(StoreError::NotFound(id)) if id == source_id
    ));

    store
        .mark_source_import_missing_paths_stale(
            file.provider,
            &file.source_root,
            &[],
            120,
            generation,
        )
        .unwrap();
    let retirement = store
        .begin_provider_file_publication_retirement(
            file.provider,
            MATERIAL_FORMAT,
            &file.source_root,
            &file.source_path,
            125,
        )
        .unwrap()
        .unwrap();
    reconcile_all(&store, &retirement, 1);
    store.retire_provider_file_publication(retirement).unwrap();
    assert!(!row_exists(&store, "capture_sources", source_id));
    assert!(!store.has_provider_data(file.provider).unwrap());
}

#[test]
fn completed_rejected_replacement_adopts_after_discarding_orphan_source() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let file = source_file(10, 100);
    let generation;
    {
        let store = Store::open(&path).unwrap();
        generation = store
            .allocate_source_import_inventory_generation(file.provider, &file.source_root)
            .unwrap();
        store
            .upsert_source_import_files(generation, std::slice::from_ref(&file))
            .unwrap();
        let scope = store
            .begin_provider_file_publication(
                file.provider,
                source_outcome(&file, generation, 110).observation,
                MATERIAL_FORMAT,
                ProviderFilePublicationKind::Replacement,
                105,
            )
            .unwrap();
        let source_id = Uuid::from_u128(906);
        let source = capture_source_fixture(source_id, PATH_A, "rejected-orphan-source");
        store
            .with_provider_file_publication_writes(&scope, |store| {
                store.upsert_capture_source(&source)
            })
            .unwrap();
        assert_eq!(
            store
                .discard_provider_file_publication_orphan_capture_sources(&scope)
                .unwrap(),
            1
        );
        assert!(!row_exists(&store, "capture_sources", source_id));
        store
            .stage_provider_file_publication_completion(
                &scope,
                &ProviderFilePublicationCompletion {
                    version: 1,
                    payload: json!({"rejected": true}),
                },
            )
            .unwrap();
        store.abandon_provider_file_publication(scope).unwrap();
    }

    let reopened = Store::open(&path).unwrap();
    let mut outcome = source_outcome(&file, generation, 120);
    outcome.status = CatalogIndexedStatus::Rejected;
    outcome.error = Some("all source content was rejected");
    let scope = reopened
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            115,
        )
        .unwrap();
    assert!(!scope.tracks_prior_material());
    reopened
        .finalize_provider_file_publication(
            scope,
            outcome,
            ProviderFilePublicationCommit::Replacement(None),
        )
        .unwrap();
    assert!(!reopened.has_pending_provider_file_publications().unwrap());
}

#[test]
fn first_import_record_writes_stay_hidden_across_reopen() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let record_ids = [
        Uuid::from_u128(910),
        Uuid::from_u128(911),
        Uuid::from_u128(912),
    ];
    {
        let store = Store::open(&path).unwrap();
        let file = source_file(10, 100);
        let generation = store
            .allocate_source_import_inventory_generation(file.provider, &file.source_root)
            .unwrap();
        store
            .upsert_source_import_files(generation, std::slice::from_ref(&file))
            .unwrap();
        let outcome = source_outcome(&file, generation, 110);
        let scope = store
            .begin_provider_file_publication(
                file.provider,
                outcome.observation,
                MATERIAL_FORMAT,
                ProviderFilePublicationKind::Replacement,
                105,
            )
            .unwrap();
        let records = record_ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let mut record = ctx_history_core::HistoryRecord::new(
                    format!("private staged record {index}"),
                    format!("unpublished-token-{index}"),
                    Vec::new(),
                    "note",
                    None,
                );
                record.id = *id;
                record
            })
            .collect::<Vec<_>>();
        store
            .with_provider_file_publication_writes(&scope, |store| {
                store.insert_record(&records[0])?;
                store.upsert_record(&records[1])?;
                store.upsert_records(&records[2..])?;
                for record in &records {
                    assert_eq!(store.get_record(record.id)?.id, record.id);
                }
                Ok(())
            })
            .unwrap();

        let observer = Store::open(&path).unwrap();
        for id in record_ids {
            assert!(matches!(
                observer.get_record(id),
                Err(StoreError::NotFound(_))
            ));
        }
        assert!(observer.list_records(10).unwrap().is_empty());
        assert!(observer
            .search_records("unpublished-token", 10)
            .unwrap()
            .is_empty());
        drop(scope);
    }

    let reopened = Store::open(&path).unwrap();
    for id in record_ids {
        assert!(matches!(
            reopened.get_record(id),
            Err(StoreError::NotFound(_))
        ));
    }
    assert!(reopened.list_records(10).unwrap().is_empty());
    assert!(reopened
        .search_records("unpublished-token", 10)
        .unwrap()
        .is_empty());
}

#[test]
fn staged_completion_is_bounded_atomic_and_survives_abandon_and_reopen() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let file = source_file(10, 100);
    let generation;
    let completion = ProviderFilePublicationCompletion {
        version: 1,
        payload: json!({"summary": {"imported": 3}, "checkpoint": "opaque"}),
    };
    {
        let store = Store::open(&path).unwrap();
        generation = store
            .allocate_source_import_inventory_generation(file.provider, &file.source_root)
            .unwrap();
        store
            .upsert_source_import_files(generation, std::slice::from_ref(&file))
            .unwrap();
        let scope = store
            .begin_provider_file_publication(
                file.provider,
                source_outcome(&file, generation, 110).observation,
                MATERIAL_FORMAT,
                ProviderFilePublicationKind::Replacement,
                105,
            )
            .unwrap();
        assert_eq!(
            store.provider_file_publication_phase(&scope).unwrap(),
            ProviderFilePublicationPhase::Importing
        );
        assert!(matches!(
            store
                .stage_provider_file_publication_completion(
                    &scope,
                    &ProviderFilePublicationCompletion {
                        version: 0,
                        payload: json!({}),
                    },
                )
                .unwrap_err(),
            StoreError::InvalidProviderFilePublicationScope
        ));
        assert!(matches!(
            store
                .stage_provider_file_publication_completion(
                    &scope,
                    &ProviderFilePublicationCompletion {
                        version: 1,
                        payload: json!("x".repeat(PROVIDER_FILE_PUBLICATION_COMPLETION_MAX_BYTES)),
                    },
                )
                .unwrap_err(),
            StoreError::InvalidProviderFilePublicationScope
        ));
        store.inject_provider_file_fault(ProviderFileFaultPoint::CompletionBeforeCommit);
        assert!(matches!(
            store
                .stage_provider_file_publication_completion(&scope, &completion)
                .unwrap_err(),
            StoreError::ProviderFileStaging
        ));
        assert_eq!(
            store
                .load_provider_file_publication_completion(&scope)
                .unwrap(),
            None
        );
        store
            .stage_provider_file_publication_completion(&scope, &completion)
            .unwrap();
        store
            .stage_provider_file_publication_completion(&scope, &completion)
            .unwrap();
        assert_eq!(
            store.provider_file_publication_phase(&scope).unwrap(),
            ProviderFilePublicationPhase::ReadyToFinalize
        );
        assert!(matches!(
            store
                .with_provider_file_publication_writes(&scope, |_| Ok(()))
                .unwrap_err(),
            StoreError::InvalidProviderFilePublicationScope
        ));
        store.abandon_provider_file_publication(scope).unwrap();
    }

    let reopened = Store::open(&path).unwrap();
    let scope = reopened
        .begin_provider_file_publication(
            file.provider,
            source_outcome(&file, generation, 120).observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            115,
        )
        .unwrap();
    assert_eq!(
        reopened
            .load_provider_file_publication_completion(&scope)
            .unwrap(),
        Some(completion)
    );
    assert_eq!(
        reopened.provider_file_publication_phase(&scope).unwrap(),
        ProviderFilePublicationPhase::ReadyToFinalize
    );
    assert!(matches!(
        reopened.abort_provider_file_publication(scope).unwrap(),
        std::ops::ControlFlow::Continue(None)
    ));
    assert!(!reopened.has_pending_provider_file_publications().unwrap());
}
