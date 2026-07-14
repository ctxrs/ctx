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
