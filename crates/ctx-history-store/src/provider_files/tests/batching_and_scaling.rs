#[test]
fn large_batched_seen_scope_is_stored_in_main_database() {
    const EVENT_COUNT: u128 = 5_000;
    const BATCH: u128 = 100;

    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let store = Store::open(&path).unwrap();
    let file = source_file(100, 100);
    let generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    let source = Uuid::from_u128(90);
    insert_capture_source(&store, source, PATH_A, "large-session");
    store.begin_immediate_batch().unwrap();
    for index in 0..EVENT_COUNT {
        insert_raw_event(
            &store,
            Uuid::from_u128(10_000 + index),
            10_000 + index as i64,
            source,
            "prior",
        );
    }
    store.commit_batch().unwrap();
    let outcome = source_outcome(&file, generation, 120);
    let scope = store
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            110,
        )
        .unwrap();
    assert!(scope.tracks_prior_material);
    let attached_staging: bool = store
        .conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM pragma_database_list WHERE name = 'provider_replacement_stage')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!attached_staging);
    assert!(main_table_exists(&store, "provider_file_publication_seen"));

    for start in (0..EVENT_COUNT).step_by(BATCH as usize) {
        store.begin_immediate_batch().unwrap();
        for index in start..(start + BATCH).min(EVENT_COUNT) {
            store
                .track_provider_file_publication_event(Uuid::from_u128(10_000 + index))
                .unwrap();
        }
        store.commit_batch().unwrap();
    }

    assert_eq!(staged_seen_count(&store), EVENT_COUNT as i64);
    reconcile_all(&store, &scope, 127);
    let counts = store
        .finalize_provider_file_publication(
            scope,
            outcome,
            ProviderFilePublicationCommit::Replacement(Some(&checkpoint(
                100,
                10,
                "unix:2049:large",
                120,
            ))),
        )
        .unwrap();
    assert_eq!(counts.reconciliation.events, 0);
    assert!(store.provider_file_publication.borrow().is_none());
    let staged_seen: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM provider_file_publication_seen",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(staged_seen, 0);
}

#[test]
fn replacement_preparation_pages_prior_source_identity_snapshot_without_a_total_cap() {
    const SOURCE_COUNT: u128 = 4_097;

    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = source_file(20, 100);
    let generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    store.begin_immediate_batch().unwrap();
    for index in 0..SOURCE_COUNT {
        let source = Uuid::from_u128(50_000 + index);
        let session = Uuid::from_u128(60_000 + index);
        let external = format!("source-{index}");
        insert_capture_source(&store, source, PATH_A, &external);
        insert_raw_session(&store, session, source, &external);
    }
    store.commit_batch().unwrap();
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
        store
            .prepare_provider_file_publication_slice(&scope, usize::MAX)
            .unwrap_err(),
        StoreError::ProviderFileReconciliationLimitOutOfRange {
            value: usize::MAX,
            max: PROVIDER_FILE_PREPARATION_MAX_ROWS,
        }
    ));
    let mut staged = 0;
    loop {
        let progress = store
            .prepare_provider_file_publication_slice(&scope, 17)
            .unwrap();
        staged += progress.source_ids_staged;
        if progress.complete {
            break;
        }
    }
    assert_eq!(staged, SOURCE_COUNT as usize);
    assert_eq!(staged_prior_source_count(&store), SOURCE_COUNT as i64);
    store.abandon_provider_file_publication(scope).unwrap();
}

