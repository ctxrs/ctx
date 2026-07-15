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

fn create_mutated_retirement_marker(store: &Store, family: RetirementInventoryFamily) {
    let (source_id, old_event_id, replacement_event_id) = match family {
        RetirementInventoryFamily::Catalog => (
            Uuid::from_u128(65_200),
            Uuid::from_u128(65_201),
            Uuid::from_u128(65_202),
        ),
        RetirementInventoryFamily::SourceImport => (
            Uuid::from_u128(65_210),
            Uuid::from_u128(65_211),
            Uuid::from_u128(65_212),
        ),
    };
    insert_capture_source(store, source_id, PATH_A, "retirement-family-format");
    insert_raw_event(store, old_event_id, 1, source_id, "old generation");

    let scope = match family {
        RetirementInventoryFamily::Catalog => {
            let catalog = catalog_file(20, 100);
            let generation = store
                .allocate_catalog_inventory_generation(catalog.provider, &catalog.source_root)
                .unwrap();
            store
                .upsert_catalog_sessions(generation, std::slice::from_ref(&catalog))
                .unwrap();
            store
                .begin_provider_file_publication(
                    catalog.provider,
                    catalog_observation(&catalog, generation, 110),
                    MATERIAL_FORMAT,
                    ProviderFilePublicationKind::Replacement,
                    105,
                )
                .unwrap()
        }
        RetirementInventoryFamily::SourceImport => {
            let file = source_file(20, 100);
            let generation = store
                .allocate_source_import_inventory_generation(file.provider, &file.source_root)
                .unwrap();
            store
                .upsert_source_import_files(generation, std::slice::from_ref(&file))
                .unwrap();
            store
                .begin_provider_file_publication(
                    file.provider,
                    source_outcome(&file, generation, 110).observation,
                    MATERIAL_FORMAT,
                    ProviderFilePublicationKind::Replacement,
                    105,
                )
                .unwrap()
        }
    };
    prepare_all(store, &scope, 1);
    let mut replacement = event_fixture(
        replacement_event_id,
        2,
        source_id,
        "retirement-family-format".to_owned(),
        "replacement generation",
    );
    replacement.dedupe_key = None;
    store
        .with_provider_file_publication_writes(&scope, |store| store.upsert_event(&replacement))
        .unwrap();
    drop(scope);
}

fn add_nonmatching_current_retirement_observations(
    store: &Store,
    family: RetirementInventoryFamily,
) -> u64 {
    match family {
        RetirementInventoryFamily::Catalog => {
            let generation = store
                .allocate_catalog_inventory_generation(CaptureProvider::Claude, ROOT)
                .unwrap();
            let mut wrong_format = catalog_file(30, 130);
            wrong_format.source_format = WRONG_CATALOG_FORMAT.to_owned();
            wrong_format.external_session_id = Some("wrong-catalog-format".to_owned());
            store
                .upsert_catalog_sessions(generation, &[wrong_format])
                .unwrap();

            let mut opposite_family = source_file(40, 140);
            opposite_family.source_format = MATERIAL_FORMAT.to_owned();
            let opposite_generation = store
                .allocate_source_import_inventory_generation(
                    opposite_family.provider,
                    &opposite_family.source_root,
                )
                .unwrap();
            store
                .upsert_source_import_files(opposite_generation, &[opposite_family])
                .unwrap();
            generation
        }
        RetirementInventoryFamily::SourceImport => {
            let generation = store
                .allocate_source_import_inventory_generation(CaptureProvider::Claude, ROOT)
                .unwrap();
            let mut wrong_format = source_file(30, 130);
            wrong_format.source_format = WRONG_SOURCE_IMPORT_FORMAT.to_owned();
            store
                .upsert_source_import_files(generation, &[wrong_format])
                .unwrap();

            let mut opposite_family = catalog_file(40, 140);
            opposite_family.source_format = FORMAT.to_owned();
            opposite_family.external_session_id = Some("opposite-catalog-family".to_owned());
            let opposite_generation = store
                .allocate_catalog_inventory_generation(
                    opposite_family.provider,
                    &opposite_family.source_root,
                )
                .unwrap();
            store
                .upsert_catalog_sessions(opposite_generation, &[opposite_family])
                .unwrap();
            generation
        }
    }
}

fn revive_matching_retirement_observation(
    store: &Store,
    family: RetirementInventoryFamily,
    generation: u64,
) {
    match family {
        RetirementInventoryFamily::Catalog => store
            .upsert_catalog_sessions(generation, &[catalog_file(20, 100)])
            .unwrap(),
        RetirementInventoryFamily::SourceImport => store
            .upsert_source_import_files(generation, &[source_file(20, 100)])
            .unwrap(),
    };
}

