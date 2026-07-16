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
        // Simulate a legacy database created before publication work was
        // globally serialized. An ineffective marker would not have blocked
        // the second owner, then both could later become durable recovery work.
        store
            .conn
            .execute(
                "UPDATE provider_file_publications SET mutation_started = 0 WHERE source_path = ?1",
                [PATH_A],
            )
            .unwrap();
        create_source_import_retirement_work(
            &store,
            PATH_B,
            Uuid::from_u128(65_310),
            Uuid::from_u128(65_311),
            200,
        );
        store
            .conn
            .execute(
                "UPDATE provider_file_publications SET mutation_started = 1 WHERE source_path = ?1",
                [PATH_A],
            )
            .unwrap();

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
fn newer_generation_of_same_observation_adopts_existing_publication() {
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
            .begin_provider_file_publication_retirement(
                CaptureProvider::Claude,
                MATERIAL_FORMAT,
                ROOT,
                PATH_A,
                150,
            )
            .unwrap()
            .is_none());
        match family {
            RetirementInventoryFamily::Catalog => {
                let catalog = catalog_file(20, 100);
                let observation = catalog_observation(&catalog, generation, 160);
                let scope = store
                    .begin_provider_file_publication(
                        catalog.provider,
                        observation,
                        MATERIAL_FORMAT,
                        ProviderFilePublicationKind::Replacement,
                        155,
                    )
                    .unwrap();
                let outcome = ProviderFileImportOutcome {
                    provider: catalog.provider,
                    observation,
                    status: CatalogIndexedStatus::Indexed,
                    error: None,
                };
                reconcile_all(&store, &scope, 1);
                store
                    .finalize_provider_file_publication(
                        scope,
                        outcome,
                        ProviderFilePublicationCommit::Replacement(None),
                    )
                    .unwrap();
            }
            RetirementInventoryFamily::SourceImport => {
                let file = source_file(20, 100);
                let outcome = source_outcome(&file, generation, 160);
                let scope = store
                    .begin_provider_file_publication(
                        file.provider,
                        outcome.observation,
                        MATERIAL_FORMAT,
                        ProviderFilePublicationKind::Replacement,
                        155,
                    )
                    .unwrap();
                reconcile_all(&store, &scope, 1);
                store
                    .finalize_provider_file_publication(
                        scope,
                        outcome,
                        ProviderFilePublicationCommit::Replacement(None),
                    )
                    .unwrap();
            }
        }
        assert!(!store.has_pending_provider_file_publications().unwrap());
        assert_eq!(table_row_count(&store, "provider_file_publications"), 0);
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
fn explicit_observation_invalidation_survives_an_identical_inventory_revival() {
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
            105,
        )
        .unwrap();
    let source = capture_source_fixture(Uuid::from_u128(66), PATH_A, "invalidated-owner");
    store
        .with_provider_file_publication_writes(&scope, |store| store.upsert_capture_source(&source))
        .unwrap();

    let owner = store
        .effective_provider_file_publication_inventory_owner()
        .unwrap()
        .unwrap();
    store
        .mark_source_import_missing_paths_stale(
            file.provider,
            &file.source_root,
            &[],
            115,
            generation,
        )
        .unwrap();
    assert!(store
        .invalidate_effective_provider_file_publication_observation(&owner, 120)
        .unwrap());
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();

    assert_eq!(
        store
            .provider_file_publication_retirement_work_count()
            .unwrap(),
        1
    );
    assert!(!store
        .provider_file_publication_matches_candidate(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            &file.source_root,
        )
        .unwrap());
    store.abandon_provider_file_publication(scope).unwrap();
    assert!(store
        .begin_provider_file_publication_retirement(
            file.provider,
            MATERIAL_FORMAT,
            &file.source_root,
            &file.source_path,
            125,
        )
        .unwrap()
        .is_some());
}

#[test]
fn explicit_observation_invalidation_discards_an_unmutated_publication() {
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
            105,
        )
        .unwrap();
    store.abandon_provider_file_publication(scope).unwrap();

    let owner = store
        .effective_provider_file_publication_inventory_owner()
        .unwrap()
        .unwrap();
    assert!(store
        .invalidate_effective_provider_file_publication_observation(&owner, 120)
        .unwrap());
    assert_eq!(table_row_count(&store, "provider_file_publications"), 0);

    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    let restarted = store
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            125,
        )
        .unwrap();
    assert!(!restarted.tracks_prior_material());
    assert!(matches!(
        store.abort_provider_file_publication(restarted).unwrap(),
        std::ops::ControlFlow::Continue(None)
    ));
}
