fn fresh_new_catalog(store: &Store, identity: &str) -> (CatalogSession, u64) {
    let mut catalog = catalog_file(10, 20);
    catalog.external_session_id = Some(identity.to_owned());
    catalog.metadata = json!({"file_observation_token_v1": "fresh-new-token"});
    let generation = store
        .allocate_catalog_inventory_generation(catalog.provider, &catalog.source_root)
        .unwrap();
    store
        .upsert_catalog_sessions(generation, std::slice::from_ref(&catalog))
        .unwrap();
    store
        .complete_catalog_inventory_generation(catalog.provider, &catalog.source_root, generation)
        .unwrap();
    (catalog, generation)
}

fn fresh_new_catalog_outcome(
    catalog: &CatalogSession,
    generation: u64,
    status: CatalogIndexedStatus,
) -> ProviderFileImportOutcome<'_> {
    ProviderFileImportOutcome {
        provider: catalog.provider,
        observation: catalog_observation(catalog, generation, 50),
        status,
        error: (status == CatalogIndexedStatus::Rejected).then_some("malformed transcript"),
    }
}

#[test]
fn fresh_new_atomic_batch_commits_status_and_checkpoint_together() {
    let temp = tempdir().unwrap();
    let mut store = Store::open(temp.path().join("fresh-new-success.sqlite")).unwrap();
    let (catalog, generation) = fresh_new_catalog(&store, "fresh-new-success");
    let outcome = fresh_new_catalog_outcome(&catalog, generation, CatalogIndexedStatus::Indexed);
    let checkpoint = checkpoint_for_catalog(&catalog, catalog.file_size_bytes, 1, 50);

    let committed = store
        .commit_fresh_new_atomic_batch::<_, StoreError>(
            &[outcome],
            std::slice::from_ref(&checkpoint),
            &[(catalog.provider, "fresh-new-success".to_owned())],
            || Ok(true),
            |_| Ok("committed"),
        )
        .unwrap();

    assert_eq!(committed, Some("committed"));
    assert_eq!(
        store.provider_file_checkpoint(checkpoint.key()).unwrap(),
        Some(checkpoint)
    );
    assert!(store
        .list_catalog_import_work(
            catalog.provider,
            &catalog.source_root,
            ImportWorkClass::Fresh,
            1,
        )
        .unwrap()
        .is_empty());
}

#[test]
fn fresh_new_deterministic_rejection_is_terminal_without_checkpoint() {
    let temp = tempdir().unwrap();
    let mut store = Store::open(temp.path().join("fresh-new-rejected.sqlite")).unwrap();
    let (catalog, generation) = fresh_new_catalog(&store, "fresh-new-rejected");
    let outcome = fresh_new_catalog_outcome(&catalog, generation, CatalogIndexedStatus::Rejected);

    assert!(store
        .reject_fresh_new_atomic_batch::<StoreError>(&[outcome], || Ok(true))
        .unwrap());
    assert!(store
        .list_catalog_import_work(
            catalog.provider,
            &catalog.source_root,
            ImportWorkClass::Fresh,
            1,
        )
        .unwrap()
        .is_empty());
    assert!(store
        .list_catalog_import_work(
            catalog.provider,
            &catalog.source_root,
            ImportWorkClass::Recovery,
            1,
        )
        .unwrap()
        .is_empty());
}

#[test]
fn fresh_new_atomic_batch_routes_a_superseded_identity_to_recovery() {
    let temp = tempdir().unwrap();
    let mut store = Store::open(temp.path().join("fresh-new.sqlite")).unwrap();
    let (catalog, generation) = fresh_new_catalog(&store, "fresh-new-identity");

    let update = CatalogSourceIndexUpdate {
        source_root: &catalog.source_root,
        source_path: &catalog.source_path,
        file_size_bytes: catalog.file_size_bytes,
        file_modified_at_ms: catalog.file_modified_at_ms,
        import_revision: catalog.import_revision,
        inventory_generation: generation,
        file_sha256: None,
        event_count: Some(1),
        indexed_at_ms: 50,
    };
    let outcome = ProviderFileImportOutcome {
        provider: catalog.provider,
        observation: ProviderFileInventoryObservation::ObservedCatalog {
            source_format: &catalog.source_format,
            update,
            metadata: &catalog.metadata,
        },
        status: CatalogIndexedStatus::Indexed,
        error: None,
    };
    let checkpoint = checkpoint_for_catalog(&catalog, catalog.file_size_bytes, 1, 50);
    let committed = store
        .commit_fresh_new_atomic_batch::<(), StoreError>(
            &[outcome],
            &[checkpoint],
            &[
                (catalog.provider, "fresh-new-identity".to_owned()),
                (catalog.provider, "fresh-new-identity".to_owned()),
            ],
            || Ok(true),
            |_| Ok(()),
        )
        .unwrap();
    assert!(committed.is_none());
    let work = store
        .list_catalog_import_work(
            catalog.provider,
            &catalog.source_root,
            ImportWorkClass::Recovery,
            1,
        )
        .unwrap();
    assert_eq!(work.len(), 1);
    assert_eq!(work[0].reason, ImportPendingReason::RecoveryReplacement);
}
