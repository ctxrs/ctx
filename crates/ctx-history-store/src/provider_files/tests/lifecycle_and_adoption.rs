#[test]
fn first_owner_lock_blocks_other_store_and_stale_marker_is_adopted() {
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
    let source = Uuid::from_u128(60);
    insert_capture_source(&store, source, PATH_A, "first-owner");
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
    assert!(!scope.tracks_prior_material);

    let lock_path = store
        .provider_file_publication
        .borrow()
        .as_ref()
        .unwrap()
        ._owner_lock_path
        .clone();
    assert!(lock_path.is_file());
    assert!(!lock_path.to_string_lossy().contains(PATH_A));
    assert_eq!(lock_path.file_stem().unwrap().to_string_lossy().len(), 64);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(lock_path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&lock_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    let observer = Store::open(&path).unwrap();
    let busy = observer
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            106,
        )
        .unwrap_err();
    assert!(matches!(
        busy,
        StoreError::ProviderFileReplacementBusy { .. }
    ));
    let mut blocked_event = event_fixture(
        Uuid::from_u128(61),
        1,
        source,
        "first-owner-event".to_owned(),
        "blocked",
    );
    blocked_event.dedupe_key = None;
    assert!(matches!(
        observer.upsert_event(&blocked_event).unwrap_err(),
        StoreError::ProviderFileReplacementBusy { .. }
    ));
    assert!(matches!(
        observer.get_capture_source(source),
        Err(StoreError::NotFound(id)) if id == source
    ));
    assert_eq!(
        store
            .provider_file_publication_capture_source(&scope, source)
            .unwrap()
            .id,
        source
    );

    let changed = source_file(20, 120);
    let changed_generation = observer
        .allocate_source_import_inventory_generation(changed.provider, &changed.source_root)
        .unwrap();
    observer
        .upsert_source_import_files(changed_generation, std::slice::from_ref(&changed))
        .unwrap();
    assert!(!observer.has_pending_provider_file_publications().unwrap());
    assert_eq!(observer.get_capture_source(source).unwrap().id, source);
    assert!(matches!(
        store
            .with_provider_file_publication_writes(&scope, |store| {
                store.upsert_event(&blocked_event)
            })
            .unwrap_err(),
        StoreError::ProviderFileObservationChanged { .. }
    ));

    store.abandon_provider_file_publication(scope).unwrap();
    let changed_outcome = source_outcome(&changed, changed_generation, 130);
    let adopted = observer
        .begin_provider_file_publication(
            changed.provider,
            changed_outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            125,
        )
        .unwrap();
    assert!(observer.has_pending_provider_file_publications().unwrap());
    assert!(matches!(
        observer.get_capture_source(source),
        Err(StoreError::NotFound(id)) if id == source
    ));
    observer
        .finalize_provider_file_publication(
            adopted,
            changed_outcome,
            ProviderFilePublicationCommit::Replacement(None),
        )
        .unwrap();
    assert!(!observer.has_pending_provider_file_publications().unwrap());
    assert_eq!(observer.get_capture_source(source).unwrap().id, source);
}

#[test]
fn dropping_publication_scope_releases_owner_lock_for_durable_marker_adoption() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let first = Store::open(&path).unwrap();
    let second = Store::open(&path).unwrap();
    let file = source_file(10, 100);
    let generation = first
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    first
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    let outcome = source_outcome(&file, generation, 110);
    let abandoned = first
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            105,
        )
        .unwrap();
    drop(abandoned);

    let adopted = second
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            106,
        )
        .unwrap();
    assert!(second.has_pending_provider_file_publications().unwrap());
    assert!(matches!(
        first
            .begin_provider_file_publication(
                file.provider,
                outcome.observation,
                MATERIAL_FORMAT,
                ProviderFilePublicationKind::Replacement,
                107,
            )
            .unwrap_err(),
        StoreError::ProviderFileReplacementBusy { .. }
    ));

    second.abandon_provider_file_publication(adopted).unwrap();
    let readopted = first
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            108,
        )
        .unwrap();
    first.abandon_provider_file_publication(readopted).unwrap();
}

