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
