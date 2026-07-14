#[test]
fn catalog_retain_completion_preserves_every_legacy_import_cursor_field() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let mut catalog = CatalogSession {
        provider: CaptureProvider::Codex,
        source_format: "codex_session_jsonl".to_owned(),
        source_root: "/history/codex".to_owned(),
        source_path: "/history/codex/session.jsonl".to_owned(),
        external_session_id: Some("retain-session".to_owned()),
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
        import_revision: 1,
        inventory_generation: first_generation,
        file_sha256: Some("legacy-hash"),
        event_count: Some(4),
        indexed_at_ms: 110,
    };
    store
        .upsert_provider_file_checkpoint(
            ProviderFileImportOutcome {
                provider: catalog.provider,
                observation: ProviderFileInventoryObservation::Catalog {
                    source_format: &catalog.source_format,
                    update: first_update,
                },
                status: CatalogIndexedStatus::Indexed,
                error: None,
            },
            &checkpoint_for_catalog(&catalog, 10, 4, 110),
        )
        .unwrap();
    let legacy_before = catalog_legacy_cursor(&store, &catalog.source_path);

    catalog.file_size_bytes = 15;
    catalog.file_modified_at_ms = 120;
    catalog.cataloged_at_ms = 121;
    let second_generation = store
        .allocate_catalog_inventory_generation(catalog.provider, &catalog.source_root)
        .unwrap();
    store
        .upsert_catalog_sessions(second_generation, std::slice::from_ref(&catalog))
        .unwrap();
    let second_update = CatalogSourceIndexUpdate {
        source_root: &catalog.source_root,
        source_path: &catalog.source_path,
        file_size_bytes: 15,
        file_modified_at_ms: 120,
        import_revision: 1,
        inventory_generation: second_generation,
        file_sha256: Some("must-not-advance"),
        event_count: Some(99),
        indexed_at_ms: 130,
    };
    store
        .complete_provider_file_observation_retaining_checkpoint(ProviderFileImportOutcome {
            provider: catalog.provider,
            observation: ProviderFileInventoryObservation::Catalog {
                source_format: &catalog.source_format,
                update: second_update,
            },
            status: CatalogIndexedStatus::Indexed,
            error: None,
        })
        .unwrap();
    assert_eq!(
        catalog_legacy_cursor(&store, &catalog.source_path),
        legacy_before
    );
    let indexed: (i64, i64, i64) = store
        .conn
        .query_row(
            "SELECT indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms FROM catalog_sessions WHERE source_path = ?1",
            params![&catalog.source_path],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(indexed, (130, 15, 120));
}
#[test]
fn crashed_replacement_keeps_old_rows_and_checkpoint_without_main_staging() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let old_event = Uuid::from_u128(80);
    let new_event = Uuid::from_u128(81);
    let workspace = Uuid::from_u128(83);
    let source_less_change = Uuid::from_u128(84);
    let old_checkpoint = {
        let store = Store::open(&path).unwrap();
        let original = source_file(10, 100);
        let generation = store
            .allocate_source_import_inventory_generation(original.provider, &original.source_root)
            .unwrap();
        store
            .upsert_source_import_files(generation, std::slice::from_ref(&original))
            .unwrap();
        let old_checkpoint = checkpoint(10, 3, "unix:2049:old", 110);
        store
            .upsert_provider_file_checkpoint(
                source_outcome(&original, generation, 110),
                &old_checkpoint,
            )
            .unwrap();
        let source = Uuid::from_u128(82);
        insert_capture_source(&store, source, PATH_A, "crash-session");
        insert_raw_event(&store, old_event, 80, source, "old");
        store
            .conn
            .execute(
                "INSERT INTO vcs_workspaces (id, kind, root_path, repo_fingerprint, created_at_ms, updated_at_ms, source_id) VALUES (?1, 'git', '/crash', 'crash-repo', 1, 1, ?2)",
                params![workspace.to_string(), source.to_string()],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO vcs_changes (id, vcs_workspace_id, kind, change_id, created_at_ms, updated_at_ms) VALUES (?1, ?2, 'git_commit', 'source-less-crash', 1, 1)",
                params![source_less_change.to_string(), workspace.to_string()],
            )
            .unwrap();
        store.rebuild_search_projection().unwrap();
        store
            .refresh_event_embedding_document_count_cache()
            .unwrap();

        let rewritten = source_file(20, 120);
        let replacement_generation = store
            .allocate_source_import_inventory_generation(rewritten.provider, &rewritten.source_root)
            .unwrap();
        store
            .upsert_source_import_files(replacement_generation, std::slice::from_ref(&rewritten))
            .unwrap();
        let outcome = source_outcome(&rewritten, replacement_generation, 130);
        let scope = store
            .begin_provider_file_publication(
                rewritten.provider,
                outcome.observation,
                MATERIAL_FORMAT,
                ProviderFilePublicationKind::Replacement,
                120,
            )
            .unwrap();
        assert!(scope.tracks_prior_material);
        insert_raw_event(&store, new_event, 81, source, "new");
        store
            .track_provider_file_publication_event(new_event)
            .unwrap();
        assert_eq!(staged_seen_count(&store), 1);
        let observer = Store::open(&path).unwrap();
        assert!(observer.list_events().unwrap().is_empty());
        assert!(observer.list_vcs_changes().unwrap().is_empty());
        assert!(observer.search_event_hits("old", 10).unwrap().is_empty());
        let archive = observer.export_archive().unwrap();
        assert!(archive.events.is_empty());
        assert!(archive.vcs_changes.is_empty());
        assert_eq!(
            observer.cached_event_embedding_document_count().unwrap(),
            None
        );
        assert_eq!(observer.count_event_embedding_documents().unwrap(), 0);
        assert_eq!(observer.count_event_embedding_documents_exact().unwrap(), 0);
        assert!(observer
            .event_embedding_documents_by_ids(&[old_event])
            .unwrap()
            .is_empty());
        old_checkpoint
    };

    let store = Store::open(&path).unwrap();
    assert!(row_exists(&store, "events", old_event));
    assert!(row_exists(&store, "events", new_event));
    assert_eq!(
        store
            .provider_file_checkpoint(old_checkpoint.key())
            .unwrap()
            .unwrap(),
        old_checkpoint
    );
    assert!(store.has_pending_provider_file_publications().unwrap());
    assert!(store.list_events().unwrap().is_empty());
    assert!(store.list_vcs_changes().unwrap().is_empty());
    assert!(store.export_archive().unwrap().vcs_changes.is_empty());
    assert!(store.search_event_hits("old", 10).unwrap().is_empty());
    assert!(store
        .event_embedding_documents_by_ids(&[old_event])
        .unwrap()
        .is_empty());
    assert!(store.provider_file_publication.borrow().is_none());
    assert!(!main_table_exists(&store, "provider_file_publication_seen"));

    let rewritten = source_file(20, 120);
    let generation = store
        .allocate_source_import_inventory_generation(rewritten.provider, &rewritten.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&rewritten))
        .unwrap();
    let outcome = source_outcome(&rewritten, generation, 140);
    let scope = store
        .begin_provider_file_publication(
            rewritten.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            130,
        )
        .unwrap();
    store
        .track_provider_file_publication_event(new_event)
        .unwrap();
    reconcile_all(&store, &scope, 1);
    let replacement_checkpoint = checkpoint(20, 4, "unix:2049:old", 140);
    store
        .finalize_provider_file_publication(
            scope,
            outcome,
            ProviderFilePublicationCommit::Replacement(Some(&replacement_checkpoint)),
        )
        .unwrap();
    store.rebuild_search_projection().unwrap();

    assert!(!store.has_pending_provider_file_publications().unwrap());
    assert_eq!(store.list_events().unwrap().len(), 1);
    assert!(!store.search_event_hits("new", 10).unwrap().is_empty());
}
#[test]
fn replacement_scope_fails_if_same_generation_observation_changes() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let original = source_file(20, 100);
    let generation = store
        .allocate_source_import_inventory_generation(original.provider, &original.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&original))
        .unwrap();
    let source = Uuid::from_u128(85);
    let old_event = Uuid::from_u128(86);
    insert_capture_source(&store, source, PATH_A, "changed-observation");
    insert_raw_event(&store, old_event, 86, source, "old");
    let original_outcome = source_outcome(&original, generation, 110);
    let scope = store
        .begin_provider_file_publication(
            original.provider,
            original_outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            105,
        )
        .unwrap();

    let changed = source_file(30, 120);
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&changed))
        .unwrap();
    let error = store
        .finalize_provider_file_publication(
            scope,
            original_outcome,
            ProviderFilePublicationCommit::Replacement(None),
        )
        .unwrap_err();

    assert!(matches!(
        error,
        StoreError::ProviderFileObservationChanged { .. }
    ));
    assert!(row_exists(&store, "events", old_event));
    assert!(store.provider_file_publication.borrow().is_none());
}
#[test]
fn transaction_boundary_faults_roll_back_and_restart_converges() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let file = source_file(20, 100);
    let source = Uuid::from_u128(329_100);
    let old_event = Uuid::from_u128(329_101);
    let new_event = Uuid::from_u128(329_102);
    let old_checkpoint = checkpoint(20, 4, "unix:2049:boundary-fault", 105);
    let generation = {
        let store = Store::open(&path).unwrap();
        let generation = store
            .allocate_source_import_inventory_generation(file.provider, &file.source_root)
            .unwrap();
        store
            .upsert_source_import_files(generation, std::slice::from_ref(&file))
            .unwrap();
        let outcome = source_outcome(&file, generation, 105);
        store
            .upsert_provider_file_checkpoint(outcome, &old_checkpoint)
            .unwrap();
        insert_capture_source(&store, source, PATH_A, "transaction-boundary-faults");
        insert_raw_event(&store, old_event, 1, source, "old generation");
        let scope = store
            .begin_provider_file_publication(
                file.provider,
                outcome.observation,
                MATERIAL_FORMAT,
                ProviderFilePublicationKind::Replacement,
                110,
            )
            .unwrap();

        store.inject_provider_file_fault(ProviderFileFaultPoint::PreparationBeforeCommit);
        assert!(matches!(
            store
                .prepare_provider_file_publication_slice(&scope, 1)
                .unwrap_err(),
            StoreError::ProviderFileStaging
        ));
        assert_eq!(staged_prior_source_count(&store), 0);
        assert_eq!(
            store
                .conn
                .query_row(
                    "SELECT preparation_complete, preparation_cursor FROM provider_file_publications",
                    [],
                    |row| Ok((row.get::<_, bool>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .unwrap(),
            (false, None)
        );
        prepare_all(&store, &scope, 1);

        let mut replacement = event_fixture(
            new_event,
            2,
            source,
            "transaction-boundary-faults".to_owned(),
            "new generation",
        );
        replacement.dedupe_key = None;
        store.inject_provider_file_fault(ProviderFileFaultPoint::MutationBeforeCommit);
        assert!(matches!(
            store
                .with_provider_file_publication_writes(&scope, |store| {
                    store.upsert_event(&replacement)
                })
                .unwrap_err(),
            StoreError::ProviderFileStaging
        ));
        assert!(!row_exists(&store, "events", new_event));
        assert_eq!(staged_seen_count(&store), 0);
        assert!(!store
            .conn
            .query_row(
                "SELECT mutation_started FROM provider_file_publications",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap());
        drop(scope);
        generation
    };

    {
        let store = Store::open(&path).unwrap();
        let outcome = source_outcome(&file, generation, 120);
        let scope = store
            .begin_provider_file_publication(
                file.provider,
                outcome.observation,
                MATERIAL_FORMAT,
                ProviderFilePublicationKind::Replacement,
                115,
            )
            .unwrap();
        prepare_all(&store, &scope, 1);
        let mut replacement = event_fixture(
            new_event,
            2,
            source,
            "transaction-boundary-faults".to_owned(),
            "new generation",
        );
        replacement.dedupe_key = None;
        store
            .with_provider_file_publication_writes(&scope, |store| store.upsert_event(&replacement))
            .unwrap();
        reconcile_all(&store, &scope, 1);
        let mut new_checkpoint = old_checkpoint.clone();
        new_checkpoint.updated_at_ms = 125;
        store.inject_provider_file_fault(ProviderFileFaultPoint::FinalizeBeforeCommit);
        assert!(matches!(
            store
                .finalize_provider_file_publication(
                    scope,
                    outcome,
                    ProviderFilePublicationCommit::Replacement(Some(&new_checkpoint)),
                )
                .unwrap_err(),
            StoreError::ProviderFileStaging
        ));
        assert!(store.has_pending_provider_file_publications().unwrap());
        assert_eq!(
            store
                .provider_file_checkpoint(old_checkpoint.key())
                .unwrap(),
            Some(old_checkpoint.clone())
        );
        assert_eq!(store.semantic_replacement_revision().unwrap(), 0);
        assert!(store.list_events().unwrap().is_empty());
    }

    let store = Store::open(&path).unwrap();
    let outcome = source_outcome(&file, generation, 130);
    let scope = store
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Incremental,
            126,
        )
        .unwrap();
    assert_eq!(scope.kind(), ProviderFilePublicationKind::Replacement);
    prepare_all(&store, &scope, 1);
    let mut replacement = event_fixture(
        new_event,
        2,
        source,
        "transaction-boundary-faults".to_owned(),
        "new generation",
    );
    replacement.dedupe_key = None;
    store
        .with_provider_file_publication_writes(&scope, |store| store.upsert_event(&replacement))
        .unwrap();
    reconcile_all(&store, &scope, 1);
    let mut new_checkpoint = old_checkpoint.clone();
    new_checkpoint.updated_at_ms = 130;
    store
        .finalize_provider_file_publication(
            scope,
            outcome,
            ProviderFilePublicationCommit::Replacement(Some(&new_checkpoint)),
        )
        .unwrap();
    assert_eq!(store.list_events().unwrap()[0].id, new_event);
    assert_eq!(
        store
            .provider_file_checkpoint(new_checkpoint.key())
            .unwrap(),
        Some(new_checkpoint)
    );
    assert_eq!(store.semantic_replacement_revision().unwrap(), 1);
}

#[test]
fn replacement_faults_preserve_fences_and_committed_cleanup_is_warning_only() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = source_file(20, 100);
    let generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    let old_checkpoint = checkpoint(20, 4, "unix:2049:fault", 105);
    let outcome = source_outcome(&file, generation, 105);
    store
        .upsert_provider_file_checkpoint(outcome, &old_checkpoint)
        .unwrap();
    let source = Uuid::from_u128(330);
    let event = Uuid::from_u128(331);
    insert_capture_source(&store, source, PATH_A, "fault-session");
    insert_raw_event(&store, event, 1, source, "retained");

    store.inject_provider_file_fault(ProviderFileFaultPoint::BeginAfterStaging);
    let begin_error = store
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            110,
        )
        .unwrap_err();
    assert!(matches!(begin_error, StoreError::ProviderFileStaging));
    assert!(store.provider_file_publication.borrow().is_none());
    assert!(store.has_pending_provider_file_publications().unwrap());

    let scope = store
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            111,
        )
        .unwrap();
    store.track_provider_file_publication_event(event).unwrap();
    reconcile_all(&store, &scope, 1);
    let mut new_checkpoint = old_checkpoint.clone();
    new_checkpoint.updated_at_ms = 120;
    store.inject_provider_file_fault(ProviderFileFaultPoint::FinalizeBeforeCommit);
    let finalize_error = store
        .finalize_provider_file_publication(
            scope,
            outcome,
            ProviderFilePublicationCommit::Replacement(Some(&new_checkpoint)),
        )
        .unwrap_err();
    assert!(matches!(finalize_error, StoreError::ProviderFileStaging));
    assert!(store.has_pending_provider_file_publications().unwrap());
    assert_eq!(
        store
            .provider_file_checkpoint(old_checkpoint.key())
            .unwrap()
            .unwrap(),
        old_checkpoint
    );
    assert!(store.provider_file_publication.borrow().is_none());

    let retry = store
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            121,
        )
        .unwrap();
    store.track_provider_file_publication_event(event).unwrap();
    reconcile_all(&store, &retry, 1);
    store.inject_provider_file_fault(ProviderFileFaultPoint::Cleanup);
    let committed = store
        .finalize_provider_file_publication(
            retry,
            outcome,
            ProviderFilePublicationCommit::Replacement(Some(&new_checkpoint)),
        )
        .unwrap();
    assert!(matches!(
        committed.maintenance_warning,
        Some(ProviderFileMaintenanceWarning::StagingCleanupDeferred { .. })
    ));
    assert!(!store.has_pending_provider_file_publications().unwrap());
    assert_eq!(
        store
            .provider_file_checkpoint(new_checkpoint.key())
            .unwrap()
            .unwrap(),
        new_checkpoint
    );
    store.cleanup_abandoned_provider_file_publication().unwrap();
    assert!(store.provider_file_publication.borrow().is_none());

    let abandoned = store
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            130,
        )
        .unwrap();
    store.abandon_provider_file_publication(abandoned).unwrap();
    assert!(store.provider_file_publication.borrow().is_none());
    assert!(store.has_pending_provider_file_publications().unwrap());
}