#[test]
fn abort_publication_discards_only_unmutated_markers_and_releases_the_scope() {
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
    let unmutated = store
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Incremental,
            105,
        )
        .unwrap();
    assert!(matches!(
        store.abort_provider_file_publication(unmutated).unwrap(),
        std::ops::ControlFlow::Continue(None)
    ));
    assert!(store.provider_file_publication.borrow().is_none());
    assert!(!store.has_pending_provider_file_publications().unwrap());

    let source = Uuid::from_u128(64_050);
    insert_capture_source(&store, source, PATH_A, "abort-after-mutation");
    let mutated = store
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Incremental,
            106,
        )
        .unwrap();
    let event = event_fixture(
        Uuid::from_u128(64_051),
        1,
        source,
        "abort-after-mutation".to_owned(),
        "durably fenced",
    );
    store
        .with_provider_file_publication_writes(&mutated, |store| store.upsert_event(&event))
        .unwrap();
    assert!(matches!(
        store.abort_provider_file_publication(mutated).unwrap(),
        std::ops::ControlFlow::Break(None)
    ));
    assert!(store.provider_file_publication.borrow().is_none());
    assert!(store.has_pending_provider_file_publications().unwrap());
}

#[test]
fn abort_outcome_surfaces_cleanup_warning_without_losing_recovery_state() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = source_file(10, 100);
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
    let source = Uuid::from_u128(64_060);
    insert_capture_source(&store, source, PATH_A, "abort-warning");
    let mut event = event_fixture(
        Uuid::from_u128(64_061),
        1,
        source,
        "abort-warning".to_owned(),
        "durably fenced",
    );
    event.dedupe_key = None;
    store
        .with_provider_file_publication_writes(&scope, |store| store.upsert_event(&event))
        .unwrap();

    store.inject_provider_file_fault(ProviderFileFaultPoint::Cleanup);
    assert!(matches!(
        store.abort_provider_file_publication(scope).unwrap(),
        std::ops::ControlFlow::Break(Some(
            ProviderFileMaintenanceWarning::StagingCleanupDeferred { .. }
        ))
    ));
    assert!(store.has_pending_provider_file_publications().unwrap());
    store.cleanup_abandoned_provider_file_publication().unwrap();
    assert!(store.provider_file_publication.borrow().is_none());
}

#[test]
fn invalid_finalize_discards_an_unmutated_marker_before_consuming_the_scope() {
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
            ProviderFilePublicationKind::Incremental,
            105,
        )
        .unwrap();
    assert_eq!(scope.kind(), ProviderFilePublicationKind::Incremental);

    assert!(matches!(
        store
            .finalize_provider_file_publication(
                scope,
                outcome,
                ProviderFilePublicationCommit::Replacement(None),
            )
            .unwrap_err(),
        StoreError::InvalidProviderFilePublicationScope
    ));
    assert!(store.provider_file_publication.borrow().is_none());
    assert!(!store.has_pending_provider_file_publications().unwrap());
}

#[test]
fn mutable_publication_write_scope_resets_after_panic() {
    let temp = tempdir().unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
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
            ProviderFilePublicationKind::Incremental,
            105,
        )
        .unwrap();

    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = store.with_provider_file_publication_writes_mut::<(), StoreError>(&scope, |_| {
            panic!("publication write panic")
        });
    }));
    assert!(panicked.is_err());
    assert!(store.provider_file_write_scope.get().is_none());
    store
        .with_provider_file_publication_writes_mut::<(), StoreError>(&scope, |_| Ok(()))
        .unwrap();
    assert!(matches!(
        store.abort_provider_file_publication(scope).unwrap(),
        std::ops::ControlFlow::Continue(None)
    ));
}

