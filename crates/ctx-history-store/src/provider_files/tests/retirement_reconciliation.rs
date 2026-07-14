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