#[test]
fn reconciliation_queries_owner_indexes_without_visiting_unrelated_corpus_rows() {
    const UNRELATED_EVENTS: u128 = 5_000;

    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = source_file(20, 100);
    let generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    let owner_source = Uuid::from_u128(42_000);
    let unrelated_source = Uuid::from_u128(42_001);
    let owner_event = Uuid::from_u128(42_002);
    insert_capture_source(&store, owner_source, PATH_A, "bounded-owner");
    insert_capture_source(
        &store,
        unrelated_source,
        "/history/claude/projects/unrelated.jsonl",
        "bounded-unrelated",
    );
    insert_raw_event(&store, owner_event, 1, owner_source, "owner");
    store.begin_immediate_batch().unwrap();
    for index in 0..UNRELATED_EVENTS {
        insert_raw_event(
            &store,
            Uuid::from_u128(43_000 + index),
            10_000 + index as i64,
            unrelated_source,
            "unrelated",
        );
    }
    store.commit_batch().unwrap();

    let outcome = source_outcome(&file, generation, 120);
    let scope = store
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            110,
        )
        .unwrap();
    prepare_all(&store, &scope, 1);
    let scan = store
        .reconciliation_batch_rows(
            &scope.scope_id.to_string(),
            CLEANUP_PHASE_EVENTS,
            None,
            None,
            1,
        )
        .unwrap();
    assert_eq!(scan.visited, 1);
    assert_eq!(scan.owned_entity_ids, vec![owner_event.to_string()]);

    for phase in 0..CLEANUP_PHASE_COMPLETE {
        let spec = reconciliation_phase_spec(phase).unwrap();
        let mut statement = store
            .conn
            .prepare(&format!("EXPLAIN QUERY PLAN {}", spec.owner_select_sql))
            .unwrap();
        let details = statement
            .query_map(
                params![owner_source.to_string(), Option::<String>::None, 2],
                |row| row.get::<_, String>(3),
            )
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .join("\n");
        for global_scan in [
            "SCAN events",
            "SCAN runs",
            "SCAN sessions",
            "SCAN files_touched",
            "SCAN artifacts",
            "SCAN summaries",
            "SCAN history_records",
        ] {
            assert!(
                !details.contains(global_scan),
                "phase {phase} has unrelated-corpus plan `{global_scan}`:\n{details}"
            );
        }
    }
    store.abandon_provider_file_publication(scope).unwrap();
}

#[test]
fn large_owner_tiny_slices_keep_candidate_work_linear_across_interrupted_retry() {
    const EVENT_COUNT: usize = 600;
    const FIRST_ATTEMPT_SLICES: usize = 204;

    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let file = source_file(20, 100);
    let generation;
    let first_queries;
    let first_candidates;
    {
        let store = Store::open(&path).unwrap();
        generation = store
            .allocate_source_import_inventory_generation(file.provider, &file.source_root)
            .unwrap();
        store
            .upsert_source_import_files(generation, std::slice::from_ref(&file))
            .unwrap();
        let source = Uuid::from_u128(78_000);
        insert_capture_source(&store, source, PATH_A, "linear-owner");
        store.begin_immediate_batch().unwrap();
        for index in 0..EVENT_COUNT {
            insert_raw_event(
                &store,
                Uuid::from_u128(78_100 + index as u128),
                78_100 + index as i64,
                source,
                "linear stale event",
            );
        }
        store.commit_batch().unwrap();
        let outcome = source_outcome(&file, generation, 120);
        let scope = store
            .begin_provider_file_publication(
                file.provider,
                outcome.observation,
                MATERIAL_FORMAT,
                ProviderFilePublicationKind::Replacement,
                110,
            )
            .unwrap();
        prepare_all(&store, &scope, 1);
        for _ in 0..FIRST_ATTEMPT_SLICES {
            let progress = store
                .reconcile_provider_file_publication_slice(&scope, 1)
                .unwrap();
            assert!(!progress.complete);
        }
        first_queries = store.provider_file_reconciliation_queries.get();
        first_candidates = store.provider_file_reconciliation_candidates.get();
        drop(scope);
    }

    let store = Store::open(&path).unwrap();
    assert!(store.has_pending_provider_file_publications().unwrap());
    let outcome = source_outcome(&file, generation, 130);
    let scope = store
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            125,
        )
        .unwrap();
    reconcile_all(&store, &scope, 1);
    let finalized = store
        .finalize_provider_file_publication(
            scope,
            outcome,
            ProviderFilePublicationCommit::Replacement(None),
        )
        .unwrap();
    let total_queries = first_queries + store.provider_file_reconciliation_queries.get();
    let total_candidates = first_candidates + store.provider_file_reconciliation_candidates.get();
    assert_eq!(finalized.reconciliation.events, EVENT_COUNT);
    assert!(
        total_queries <= EVENT_COUNT + FIRST_ATTEMPT_SLICES + 32,
        "tiny-slice reconciliation issued {total_queries} candidate queries"
    );
    assert!(
        total_candidates <= EVENT_COUNT * 3,
        "tiny-slice reconciliation materialized {total_candidates} candidates"
    );
    assert_eq!(table_row_count(&store, "events"), 0);

    for phase in [
        CLEANUP_PHASE_FILES,
        CLEANUP_PHASE_EDGES,
        CLEANUP_PHASE_EVENTS,
        CLEANUP_PHASE_RUNS,
    ] {
        let sql = reconciliation_phase_spec(phase).unwrap().owner_select_sql;
        assert!(
            sql.matches("LIMIT ?3").count() >= 3,
            "phase {phase} does not bound each candidate branch"
        );
    }
}