fn current_retirement_observation_exists(store: &Store, table: &str, source_format: &str) -> bool {
    store
        .conn
        .query_row(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM {table} WHERE provider = ?1 \
                 AND source_format = ?2 AND source_root = ?3 AND source_path = ?4 \
                 AND is_stale = 0)"
            ),
            params![
                CaptureProvider::Claude.as_str(),
                source_format,
                ROOT,
                PATH_A
            ],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn retirement_ignores_opposite_family_and_wrong_format_current_observations() {
    for family in [
        RetirementInventoryFamily::Catalog,
        RetirementInventoryFamily::SourceImport,
    ] {
        let temp = tempdir().unwrap();
        let path = temp.path().join("work.sqlite");
        {
            let store = Store::open(&path).unwrap();
            create_mutated_retirement_marker(&store, family);
        }

        let store = Store::open(&path).unwrap();
        add_nonmatching_current_retirement_observations(&store, family);
        assert!(current_retirement_observation_exists(
            &store,
            family.inventory_table(),
            family.wrong_source_format(),
        ));
        assert!(current_retirement_observation_exists(
            &store,
            family.opposite_inventory_table(),
            family.inventory_source_format(),
        ));

        assert_eq!(
            store
                .provider_file_publication_retirement_work_count()
                .unwrap(),
            1,
            "{family:?}",
        );
        assert!(store
            .list_provider_file_publication_retirement_work(0)
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .list_provider_file_publication_retirement_work(10)
                .unwrap(),
            vec![ProviderFilePublicationRetirementWork {
                provider: CaptureProvider::Claude,
                material_source_format: MATERIAL_FORMAT.to_owned(),
                material_source_root: ROOT.to_owned(),
                source_path: PATH_A.to_owned(),
                estimated_bytes: 20,
                last_attempt_at_ms: 105,
            }],
            "{family:?}",
        );

        let retirement = store
            .begin_provider_file_publication_retirement(
                CaptureProvider::Claude,
                MATERIAL_FORMAT,
                ROOT,
                PATH_A,
                150,
            )
            .unwrap()
            .unwrap_or_else(|| panic!("{family:?} retirement was not adoptable"));
        prepare_all(&store, &retirement, 1);
        reconcile_all(&store, &retirement, 1);
        store.retire_provider_file_publication(retirement).unwrap();

        assert_eq!(
            store
                .provider_file_publication_retirement_work_count()
                .unwrap(),
            0,
            "{family:?}",
        );
        assert!(store
            .list_provider_file_publication_retirement_work(10)
            .unwrap()
            .is_empty());
        assert!(!store.has_pending_provider_file_publications().unwrap());
        assert!(current_retirement_observation_exists(
            &store,
            family.inventory_table(),
            family.wrong_source_format(),
        ));
        assert!(current_retirement_observation_exists(
            &store,
            family.opposite_inventory_table(),
            family.inventory_source_format(),
        ));
    }
}

fn create_source_import_retirement_work(
    store: &Store,
    source_path: &str,
    source_id: Uuid,
    event_id: Uuid,
    created_at_ms: i64,
) {
    let mut file = source_file(20, created_at_ms - 5);
    file.source_path = source_path.to_owned();
    let generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    insert_capture_source(store, source_id, source_path, "retirement-attempt-order");
    insert_raw_event(
        store,
        event_id,
        created_at_ms,
        source_id,
        "retirement attempt ordering",
    );

    let scope = store
        .begin_provider_file_publication(
            file.provider,
            source_outcome(&file, generation, created_at_ms + 5).observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            created_at_ms,
        )
        .unwrap();
    prepare_all(store, &scope, 1);
    let mut mutation = event_fixture(
        Uuid::from_u128(event_id.as_u128() + 1),
        (created_at_ms + 1) as u64,
        source_id,
        "retirement-attempt-order".to_owned(),
        "mutation before retirement",
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
            created_at_ms + 10,
            generation,
        )
        .unwrap();
}

#[test]
fn retirement_attempt_timestamps_rotate_fairly_and_survive_reopen() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    {
        let store = Store::open(&path).unwrap();
        create_source_import_retirement_work(
            &store,
            PATH_A,
            Uuid::from_u128(65_300),
            Uuid::from_u128(65_301),
            100,
        );
        create_source_import_retirement_work(
            &store,
            PATH_B,
            Uuid::from_u128(65_310),
            Uuid::from_u128(65_311),
            200,
        );

        let initial = store
            .list_provider_file_publication_retirement_work(10)
            .unwrap();
        assert_eq!(
            initial
                .iter()
                .map(|work| (work.source_path.as_str(), work.last_attempt_at_ms))
                .collect::<Vec<_>>(),
            vec![(PATH_A, 100), (PATH_B, 200)]
        );

        let retirement = store
            .begin_provider_file_publication_retirement(
                CaptureProvider::Claude,
                MATERIAL_FORMAT,
                ROOT,
                PATH_A,
                300,
            )
            .unwrap()
            .unwrap();
        let after_begin = store
            .list_provider_file_publication_retirement_work(10)
            .unwrap();
        assert_eq!(
            after_begin
                .iter()
                .map(|work| (work.source_path.as_str(), work.last_attempt_at_ms))
                .collect::<Vec<_>>(),
            vec![(PATH_B, 200), (PATH_A, 300)]
        );
        store.abandon_provider_file_publication(retirement).unwrap();
    }

    let reopened = Store::open(&path).unwrap();
    let after_abandon = reopened
        .list_provider_file_publication_retirement_work(10)
        .unwrap();
    assert_eq!(after_abandon[0].source_path, PATH_B);
    assert_eq!(after_abandon[0].last_attempt_at_ms, 200);
    assert_eq!(after_abandon[1].source_path, PATH_A);
    assert!(after_abandon[1].last_attempt_at_ms > 300);
}