#[test]
fn crashed_mutated_replacement_cannot_be_adopted_or_finalized_as_incremental() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let file = source_file(20, 100);
    let source = Uuid::from_u128(64_100);
    let old_event = Uuid::from_u128(64_101);
    let new_event = Uuid::from_u128(64_102);

    let generation = {
        let first = Store::open(&path).unwrap();
        let generation = first
            .allocate_source_import_inventory_generation(file.provider, &file.source_root)
            .unwrap();
        first
            .upsert_source_import_files(generation, std::slice::from_ref(&file))
            .unwrap();
        insert_capture_source(&first, source, PATH_A, "mutated-replacement-adoption");
        insert_raw_event(&first, old_event, 1, source, "old generation");
        let outcome = source_outcome(&file, generation, 110);
        let scope = first
            .begin_provider_file_publication(
                file.provider,
                outcome.observation,
                MATERIAL_FORMAT,
                ProviderFilePublicationKind::Replacement,
                105,
            )
            .unwrap();
        prepare_all(&first, &scope, 1);
        let mut replacement = event_fixture(
            new_event,
            2,
            source,
            "mutated-replacement-adoption".to_owned(),
            "new generation",
        );
        replacement.dedupe_key = None;
        first
            .with_provider_file_publication_writes(&scope, |store| store.upsert_event(&replacement))
            .unwrap();
        assert_eq!(
            first
                .conn
                .query_row(
                    "SELECT publication_kind || ':' || mutation_started FROM provider_file_publications",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "replacement:1"
        );
        drop(scope);
        generation
    };

    let second = Store::open(&path).unwrap();
    assert!(second.list_events().unwrap().is_empty());
    let outcome = source_outcome(&file, generation, 120);
    let adopted = second
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Incremental,
            115,
        )
        .unwrap();
    assert_eq!(adopted.kind(), ProviderFilePublicationKind::Replacement);
    assert!(adopted.tracks_prior_material);
    let append_checkpoint = checkpoint(20, 4, "unix:2049:mutated-adoption", 120);
    assert!(matches!(
        second
            .finalize_provider_file_publication(
                adopted,
                outcome,
                ProviderFilePublicationCommit::Append(&append_checkpoint),
            )
            .unwrap_err(),
        StoreError::InvalidProviderFilePublicationScope
    ));
    assert!(second.list_events().unwrap().is_empty());

    let adopted = second
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Incremental,
            121,
        )
        .unwrap();
    assert_eq!(adopted.kind(), ProviderFilePublicationKind::Replacement);
    prepare_all(&second, &adopted, 1);
    let mut replacement = event_fixture(
        new_event,
        2,
        source,
        "mutated-replacement-adoption".to_owned(),
        "new generation",
    );
    replacement.dedupe_key = None;
    second
        .with_provider_file_publication_writes(&adopted, |store| store.upsert_event(&replacement))
        .unwrap();
    reconcile_all(&second, &adopted, 1);
    second
        .finalize_provider_file_publication(
            adopted,
            outcome,
            ProviderFilePublicationCommit::Replacement(Some(&append_checkpoint)),
        )
        .unwrap();
    assert_eq!(second.list_events().unwrap()[0].id, new_event);
    assert!(!row_exists(&second, "events", old_event));
}

#[test]
fn changed_observation_adoption_resets_prior_staging_progress() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let original = source_file(20, 100);
    let source = Uuid::from_u128(64_200);
    let replacement_event = Uuid::from_u128(64_201);
    let prior_event = Uuid::from_u128(64_202);

    {
        let store = Store::open(&path).unwrap();
        let generation = store
            .allocate_source_import_inventory_generation(original.provider, &original.source_root)
            .unwrap();
        store
            .upsert_source_import_files(generation, std::slice::from_ref(&original))
            .unwrap();
        insert_capture_source(&store, source, PATH_A, "changed-staging-adoption");
        insert_raw_event(&store, prior_event, 1, source, "prior generation");
        let scope = store
            .begin_provider_file_publication(
                original.provider,
                source_outcome(&original, generation, 110).observation,
                MATERIAL_FORMAT,
                ProviderFilePublicationKind::Replacement,
                105,
            )
            .unwrap();
        prepare_all(&store, &scope, 1);
        let mut event = event_fixture(
            replacement_event,
            2,
            source,
            "changed-staging-adoption".to_owned(),
            "old attempt",
        );
        event.dedupe_key = None;
        store
            .with_provider_file_publication_writes(&scope, |store| store.upsert_event(&event))
            .unwrap();
        let completion = ProviderFilePublicationCompletion {
            version: 1,
            payload: json!({"attempt": "old-observation"}),
        };
        store
            .stage_provider_file_publication_completion(&scope, &completion)
            .unwrap();
        store
            .reconcile_provider_file_publication_slice(&scope, 1)
            .unwrap();
        assert_eq!(staged_seen_count(&store), 1);
        drop(scope);
    }

    let store = Store::open(&path).unwrap();
    let changed = source_file(30, 120);
    let generation = store
        .allocate_source_import_inventory_generation(changed.provider, &changed.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&changed))
        .unwrap();
    let adopted = store
        .begin_provider_file_publication(
            changed.provider,
            source_outcome(&changed, generation, 130).observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            125,
        )
        .unwrap();
    assert_eq!(staged_seen_count(&store), 0);
    assert_eq!(
        store
            .load_provider_file_publication_completion(&adopted)
            .unwrap(),
        None
    );
    let reset_progress: (i64, Option<String>, Option<String>) = store
        .conn
        .query_row(
            "SELECT cleanup_phase, cleanup_source_cursor, cleanup_entity_cursor \
             FROM provider_file_publications",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(reset_progress, (0, None, None));
    assert_eq!(
        store.provider_file_publication_phase(&adopted).unwrap(),
        ProviderFilePublicationPhase::Preparing
    );
    assert!(matches!(
        store.abort_provider_file_publication(adopted).unwrap(),
        std::ops::ControlFlow::Break(None)
    ));
}

