#[test]
fn retirement_without_child_material_prepares_in_bounded_slices() {
    const SOURCE_COUNT: u128 = 130;

    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let file = source_file(10, 100);
    {
        let store = Store::open(&path).unwrap();
        let generation = store
            .allocate_source_import_inventory_generation(file.provider, &file.source_root)
            .unwrap();
        store
            .upsert_source_import_files(generation, std::slice::from_ref(&file))
            .unwrap();
        for index in 0..SOURCE_COUNT {
            insert_capture_source(
                &store,
                Uuid::from_u128(66_000 + index),
                PATH_A,
                &format!("retirement-preparation-{index}"),
            );
        }
        let scope = store
            .begin_provider_file_publication(
                file.provider,
                source_outcome(&file, generation, 110).observation,
                MATERIAL_FORMAT,
                ProviderFilePublicationKind::Incremental,
                105,
            )
            .unwrap();
        store
            .conn
            .execute(
                "UPDATE provider_file_publications SET mutation_started = 1 WHERE replacement_id = ?1",
                params![scope.scope_id.to_string()],
            )
            .unwrap();
        assert!(matches!(
            store.abort_provider_file_publication(scope).unwrap(),
            std::ops::ControlFlow::Break(None)
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
    }

    let mut staged = 0;
    let mut completed = false;
    for cycle in 0..64 {
        let store = Store::open(&path).unwrap();
        let scope = store
            .begin_provider_file_publication_retirement(
                file.provider,
                MATERIAL_FORMAT,
                &file.source_root,
                &file.source_path,
                130 + cycle,
            )
            .unwrap()
            .unwrap();
        match store.provider_file_publication_phase(&scope).unwrap() {
            ProviderFilePublicationPhase::Preparing => {
                let progress = store
                    .prepare_provider_file_publication_slice(&scope, 64)
                    .unwrap();
                assert!(progress.rows_processed <= 64, "{progress:?}");
                staged += progress.source_ids_staged;
            }
            ProviderFilePublicationPhase::Reconciling => {
                let progress = store
                    .reconcile_provider_file_publication_slice(&scope, 64)
                    .unwrap();
                assert!(progress.rows_scanned <= 64, "{progress:?}");
            }
            ProviderFilePublicationPhase::ReadyToFinalize => {
                store.retire_provider_file_publication(scope).unwrap();
                completed = true;
                break;
            }
            ProviderFilePublicationPhase::Importing => {
                panic!("retirement entered importer phase")
            }
        }
        assert!(store
            .abandon_provider_file_publication(scope)
            .unwrap()
            .is_none());
    }

    assert_eq!(staged, SOURCE_COUNT as usize);
    assert!(completed);
    let store = Store::open(&path).unwrap();
    assert!(!store.has_pending_provider_file_publications().unwrap());
}

#[test]
fn completed_empty_retirement_preparation_survives_reopen() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let file = source_file(10, 100);
    {
        let store = Store::open(&path).unwrap();
        let generation = store
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
                ProviderFilePublicationKind::Incremental,
                105,
            )
            .unwrap();
        store
            .conn
            .execute(
                "UPDATE provider_file_publications SET mutation_started = 1 WHERE replacement_id = ?1",
                params![scope.scope_id.to_string()],
            )
            .unwrap();
        assert!(matches!(
            store.abort_provider_file_publication(scope).unwrap(),
            std::ops::ControlFlow::Break(None)
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
    }

    {
        let store = Store::open(&path).unwrap();
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
        let progress = store
            .prepare_provider_file_publication_slice(&retirement, 1)
            .unwrap();
        assert_eq!(progress.source_ids_staged, 0);
        assert!(progress.complete);
        assert_eq!(
            store.provider_file_publication_phase(&retirement).unwrap(),
            ProviderFilePublicationPhase::ReadyToFinalize
        );
        store.abandon_provider_file_publication(retirement).unwrap();
    }

    let store = Store::open(&path).unwrap();
    let retirement = store
        .begin_provider_file_publication_retirement(
            file.provider,
            MATERIAL_FORMAT,
            &file.source_root,
            &file.source_path,
            130,
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        store.provider_file_publication_phase(&retirement).unwrap(),
        ProviderFilePublicationPhase::ReadyToFinalize
    );
    store.retire_provider_file_publication(retirement).unwrap();
    assert!(!store.has_pending_provider_file_publications().unwrap());
    assert_eq!(table_row_count(&store, "source_import_files"), 0);
}

#[test]
fn retirement_reconciliation_discards_seen_candidates_across_reopen_cycles() {
    const EVENT_COUNT: u128 = 130;

    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let file = source_file(10, 100);
    let source = Uuid::from_u128(67_000);
    {
        let store = Store::open(&path).unwrap();
        let generation = store
            .allocate_source_import_inventory_generation(file.provider, &file.source_root)
            .unwrap();
        store
            .upsert_source_import_files(generation, std::slice::from_ref(&file))
            .unwrap();
        insert_capture_source(&store, source, PATH_A, "retirement-retained");
        for index in 0..EVENT_COUNT {
            insert_raw_event(
                &store,
                Uuid::from_u128(67_100 + index),
                index as i64,
                source,
                &format!("retained event {index}"),
            );
        }
        let scope = store
            .begin_provider_file_publication(
                file.provider,
                source_outcome(&file, generation, 110).observation,
                MATERIAL_FORMAT,
                ProviderFilePublicationKind::Replacement,
                105,
            )
            .unwrap();
        prepare_all(&store, &scope, 64);
        for index in 0..EVENT_COUNT {
            store
                .track_provider_file_publication_event(Uuid::from_u128(67_100 + index))
                .unwrap();
        }
        let mut mutation = event_fixture(
            Uuid::from_u128(67_500),
            500,
            source,
            "retirement-retained".to_owned(),
            "retained mutation",
        );
        mutation.dedupe_key = None;
        store
            .with_provider_file_publication_writes(&scope, |store| store.upsert_event(&mutation))
            .unwrap();
        assert!(matches!(
            store.abort_provider_file_publication(scope).unwrap(),
            std::ops::ControlFlow::Break(None)
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
    }

    let mut completed = false;
    let mut cycles = 0;
    for cycle in 0..32 {
        let store = Store::open(&path).unwrap();
        let scope = store
            .begin_provider_file_publication_retirement(
                file.provider,
                MATERIAL_FORMAT,
                &file.source_root,
                &file.source_path,
                130 + cycle,
            )
            .unwrap()
            .unwrap();
        if cycle == 0 {
            let staged_events: i64 = store
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM provider_file_publication_seen \
                     WHERE replacement_id = ?1 AND entity_kind = 'event'",
                    params![scope.scope_id.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(staged_events, EVENT_COUNT as i64 + 1);
        }
        let preparation = store
            .prepare_provider_file_publication_slice(&scope, 64)
            .unwrap();
        assert!(preparation.rows_processed <= 64);
        if !preparation.complete {
            cycles += 1;
            store.abandon_provider_file_publication(scope).unwrap();
            continue;
        }
        let progress = store
            .reconcile_provider_file_publication_slice(&scope, 64)
            .unwrap();
        cycles += 1;
        if progress.complete {
            store.retire_provider_file_publication(scope).unwrap();
            completed = true;
            break;
        }
        assert!(store
            .abandon_provider_file_publication(scope)
            .unwrap()
            .is_none());
    }

    assert!(completed, "bounded retirement did not converge");
    assert!(cycles >= 3);
    let store = Store::open(&path).unwrap();
    assert_eq!(table_row_count(&store, "events"), 0);
    assert!(!store.has_pending_provider_file_publications().unwrap());
}

#[test]
fn tombstoned_unmutated_marker_does_not_block_ordinary_owner_entity_writes() {
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
    let source = Uuid::from_u128(65_100);
    insert_capture_source(&store, source, PATH_A, "tombstone-ordinary-write");
    let scope = store
        .begin_provider_file_publication(
            file.provider,
            source_outcome(&file, generation, 110).observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            105,
        )
        .unwrap();
    drop(scope);

    let observer = Store::open(&path).unwrap();
    let tombstone_generation = observer
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    observer
        .mark_source_import_missing_paths_stale(
            file.provider,
            &file.source_root,
            &[],
            120,
            tombstone_generation,
        )
        .unwrap();
    let session = Uuid::from_u128(65_101);
    observer
        .upsert_session(&session_fixture(
            session,
            source,
            "tombstone-ordinary-write",
        ))
        .unwrap();
    assert_eq!(observer.get_session(session).unwrap().id, session);
}

#[test]
fn process_crash_tombstone_retirement_restarts_and_is_idempotent() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let file = source_file(20, 100);
    let source = Uuid::from_u128(65_300);
    let first_event = Uuid::from_u128(65_301);
    let second_event = Uuid::from_u128(65_302);
    let old_checkpoint = checkpoint(20, 4, "unix:2049:tombstone-retirement", 105);
    let generation = {
        let store = Store::open(&path).unwrap();
        let generation = store
            .allocate_source_import_inventory_generation(file.provider, &file.source_root)
            .unwrap();
        store
            .upsert_source_import_files(generation, std::slice::from_ref(&file))
            .unwrap();
        store
            .upsert_provider_file_checkpoint(
                source_outcome(&file, generation, 105),
                &old_checkpoint,
            )
            .unwrap();
        insert_capture_source(&store, source, PATH_A, "tombstone-retirement");
        insert_raw_event(&store, first_event, 1, source, "first old event");
        insert_raw_event(&store, second_event, 2, source, "second old event");
        generation
    };

    let ready = temp.path().join("partial-crash-ready");
    let mut child = spawn_provider_file_helper(
        "partial-crash",
        &path,
        Some(&ready),
        None,
        Some((generation, first_event)),
    );
    wait_for_path(&ready);
    assert_eq!(child.wait().unwrap().code(), Some(29));

    {
        let observer = Store::open(&path).unwrap();
        assert!(observer.has_pending_provider_file_publications().unwrap());
        assert!(observer.list_events().unwrap().is_empty());
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
        assert!(observer.has_pending_provider_file_publications().unwrap());
        assert!(observer.list_events().unwrap().is_empty());
    }

    {
        let mut adopter = Store::open(&path).unwrap();
        let retirement = adopter
            .begin_provider_file_publication_retirement(
                file.provider,
                MATERIAL_FORMAT,
                ROOT,
                PATH_A,
                140,
            )
            .unwrap()
            .unwrap();
        assert!(retirement.retires_observation);
        assert!(matches!(
            adopter
                .with_provider_file_publication_writes(&retirement, |_| Ok(()))
                .unwrap_err(),
            StoreError::InvalidProviderFilePublicationScope
        ));
        assert!(matches!(
            adopter
                .with_provider_file_publication_writes_mut::<(), StoreError>(
                    &retirement,
                    |_| Ok(()),
                )
                .unwrap_err(),
            StoreError::InvalidProviderFilePublicationScope
        ));
        assert!(adopter.provider_file_write_scope.get().is_none());
        prepare_all(&adopter, &retirement, 1);
        adopter
            .reconcile_provider_file_publication_slice(&retirement, 1)
            .unwrap();
        drop(retirement);
    }

    let restarted = Store::open(&path).unwrap();
    let retirement = restarted
        .begin_provider_file_publication_retirement(
            file.provider,
            MATERIAL_FORMAT,
            ROOT,
            PATH_A,
            150,
        )
        .unwrap()
        .unwrap();
    prepare_all(&restarted, &retirement, 1);
    reconcile_all(&restarted, &retirement, 1);
    let retired = restarted
        .retire_provider_file_publication(retirement)
        .unwrap();
    assert!(retired.reconciliation.events >= 1);
    assert!(!restarted.has_pending_provider_file_publications().unwrap());
    assert_eq!(table_row_count(&restarted, "events"), 0);
    assert_eq!(
        restarted
            .conn
            .query_row(
                "SELECT COUNT(*) FROM source_import_files WHERE source_path = ?1",
                params![PATH_A],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        restarted
            .provider_file_checkpoint(old_checkpoint.key())
            .unwrap(),
        None
    );
    assert_eq!(restarted.semantic_replacement_revision().unwrap(), 1);
    assert!(restarted
        .begin_provider_file_publication_retirement(
            file.provider,
            MATERIAL_FORMAT,
            ROOT,
            PATH_A,
            160,
        )
        .unwrap()
        .is_none());
}

#[test]
fn mutated_publication_with_permanently_missing_observation_can_retire() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let file = source_file(20, 100);
    let source = Uuid::from_u128(65_400);
    let event_id = Uuid::from_u128(65_401);
    {
        let store = Store::open(&path).unwrap();
        let generation = store
            .allocate_source_import_inventory_generation(file.provider, &file.source_root)
            .unwrap();
        store
            .upsert_source_import_files(generation, std::slice::from_ref(&file))
            .unwrap();
        insert_capture_source(&store, source, PATH_A, "missing-observation-retirement");
        let mut event = event_fixture(
            event_id,
            1,
            source,
            "missing-observation-retirement".to_owned(),
            "old generation",
        );
        event.dedupe_key = None;
        let scope = store
            .begin_provider_file_publication(
                file.provider,
                source_outcome(&file, generation, 110).observation,
                MATERIAL_FORMAT,
                ProviderFilePublicationKind::Replacement,
                105,
            )
            .unwrap();
        prepare_all(&store, &scope, 1);
        store
            .with_provider_file_publication_writes(&scope, |store| store.upsert_event(&event))
            .unwrap();
        drop(scope);
    }

    let store = Store::open(&path).unwrap();
    store
        .conn
        .execute(
            "DELETE FROM source_import_files WHERE source_path = ?1",
            params![PATH_A],
        )
        .unwrap();
    let retirement = store
        .begin_provider_file_publication_retirement(
            file.provider,
            MATERIAL_FORMAT,
            ROOT,
            PATH_A,
            120,
        )
        .unwrap()
        .unwrap();
    prepare_all(&store, &retirement, 1);
    reconcile_all(&store, &retirement, 1);
    store.retire_provider_file_publication(retirement).unwrap();
    assert!(!row_exists(&store, "events", event_id));
    assert!(!store.has_pending_provider_file_publications().unwrap());
}

#[test]
fn newer_generation_reclaims_retirement_marker_through_fresh_import() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let file = source_file(20, 100);
    let source = Uuid::from_u128(65_500);
    let event_id = Uuid::from_u128(65_501);
    let generation = {
        let store = Store::open(&path).unwrap();
        let generation = store
            .allocate_source_import_inventory_generation(file.provider, &file.source_root)
            .unwrap();
        store
            .upsert_source_import_files(generation, std::slice::from_ref(&file))
            .unwrap();
        insert_capture_source(&store, source, PATH_A, "retirement-revival");
        let scope = store
            .begin_provider_file_publication(
                file.provider,
                source_outcome(&file, generation, 110).observation,
                MATERIAL_FORMAT,
                ProviderFilePublicationKind::Replacement,
                105,
            )
            .unwrap();
        prepare_all(&store, &scope, 1);
        let mut event = event_fixture(
            event_id,
            1,
            source,
            "retirement-revival".to_owned(),
            "old generation",
        );
        event.dedupe_key = None;
        store
            .with_provider_file_publication_writes(&scope, |store| store.upsert_event(&event))
            .unwrap();
        drop(scope);
        generation
    };
    {
        let observer = Store::open(&path).unwrap();
        let tombstone_generation = observer
            .allocate_source_import_inventory_generation(file.provider, &file.source_root)
            .unwrap();
        observer
            .mark_source_import_missing_paths_stale(
                file.provider,
                &file.source_root,
                &[],
                120,
                tombstone_generation,
            )
            .unwrap();
    }

    let retiring = Store::open(&path).unwrap();
    let retirement = retiring
        .begin_provider_file_publication_retirement(
            file.provider,
            MATERIAL_FORMAT,
            ROOT,
            PATH_A,
            130,
        )
        .unwrap()
        .unwrap();
    prepare_all(&retiring, &retirement, 1);
    reconcile_all(&retiring, &retirement, 1);

    let reviver = Store::open(&path).unwrap();
    let revived_generation = reviver
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    reviver
        .upsert_source_import_files(revived_generation, std::slice::from_ref(&file))
        .unwrap();
    assert!(matches!(
        retiring.retire_provider_file_publication(retirement),
        Err(StoreError::ProviderFileObservationChanged { .. })
    ));
    assert!(reviver.has_pending_provider_file_publications().unwrap());
    assert!(reviver.list_events().unwrap().is_empty());

    let outcome = source_outcome(&file, revived_generation, 140);
    let adopted = reviver
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            135,
        )
        .unwrap();
    assert_eq!(adopted.kind(), ProviderFilePublicationKind::Replacement);
    prepare_all(&reviver, &adopted, 1);
    reconcile_all(&reviver, &adopted, 1);
    reviver
        .finalize_provider_file_publication(
            adopted,
            outcome,
            ProviderFilePublicationCommit::Replacement(None),
        )
        .unwrap();
    assert!(!reviver.has_pending_provider_file_publications().unwrap());
    assert_eq!(table_row_count(&reviver, "events"), 0);
    assert!(generation < revived_generation);
}

#[test]
fn process_exit_during_retirement_finalization_rolls_back_and_restarts() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let file = source_file(20, 100);
    let source = Uuid::from_u128(65_600);
    let event_id = Uuid::from_u128(65_601);
    {
        let store = Store::open(&path).unwrap();
        let generation = store
            .allocate_source_import_inventory_generation(file.provider, &file.source_root)
            .unwrap();
        store
            .upsert_source_import_files(generation, std::slice::from_ref(&file))
            .unwrap();
        insert_capture_source(&store, source, PATH_A, "retirement-process-exit");
        let scope = store
            .begin_provider_file_publication(
                file.provider,
                source_outcome(&file, generation, 110).observation,
                MATERIAL_FORMAT,
                ProviderFilePublicationKind::Replacement,
                105,
            )
            .unwrap();
        prepare_all(&store, &scope, 1);
        let mut event = event_fixture(
            event_id,
            1,
            source,
            "retirement-process-exit".to_owned(),
            "old generation",
        );
        event.dedupe_key = None;
        store
            .with_provider_file_publication_writes(&scope, |store| store.upsert_event(&event))
            .unwrap();
        drop(scope);
    }
    {
        let observer = Store::open(&path).unwrap();
        let generation = observer
            .allocate_source_import_inventory_generation(file.provider, &file.source_root)
            .unwrap();
        observer
            .mark_source_import_missing_paths_stale(
                file.provider,
                &file.source_root,
                &[],
                120,
                generation,
            )
            .unwrap();
    }

    let status = spawn_provider_file_helper("retirement-finalize-crash", &path, None, None, None)
        .wait()
        .unwrap();
    assert_eq!(status.code(), Some(37));

    let restarted = Store::open(&path).unwrap();
    assert!(restarted.has_pending_provider_file_publications().unwrap());
    assert_eq!(restarted.semantic_replacement_revision().unwrap(), 0);
    assert!(restarted
        .conn
        .query_row(
            "SELECT is_stale FROM source_import_files WHERE source_path = ?1",
            params![PATH_A],
            |row| row.get::<_, bool>(0),
        )
        .unwrap());
    let retirement = restarted
        .begin_provider_file_publication_retirement(
            file.provider,
            MATERIAL_FORMAT,
            ROOT,
            PATH_A,
            170,
        )
        .unwrap()
        .unwrap();
    prepare_all(&restarted, &retirement, 1);
    reconcile_all(&restarted, &retirement, 1);
    restarted
        .retire_provider_file_publication(retirement)
        .unwrap();
    assert!(!restarted.has_pending_provider_file_publications().unwrap());
    assert_eq!(table_row_count(&restarted, "events"), 0);
    assert_eq!(restarted.semantic_replacement_revision().unwrap(), 1);
}

#[test]
fn superseded_unmutated_marker_does_not_block_ordinary_owner_entity_writes() {
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
    let source = Uuid::from_u128(65_200);
    insert_capture_source(&store, source, PATH_A, "superseded-ordinary-write");
    let scope = store
        .begin_provider_file_publication(
            file.provider,
            source_outcome(&file, generation, 110).observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            105,
        )
        .unwrap();
    drop(scope);

    let observer = Store::open(&path).unwrap();
    let superseding = source_file(20, 120);
    let superseding_generation = observer
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    observer
        .upsert_source_import_files(superseding_generation, &[superseding])
        .unwrap();
    let session = Uuid::from_u128(65_201);
    observer
        .upsert_session(&session_fixture(
            session,
            source,
            "superseded-ordinary-write",
        ))
        .unwrap();
    assert_eq!(observer.get_session(session).unwrap().id, session);
}