#[test]
fn matching_family_and_format_current_observation_blocks_retirement() {
    for family in [
        RetirementInventoryFamily::Catalog,
        RetirementInventoryFamily::SourceImport,
    ] {
        let temp = tempdir().unwrap();
        let path = temp.path().join("work.sqlite");
        {
            let store = Store::open(&path).unwrap();
            create_mutated_retirement_marker(&store, family);
        }

        let store = Store::open(&path).unwrap();
        let generation = add_nonmatching_current_retirement_observations(&store, family);
        revive_matching_retirement_observation(&store, family, generation);
        assert!(current_retirement_observation_exists(
            &store,
            family.inventory_table(),
            family.inventory_source_format(),
        ));

        assert_eq!(
            store
                .provider_file_publication_retirement_work_count()
                .unwrap(),
            0,
            "{family:?}",
        );
        assert!(store
            .list_provider_file_publication_retirement_work(10)
            .unwrap()
            .is_empty());
        assert!(store
            .begin_provider_file_publication_retirement(
                CaptureProvider::Claude,
                MATERIAL_FORMAT,
                ROOT,
                PATH_A,
                150,
            )
            .unwrap()
            .is_none());
        assert!(store.has_pending_provider_file_publications().unwrap());
        assert_eq!(table_row_count(&store, "provider_file_publications"), 1);
    }
}

#[test]
fn tombstoned_observation_retires_visibility_fence_and_can_be_adopted() {
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
    let source = Uuid::from_u128(65);
    insert_capture_source(&store, source, PATH_A, "tombstoned-owner");
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
    assert!(matches!(
        store.get_capture_source(source),
        Err(StoreError::NotFound(id)) if id == source
    ));

    store
        .mark_source_import_missing_paths_stale(
            file.provider,
            &file.source_root,
            &[],
            111,
            generation,
        )
        .unwrap();
    assert!(!store.has_pending_provider_file_publications().unwrap());
    assert_eq!(store.get_capture_source(source).unwrap().id, source);
    store.abandon_provider_file_publication(scope).unwrap();

    let revived_generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(revived_generation, std::slice::from_ref(&file))
        .unwrap();
    let revived_outcome = source_outcome(&file, revived_generation, 120);
    let adopted = store
        .begin_provider_file_publication(
            file.provider,
            revived_outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            115,
        )
        .unwrap();
    assert!(store.has_pending_provider_file_publications().unwrap());
    assert!(matches!(
        store.get_capture_source(source),
        Err(StoreError::NotFound(id)) if id == source
    ));
    reconcile_all(&store, &adopted, 10);
    store
        .finalize_provider_file_publication(
            adopted,
            revived_outcome,
            ProviderFilePublicationCommit::Replacement(None),
        )
        .unwrap();
    assert_eq!(store.get_capture_source(source).unwrap().id, source);
}

#[test]
fn retirement_without_prior_material_skips_preparation() {
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
        let mut event = event_fixture(
            Uuid::from_u128(66_500),
            1,
            Uuid::from_u128(66_000),
            "retirement-preparation-0".to_owned(),
            "mutation before disappearance",
        );
        event.dedupe_key = None;
        store
            .with_provider_file_publication_writes(&scope, |store| store.upsert_event(&event))
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
    let mut completed_on_cycle = None;
    for cycle in 0..4 {
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
        let progress = store
            .prepare_provider_file_publication_slice(&scope, 64)
            .unwrap();
        staged += progress.source_ids_staged;
        if progress.complete {
            completed_on_cycle = Some(cycle);
            reconcile_all(&store, &scope, 256);
            store.retire_provider_file_publication(scope).unwrap();
            break;
        }
        assert!(store
            .abandon_provider_file_publication(scope)
            .unwrap()
            .is_none());
    }

    assert_eq!(staged, 0);
    assert_eq!(completed_on_cycle, Some(0));
    let store = Store::open(&path).unwrap();
    assert!(!store.has_pending_provider_file_publications().unwrap());
}

#[test]
fn retirement_reconciliation_preserves_seen_candidates_across_reopen_cycles() {
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
        assert!(
            store
                .prepare_provider_file_publication_slice(&scope, 64)
                .unwrap()
                .complete
        );
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
    assert_eq!(table_row_count(&store, "events"), EVENT_COUNT as i64 + 1);
    assert!(!store.has_pending_provider_file_publications().unwrap());
}