#[test]
fn changed_observation_adoption_cleans_first_attempt_material() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let original = source_file(20, 100);
    let source_id = Uuid::from_u128(64_210);
    let event_id = Uuid::from_u128(64_211);
    let record_id = Uuid::from_u128(64_212);

    {
        let store = Store::open(&path).unwrap();
        let generation = store
            .allocate_source_import_inventory_generation(original.provider, &original.source_root)
            .unwrap();
        store
            .upsert_source_import_files(generation, std::slice::from_ref(&original))
            .unwrap();
        let scope = store
            .begin_provider_file_publication(
                original.provider,
                source_outcome(&original, generation, 110).observation,
                MATERIAL_FORMAT,
                ProviderFilePublicationKind::Replacement,
                105,
            )
            .unwrap();
        assert!(!scope.tracks_prior_material());
        let source = capture_source_fixture(source_id, PATH_A, "first-attempt-source");
        let mut event = event_fixture(
            event_id,
            1,
            source_id,
            "first-attempt-event".to_owned(),
            "must be removed after changed adoption",
        );
        event.dedupe_key = None;
        let mut record = ctx_history_core::HistoryRecord::new(
            "first-attempt-record",
            "must be removed after changed adoption",
            Vec::new(),
            "note",
            None,
        );
        record.id = record_id;
        store
            .with_provider_file_publication_writes(&scope, |store| {
                store.upsert_capture_source(&source)?;
                store.upsert_event(&event)?;
                store.upsert_record(&record)
            })
            .unwrap();
        drop(scope);
    }

    let store = Store::open(&path).unwrap();
    let changed = source_file(30, 120);
    let generation = store
        .allocate_source_import_inventory_generation(changed.provider, &changed.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&changed))
        .unwrap();
    let outcome = source_outcome(&changed, generation, 130);
    let adopted = store
        .begin_provider_file_publication(
            changed.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            125,
        )
        .unwrap();
    assert!(adopted.tracks_prior_material());
    assert!(store.list_events().unwrap().is_empty());
    assert!(store.list_records(10).unwrap().is_empty());
    reconcile_all(&store, &adopted, 1);
    store
        .finalize_provider_file_publication(
            adopted,
            outcome,
            ProviderFilePublicationCommit::Replacement(None),
        )
        .unwrap();

    assert!(!row_exists(&store, "events", event_id));
    assert!(!row_exists(&store, "history_records", record_id));
    assert!(!row_exists(&store, "capture_sources", source_id));
}

#[test]
fn retirement_cleans_source_less_record_from_crashed_first_attempt() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let file = source_file(20, 100);
    let record_ids = [
        Uuid::from_u128(64_220),
        Uuid::from_u128(64_221),
        Uuid::from_u128(64_222),
    ];
    let generation = {
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
                ProviderFilePublicationKind::Replacement,
                105,
            )
            .unwrap();
        let records = record_ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let mut record = ctx_history_core::HistoryRecord::new(
                    format!("retired-first-attempt-record-{index}"),
                    "must remain hidden until retirement removes it",
                    Vec::new(),
                    "note",
                    None,
                );
                record.id = *id;
                record
            })
            .collect::<Vec<_>>();
        store
            .with_provider_file_publication_writes(&scope, |store| store.upsert_records(&records))
            .unwrap();
        drop(scope);
        generation
    };

    let store = Store::open(&path).unwrap();
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
    assert!(retirement.tracks_prior_material());
    assert!(store.list_records(10).unwrap().is_empty());
    prepare_all(&store, &retirement, 1);
    let first_slice = store
        .reconcile_provider_file_publication_slice(&retirement, 1)
        .unwrap();
    assert_eq!(first_slice.rows_scanned, 1);
    assert!(!first_slice.complete);
    store.abandon_provider_file_publication(retirement).unwrap();
    drop(store);

    let reopened = Store::open(&path).unwrap();
    let retirement = reopened
        .begin_provider_file_publication_retirement(
            file.provider,
            MATERIAL_FORMAT,
            &file.source_root,
            &file.source_path,
            130,
        )
        .unwrap()
        .unwrap();
    assert!(reopened.list_records(10).unwrap().is_empty());
    reconcile_all(&reopened, &retirement, 1);
    reopened
        .retire_provider_file_publication(retirement)
        .unwrap();

    for record_id in record_ids {
        assert!(!row_exists(&reopened, "history_records", record_id));
    }
    assert!(reopened.list_records(10).unwrap().is_empty());
}
