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
fn publication_phases_resume_after_single_1024_row_slices_without_advancing_outcome() {
    const ROW_COUNT: u128 = 1_025;
    const SLICE_ROWS: usize = 1_024;

    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let original = source_file(10, 90);
    let changed = source_file(20, 100);
    let old_checkpoint = checkpoint(10, 3, "unix:2049:phased", 95);
    let new_checkpoint = checkpoint(20, 5, "unix:2049:phased", 150);
    let completion = ProviderFilePublicationCompletion {
        version: 1,
        payload: json!({
            "summary": {"imported": 0, "skipped": 1025},
            "checkpoint": {"committed_byte_offset": 20}
        }),
    };
    let changed_generation;
    let pending_indexed_state;

    {
        let store = Store::open(&path).unwrap();
        let original_generation = store
            .allocate_source_import_inventory_generation(original.provider, &original.source_root)
            .unwrap();
        store
            .upsert_source_import_files(original_generation, std::slice::from_ref(&original))
            .unwrap();
        store
            .upsert_provider_file_checkpoint(
                source_outcome(&original, original_generation, 95),
                &old_checkpoint,
            )
            .unwrap();
        store.begin_immediate_batch().unwrap();
        for index in 0..ROW_COUNT {
            let source_id = Uuid::from_u128(150_000 + index);
            insert_capture_source(&store, source_id, PATH_A, &format!("phased-source-{index}"));
            insert_raw_event(
                &store,
                Uuid::from_u128(160_000 + index),
                160_000 + index as i64,
                source_id,
                "stale phased event",
            );
        }
        store.commit_batch().unwrap();
        changed_generation = store
            .allocate_source_import_inventory_generation(changed.provider, &changed.source_root)
            .unwrap();
        store
            .upsert_source_import_files(changed_generation, std::slice::from_ref(&changed))
            .unwrap();
        pending_indexed_state = store
            .conn
            .query_row(
                "SELECT indexed_status, indexed_file_size_bytes, \
                        indexed_file_modified_at_ms \
                 FROM source_import_files WHERE provider = 'claude' \
                   AND source_root = ?1 AND source_path = ?2",
                params![ROOT, PATH_A],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                    ))
                },
            )
            .unwrap();
        let scope = store
            .begin_provider_file_publication(
                changed.provider,
                source_outcome(&changed, changed_generation, 150).observation,
                MATERIAL_FORMAT,
                ProviderFilePublicationKind::Replacement,
                105,
            )
            .unwrap();
        assert_eq!(
            store.provider_file_publication_phase(&scope).unwrap(),
            ProviderFilePublicationPhase::Preparing
        );
        let preparation = store
            .prepare_provider_file_publication_slice(&scope, SLICE_ROWS)
            .unwrap();
        assert_eq!(preparation.source_ids_staged, SLICE_ROWS);
        assert!(!preparation.complete);
        assert_eq!(staged_prior_source_count(&store), SLICE_ROWS as i64);
        store.abandon_provider_file_publication(scope).unwrap();
    }

    {
        let store = Store::open(&path).unwrap();
        let outcome = source_outcome(&changed, changed_generation, 150);
        let scope = store
            .begin_provider_file_publication(
                changed.provider,
                outcome.observation,
                MATERIAL_FORMAT,
                ProviderFilePublicationKind::Replacement,
                110,
            )
            .unwrap();
        assert_eq!(
            store.provider_file_publication_phase(&scope).unwrap(),
            ProviderFilePublicationPhase::Preparing
        );
        let preparation = store
            .prepare_provider_file_publication_slice(&scope, SLICE_ROWS)
            .unwrap();
        assert_eq!(preparation.source_ids_staged, 1);
        assert!(preparation.complete);
        assert_eq!(staged_prior_source_count(&store), ROW_COUNT as i64);
        assert_eq!(
            store.provider_file_publication_phase(&scope).unwrap(),
            ProviderFilePublicationPhase::Importing
        );
        store
            .stage_provider_file_publication_completion(&scope, &completion)
            .unwrap();
        assert_eq!(
            store.provider_file_publication_phase(&scope).unwrap(),
            ProviderFilePublicationPhase::Reconciling
        );
        store.abandon_provider_file_publication(scope).unwrap();
    }

    {
        let store = Store::open(&path).unwrap();
        let outcome = source_outcome(&changed, changed_generation, 150);
        let scope = store
            .begin_provider_file_publication(
                changed.provider,
                outcome.observation,
                MATERIAL_FORMAT,
                ProviderFilePublicationKind::Replacement,
                120,
            )
            .unwrap();
        assert_eq!(
            store
                .load_provider_file_publication_completion(&scope)
                .unwrap(),
            Some(completion.clone())
        );
        let reconciliation = store
            .reconcile_provider_file_publication_slice(&scope, SLICE_ROWS)
            .unwrap();
        assert_eq!(reconciliation.rows_scanned, SLICE_ROWS);
        assert_eq!(
            reconciliation.counts,
            ProviderFileReconciliationCounts::default()
        );
        assert!(!reconciliation.complete);
        let cleanup_cursor: Option<String> = store
            .conn
            .query_row(
                "SELECT cleanup_source_cursor FROM provider_file_publications",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(cleanup_cursor.is_some());
        assert_eq!(
            store
                .provider_file_checkpoint(old_checkpoint.key())
                .unwrap(),
            Some(old_checkpoint.clone())
        );
        let visible_indexed_state: (String, Option<i64>, Option<i64>) = store
            .conn
            .query_row(
                "SELECT indexed_status, indexed_file_size_bytes, \
                        indexed_file_modified_at_ms \
                 FROM source_import_files WHERE provider = 'claude' \
                   AND source_root = ?1 AND source_path = ?2",
                params![ROOT, PATH_A],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(visible_indexed_state, pending_indexed_state);
        store.abandon_provider_file_publication(scope).unwrap();
    }

    let store = Store::open(&path).unwrap();
    let outcome = source_outcome(&changed, changed_generation, 150);
    let scope = store
        .begin_provider_file_publication(
            changed.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            130,
        )
        .unwrap();
    assert_eq!(
        store.provider_file_publication_phase(&scope).unwrap(),
        ProviderFilePublicationPhase::Reconciling
    );
    assert_eq!(
        store
            .load_provider_file_publication_completion(&scope)
            .unwrap(),
        Some(completion)
    );
    let reconciliation = loop {
        let reconciliation = store
            .reconcile_provider_file_publication_slice(&scope, SLICE_ROWS)
            .unwrap();
        assert!(reconciliation.rows_scanned <= SLICE_ROWS);
        if reconciliation.complete {
            break reconciliation;
        }
    };
    assert_eq!(reconciliation.counts.events, ROW_COUNT as usize);
    assert_eq!(
        store.provider_file_publication_phase(&scope).unwrap(),
        ProviderFilePublicationPhase::ReadyToFinalize
    );
    assert_eq!(
        store
            .provider_file_checkpoint(old_checkpoint.key())
            .unwrap(),
        Some(old_checkpoint)
    );
    let visible_indexed_state: (String, Option<i64>, Option<i64>) = store
        .conn
        .query_row(
            "SELECT indexed_status, indexed_file_size_bytes, indexed_file_modified_at_ms \
             FROM source_import_files WHERE provider = 'claude' \
               AND source_root = ?1 AND source_path = ?2",
            params![ROOT, PATH_A],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(visible_indexed_state, pending_indexed_state);

    store
        .finalize_provider_file_publication(
            scope,
            outcome,
            ProviderFilePublicationCommit::Replacement(Some(&new_checkpoint)),
        )
        .unwrap();
    assert_eq!(
        store
            .provider_file_checkpoint(new_checkpoint.key())
            .unwrap(),
        Some(new_checkpoint)
    );
    let finalized_indexed_state: (String, Option<i64>, Option<i64>) = store
        .conn
        .query_row(
            "SELECT indexed_status, indexed_file_size_bytes, indexed_file_modified_at_ms \
             FROM source_import_files WHERE provider = 'claude' \
               AND source_root = ?1 AND source_path = ?2",
            params![ROOT, PATH_A],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        finalized_indexed_state,
        ("indexed".into(), Some(20), Some(100))
    );
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
        assert_eq!(
            spec.owner_select_sql.matches("ORDER BY").count(),
            spec.owner_select_sql.matches("LIMIT ?3").count(),
            "fresh phase {phase} contains an unbounded ordering clause"
        );
        assert!(!spec.owner_select_sql.contains("GROUP BY"));
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
        if phase != CLEANUP_PHASE_HISTORY_RECORD_TAGS {
            assert!(
                details.contains("idx_reconcile_"),
                "fresh phase {phase} omitted its reconciliation index:\n{details}"
            );
        }
        if matches!(
            phase,
            CLEANUP_PHASE_LINKS
                | CLEANUP_PHASE_SUMMARIES
                | CLEANUP_PHASE_SESSIONS
                | CLEANUP_PHASE_ARTIFACTS
                | CLEANUP_PHASE_HISTORY_RECORD_TAGS
                | CLEANUP_PHASE_RECORD_EDGES
                | CLEANUP_PHASE_HISTORY_RECORDS
                | CLEANUP_PHASE_VCS_WORKSPACES
                | CLEANUP_PHASE_AUDIT_LOG
        ) {
            assert!(
                !details.contains("USE TEMP B-TREE FOR ORDER BY"),
                "fresh direct phase {phase} uses a temp ordering plan:\n{details}"
            );
        }
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
fn legacy_reconciliation_tiny_pages_resume_through_large_direct_and_sparse_indirect_rows() {
    const DIRECT_OWNER_EVENTS: usize = 257;
    const UNRELATED_EVENTS: usize = 1_025;
    const INDIRECT_OWNER_EVENTS: usize = 2;
    const FIRST_ATTEMPT_SLICES: usize = 400;

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
        let owner_source = Uuid::from_u128(90_000);
        let unrelated_source = Uuid::from_u128(90_001);
        let owner_session = Uuid::from_u128(90_002);
        insert_capture_source(&store, owner_source, PATH_A, "legacy-owner");
        insert_capture_source(&store, unrelated_source, PATH_B, "legacy-unrelated");
        insert_raw_session(&store, owner_session, owner_source, "legacy-owner");
        store.begin_immediate_batch().unwrap();
        for index in 0..DIRECT_OWNER_EVENTS {
            insert_raw_event(
                &store,
                Uuid::from_u128(100_000 + index as u128),
                index as i64,
                owner_source,
                "legacy direct owner",
            );
        }
        let mut indirect_index = 0u128;
        for index in 0..UNRELATED_EVENTS {
            insert_raw_event(
                &store,
                Uuid::from_u128(200_000 + index as u128),
                10_000 + index as i64,
                unrelated_source,
                "legacy unrelated",
            );
            if matches!(index, 250 | 900) {
                store
                    .conn
                    .execute(
                        r#"
                        INSERT INTO events
                          (id, seq, session_id, event_type, role, occurred_at_ms,
                           payload_json)
                        VALUES (?1, ?2, ?3, 'message', 'user', 1, '{}')
                        "#,
                        params![
                            Uuid::from_u128(300_000 + indirect_index).to_string(),
                            20_000 + index as i64,
                            owner_session.to_string(),
                        ],
                    )
                    .unwrap();
                indirect_index += 1;
            }
        }
        store.commit_batch().unwrap();
        assert_eq!(indirect_index as usize, INDIRECT_OWNER_EVENTS);

        let optimized_indexes = store
            .conn
            .prepare(
                "SELECT name FROM sqlite_schema \
                 WHERE type = 'index' AND name LIKE 'idx_reconcile_%' ORDER BY name",
            )
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(optimized_indexes.len(), 20);
        for index in optimized_indexes {
            store
                .conn
                .execute(&format!("DROP INDEX {index}"), [])
                .unwrap();
        }

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
        stage_test_completion(&store, &scope);
        for _ in 0..FIRST_ATTEMPT_SLICES {
            let progress = store
                .reconcile_provider_file_publication_slice(&scope, 1)
                .unwrap();
            assert_eq!(progress.rows_scanned, 1);
            assert!(!progress.complete);
        }
        let durable_cursor: Option<String> = store
            .conn
            .query_row(
                "SELECT cleanup_entity_cursor FROM provider_file_publications",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(durable_cursor
            .as_deref()
            .is_some_and(|cursor| cursor.starts_with(LEGACY_INDIRECT_CURSOR_PREFIX)));
        first_queries = store.provider_file_reconciliation_queries.get();
        first_candidates = store.provider_file_reconciliation_candidates.get();
        drop(scope);
    }

    let store = Store::open(&path).unwrap();
    let optimized_indexes_present: bool = store
        .conn
        .query_row(
            crate::schema::indexes::RECONCILIATION_INDEXES_PRESENT_SQL,
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!optimized_indexes_present);
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
    let complete = loop {
        let progress = store
            .reconcile_provider_file_publication_slice(&scope, 1)
            .unwrap();
        assert!(progress.rows_scanned <= 1);
        if progress.complete {
            break progress;
        }
    };
    assert_eq!(
        complete.counts.events,
        DIRECT_OWNER_EVENTS + INDIRECT_OWNER_EVENTS
    );
    let total_queries = first_queries + store.provider_file_reconciliation_queries.get();
    let total_candidates = first_candidates + store.provider_file_reconciliation_candidates.get();
    assert!(
        total_queries <= DIRECT_OWNER_EVENTS + UNRELATED_EVENTS + INDIRECT_OWNER_EVENTS + 64,
        "legacy reconciliation issued {total_queries} bounded scans"
    );
    assert!(
        total_candidates <= total_queries * 2,
        "legacy reconciliation materialized {total_candidates} candidates across {total_queries} scans"
    );
    assert_eq!(table_row_count(&store, "events"), UNRELATED_EVENTS as i64);
    let finalized = store
        .finalize_provider_file_publication(
            scope,
            outcome,
            ProviderFilePublicationCommit::Replacement(None),
        )
        .unwrap();
    assert_eq!(finalized.reconciliation, complete.counts);
}

#[test]
fn legacy_reconciliation_sql_forces_only_v46_indexes() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    for phase in 0..CLEANUP_PHASE_COMPLETE {
        let spec = legacy_reconciliation_phase_spec(phase).unwrap();
        assert!(!spec.direct_owner_select_sql.contains("idx_reconcile_"));
        let direct_plan = store
            .conn
            .prepare(&format!(
                "EXPLAIN QUERY PLAN {}",
                spec.direct_owner_select_sql
            ))
            .unwrap()
            .query_map(params!["source", Option::<i64>::None, 2], |row| {
                row.get::<_, String>(3)
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .join("\n");
        assert!(
            !direct_plan.contains("USE TEMP B-TREE FOR ORDER BY"),
            "legacy direct phase {phase} uses a temp ordering plan:\n{direct_plan}"
        );
        if let Some(indirect_sql) = spec.indirect_owner_scan_sql {
            assert!(!indirect_sql.contains("INDEXED BY idx_reconcile_"));
            assert!(indirect_sql.contains("ORDER BY entity.rowid LIMIT ?3"));
        }
    }
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
        stage_test_completion(&store, &scope);
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
