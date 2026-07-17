fn publication_material_root(store: &Store, scope: &ProviderFilePublicationScope) -> String {
    store
        .conn
        .query_row(
            "SELECT material_source_root FROM provider_file_publications WHERE replacement_id = ?1",
            params![scope.scope_id.to_string()],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn pi_source_import_begin_owns_the_observed_file() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let mut file = source_file(20, 100);
    file.provider = CaptureProvider::Pi;
    file.source_format = "pi_session_jsonl".to_owned();
    file.source_root = "/history/pi/sessions".to_owned();
    file.source_path = "/history/pi/sessions/session.jsonl".to_owned();
    let generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    let observation = source_outcome(&file, generation, 120).observation;

    assert_eq!(
        store
            .begin_provider_file_publication(
                file.provider,
                observation,
                ProviderFilePublicationMaterialOwner::catalog_root(
                    file.provider,
                    "pi_session_jsonl",
                    &file.source_root,
                ),
                ProviderFilePublicationKind::Replacement,
                110,
            )
            .unwrap_err(),
        StoreError::InvalidProviderFilePublicationScope
    );
    assert_eq!(
        store
            .begin_provider_file_publication(
                file.provider,
                observation,
                ProviderFilePublicationMaterialOwner::source_file(
                    CaptureProvider::Codex,
                    "pi_session_jsonl",
                    &file.source_path,
                ),
                ProviderFilePublicationKind::Replacement,
                110,
            )
            .unwrap_err(),
        StoreError::InvalidProviderFilePublicationScope
    );
    let scope = store
        .begin_provider_file_publication(
            file.provider,
            observation,
            ProviderFilePublicationMaterialOwner::source_file(
                file.provider,
                "pi_session_jsonl",
                &file.source_path,
            ),
            ProviderFilePublicationKind::Replacement,
            110,
        )
        .unwrap();

    assert_eq!(publication_material_root(&store, &scope), file.source_path);
}

#[test]
fn codex_catalog_begin_preserves_the_inventory_root_owner() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let mut catalog = catalog_file(20, 100);
    catalog.provider = CaptureProvider::Codex;
    catalog.source_format = "codex_session_jsonl_tree".to_owned();
    catalog.source_root = "/history/codex/sessions".to_owned();
    catalog.source_path = "/history/codex/sessions/session.jsonl".to_owned();
    let generation = store
        .allocate_catalog_inventory_generation(catalog.provider, &catalog.source_root)
        .unwrap();
    store
        .upsert_catalog_sessions(generation, std::slice::from_ref(&catalog))
        .unwrap();
    let observation = catalog_observation(&catalog, generation, 120);
    let scope = store
        .begin_provider_file_publication(
            catalog.provider,
            observation,
            ProviderFilePublicationMaterialOwner::catalog_root(
                catalog.provider,
                "codex_session_jsonl",
                &catalog.source_root,
            ),
            ProviderFilePublicationKind::Replacement,
            110,
        )
        .unwrap();

    assert_eq!(
        publication_material_root(&store, &scope),
        catalog.source_root
    );
}
