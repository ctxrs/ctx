#[test]
fn codex_preinventory_failures_survive_when_catalog_has_no_pending_sessions() {
    let temp = tempdir();
    let source_path = temp.path().join("sessions");
    fs::create_dir_all(&source_path).unwrap();
    let source = explicit_path_source(CaptureProvider::Codex, source_path);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let source_root = source.path.display().to_string();
    let inventory_generation = store
        .allocate_catalog_inventory_generation(CaptureProvider::Codex, &source_root)
        .unwrap();
    assert!(store
        .complete_catalog_inventory_generation(
            CaptureProvider::Codex,
            &source_root,
            inventory_generation,
        )
        .unwrap());
    let catalog = CatalogSummary {
        failed_sessions: 1,
        failures: vec![ProviderImportFailure {
            line: 0,
            error: "catalog-only rejection".to_owned(),
        }],
        ..CatalogSummary::default()
    };

    let summary = import_incremental_codex_session_tree(
        &mut store,
        &source,
        new_id(),
        None,
        Some(&catalog),
        Some(inventory_generation),
        false,
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert_eq!(summary.failures, catalog.failures);
}
