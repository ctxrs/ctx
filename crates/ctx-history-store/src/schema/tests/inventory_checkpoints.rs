#[test]
fn fresh_v57_inventory_checkpoint_schema_has_no_mirrored_directory_queue() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    assert_eq!(crate::current_history_store_schema_version(), 57);
    let tables = store
        .conn
        .prepare(
            "SELECT name FROM sqlite_schema \
             WHERE type = 'table' AND name LIKE 'import_inventory_%' ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(tables.contains(&"import_inventory_runs".to_owned()));
    assert!(tables.contains(&"import_inventory_checkpoints".to_owned()));
    assert!(tables.contains(&"import_inventory_path_effects".to_owned()));
    assert!(!tables.contains(&"import_inventory_directory_work".to_owned()));
    let checkpoint_columns = store
        .conn
        .prepare("SELECT name FROM pragma_table_info('import_inventory_checkpoints')")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    for required in [
        "scratch_database_identity",
        "selection_keyset",
        "selection_eof",
        "selection_complete",
        "selection_commitment_identity",
        "application_ordinal",
        "application_prefix",
        "store_reconciliation_keyset",
        "store_reconciliation_complete",
        "store_reconciliation_visited_rows",
        "store_reconciliation_stale_rows",
        "store_reconciliation_visited_bytes",
        "cleanup_visited_row_count",
        "cleanup_attempt_count",
    ] {
        assert!(checkpoint_columns.iter().any(|column| column == required));
    }
    let mirrored_index: bool = store
        .conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema \
             WHERE name = 'idx_import_inventory_directory_queue_selection')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!mirrored_index);

    let recovery_plan = store
        .conn
        .prepare(
            "EXPLAIN QUERY PLAN \
             SELECT checkpoint.run_id \
             FROM import_inventory_generations AS generation \
             JOIN import_inventory_checkpoints AS checkpoint \
               ON checkpoint.inventory_family = generation.inventory_family \
              AND checkpoint.provider = generation.provider \
              AND checkpoint.source_root = generation.source_root \
              AND checkpoint.inventory_generation = generation.current_generation \
             JOIN import_inventory_runs AS run ON run.run_id = checkpoint.run_id \
             WHERE generation.inventory_family = 'catalog_sessions' \
               AND generation.provider = 'codex' AND generation.source_root = '/tmp/root' \
               AND checkpoint.status IN ('active', 'abandoned', 'cleaning')",
        )
        .unwrap()
        .query_map([], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(recovery_plan
        .iter()
        .all(|detail| !detail.contains("SCAN checkpoint") && !detail.contains("TEMP B-TREE")));
    assert!(recovery_plan
        .iter()
        .any(|detail| detail.contains("SEARCH checkpoint USING INDEX")));
    for (table, index) in [
        (
            "catalog_sessions",
            "idx_catalog_sessions_provider_source_root_stale",
        ),
        (
            "source_import_files",
            "idx_source_import_files_provider_source_root_stale",
        ),
    ] {
        let plan = store
            .conn
            .prepare(&format!(
                "EXPLAIN QUERY PLAN SELECT rowid, length(CAST(source_path AS BLOB)) \
                 FROM {table} INDEXED BY {index} \
                 WHERE provider = 'codex' AND source_root = '/tmp/root' \
                   AND is_stale = 0 AND rowid > 0 ORDER BY rowid LIMIT 64"
            ))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(plan.iter().any(|detail| detail.contains(index)), "{plan:?}");
        assert!(
            plan.iter().all(|detail| !detail.contains("TEMP B-TREE")),
            "{plan:?}"
        );
    }
}

fn assert_historical_schema_adds_empty_inventory_checkpoints_without_corpus_churn(
    fixture: &str,
    filename: &str,
) {
    let temp = tempdir();
    let path = temp.path().join(filename);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(fixture).unwrap();
    conn.execute(
        "INSERT INTO catalog_sessions (\
           source_path, provider, source_format, source_root, agent_type, \
           file_size_bytes, file_modified_at_ms, import_revision, cataloged_at_ms, \
           is_stale, metadata_json\
         ) VALUES (?1, 'codex', 'codex_session_jsonl', ?2, 'primary', \
                   1, 1, 1, 1, 0, '{}')",
        params!["/tmp/session.jsonl", "/tmp/codex"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO source_import_files (\
               provider, source_format, source_root, source_path, file_size_bytes, \
               file_modified_at_ms, import_revision, observed_at_ms, is_stale, metadata_json\
             ) VALUES ('codex', 'codex_session_jsonl', ?1, ?2, 1, 1, 1, 1, 0, '{}')",
        params!["/tmp/codex", "/tmp/source.jsonl"],
    )
    .unwrap();
    let corpus_rootpages_before = conn
        .prepare(
            "SELECT type, name, rootpage FROM sqlite_schema \
             WHERE tbl_name IN ('capture_sources', 'catalog_sessions', \
                                'source_import_files', 'sessions', 'events') \
               AND rootpage > 0 ORDER BY type, name",
        )
        .unwrap();
    let corpus_rootpages_before = corpus_rootpages_before
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    drop(conn);

    let reopened = Store::open(&path).unwrap();
    let corpus_rootpages_after = reopened
        .conn
        .prepare(
            "SELECT type, name, rootpage FROM sqlite_schema \
             WHERE tbl_name IN ('capture_sources', 'catalog_sessions', \
                                'source_import_files', 'sessions', 'events') \
               AND rootpage > 0 ORDER BY type, name",
        )
        .unwrap();
    let corpus_rootpages_after = corpus_rootpages_after
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(corpus_rootpages_after, corpus_rootpages_before);
    for table in [
        "import_inventory_runs",
        "import_inventory_checkpoints",
        "import_inventory_path_effects",
    ] {
        let count: i64 = reopened
            .conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
    let corpus_rows: i64 = reopened
        .conn
        .query_row("SELECT COUNT(*) FROM catalog_sessions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(corpus_rows, 1);
}

#[test]
fn schema_v46_fixture_adds_empty_inventory_checkpoints_without_corpus_churn() {
    assert_historical_schema_adds_empty_inventory_checkpoints_without_corpus_churn(
        include_str!("../fixtures/schema_v46.sql"),
        "schema-v46.sqlite",
    );
}

#[test]
fn schema_v53_fixture_adds_empty_inventory_checkpoints_without_corpus_churn() {
    assert_historical_schema_adds_empty_inventory_checkpoints_without_corpus_churn(
        include_str!("../fixtures/schema_v53.sql"),
        "schema-v53.sqlite",
    );
}

#[test]
fn v57_open_rejects_a_second_main_store_directory_queue() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    let store = Store::open(&path).unwrap();
    store
        .conn
        .execute_batch("CREATE TABLE import_inventory_directory_work (id INTEGER PRIMARY KEY);")
        .unwrap();
    drop(store);

    let error = match Store::open(&path) {
        Ok(_) => panic!("mirrored main-store directory queue was accepted"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        StoreError::ImportInventorySchemaIncompatible(
            "durable inventory directory queue must be owned only by capture scratch"
        )
    ));
}

#[test]
fn v57_open_recreates_an_empty_checkpoint_schema_with_an_incompatible_key() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    let store = Store::open(&path).unwrap();
    store
        .conn
        .execute_batch(
            "DROP TABLE import_inventory_path_effects; \
             DROP TABLE import_inventory_checkpoints; \
             DROP TABLE import_inventory_runs;",
        )
        .unwrap();
    let incompatible = crate::schema::ddl::IMPORT_INVENTORY_CHECKPOINT_TABLES_SQL.replace(
        "    UNIQUE (\n      inventory_family, provider, source_root, inventory_generation\n    ),\n",
        "",
    );
    store.conn.execute_batch(&incompatible).unwrap();
    drop(store);

    let reopened = Store::open(&path).unwrap();
    let source_root_key: i64 = reopened
        .conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_index_list('import_inventory_checkpoints') AS list \
             WHERE list.[unique] = 1 AND list.origin = 'u' AND EXISTS (\
               SELECT 1 FROM pragma_index_xinfo(list.name) \
               WHERE key = 1 AND name = 'source_root'\
             )",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(source_root_key > 0);
}

fn insert_nonempty_inventory_checkpoint_fixture(conn: &Connection) {
    conn.execute(
        "INSERT INTO import_inventory_generations (\
           provider, source_root, inventory_family, current_generation\
         ) VALUES ('codex', '/fixture/root', 'catalog_sessions', 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO import_inventory_runs (\
           run_id, checkpoint_format_version, producer_build_id, store_schema_version, \
           publication_state_marker, publication_owner_present, created_at_ms, updated_at_ms\
         ) VALUES (zeroblob(32), 1, zeroblob(32), 57, ?1, 0, 1, 1)",
        ["b558de1f76ca5db8c121394c9b0c61802f8de569b199244b17c5693487f18c99"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO import_inventory_checkpoints (\
           run_id, inventory_family, provider, source_format, source_root, \
           source_identity, source_fingerprint, root_platform_tag, root_encoding_tag, \
           root_path_hash, inventory_generation, scratch_identity, scratch_integrity, \
           scratch_lock_identity, scratch_database_identity, application_prefix, \
           created_at_ms, updated_at_ms\
         ) VALUES (\
           zeroblob(32), 'catalog_sessions', 'codex', 'codex_session_jsonl', \
           '/fixture/root', zeroblob(32), zeroblob(32), 'unix', 'unix_bytes', \
           zeroblob(32), 1, zeroblob(32), zeroblob(32), zeroblob(32), zeroblob(32), \
           zeroblob(32), 1, 1\
         )",
        [],
    )
    .unwrap();
}

#[test]
fn v57_open_rejects_nonempty_effect_schemas_with_malformed_journal_or_foreign_keys() {
    for (name, incompatible) in [
        (
            "journal-key",
            crate::schema::ddl::IMPORT_INVENTORY_CHECKPOINT_TABLES_SQL.replace(
                "    UNIQUE (\n      run_id, inventory_family, provider, source_root, inventory_generation,\n      capture_journal_identity\n    ),\n",
                "",
            ),
        ),
        (
            "journal-order",
            crate::schema::ddl::IMPORT_INVENTORY_CHECKPOINT_TABLES_SQL.replace(
                "      run_id, inventory_family, provider, source_root, inventory_generation,\n      capture_journal_identity\n    ),\n",
                "      run_id, provider, inventory_family, source_root, inventory_generation,\n      capture_journal_identity\n    ),\n",
            ),
        ),
        (
            "journal-descending",
            crate::schema::ddl::IMPORT_INVENTORY_CHECKPOINT_TABLES_SQL.replace(
                "      capture_journal_identity\n    ),\n",
                "      capture_journal_identity DESC\n    ),\n",
            ),
        ),
        (
            "journal-collation",
            crate::schema::ddl::IMPORT_INVENTORY_CHECKPOINT_TABLES_SQL.replace(
                "      capture_journal_identity\n    ),\n",
                "      capture_journal_identity COLLATE NOCASE\n    ),\n",
            ),
        ),
        (
            "effect-fk",
            crate::schema::ddl::IMPORT_INVENTORY_CHECKPOINT_TABLES_SQL.replace(
                "    FOREIGN KEY (run_id, inventory_family, provider, source_root)\n      REFERENCES import_inventory_checkpoints(run_id, inventory_family, provider, source_root)\n      ON DELETE CASCADE,\n",
                "",
            ),
        ),
    ] {
        let temp = tempdir();
        let path = temp.path().join(format!("{name}.sqlite"));
        let store = Store::open(&path).unwrap();
        store
            .conn
            .execute_batch(
                "DROP TABLE import_inventory_path_effects; \
                 DROP TABLE import_inventory_checkpoints; \
                 DROP TABLE import_inventory_runs;",
            )
            .unwrap();
        store.conn.execute_batch(&incompatible).unwrap();
        insert_nonempty_inventory_checkpoint_fixture(&store.conn);
        store
            .conn
            .execute(
                "INSERT INTO import_inventory_path_effects (\
                   run_id, inventory_family, provider, source_root, inventory_generation, \
                   capture_journal_identity, path_platform_tag, path_encoding_tag, \
                   native_path_hash, source_path, effect_kind, \
                   selection_commitment_identity, selection_ordinal, \
                   resulting_application_keyset, prior_application_prefix, \
                   resulting_application_prefix, payload_fingerprint, member_digest, \
                   owner_epoch, prior_applied_row_count, resulting_applied_row_count, \
                   prior_applied_bytes, resulting_applied_bytes, affected_row_count, \
                   affected_bytes, applied_at_ms\
                 ) VALUES (\
                   zeroblob(32), 'catalog_sessions', 'codex', '/fixture/root', 1, \
                   randomblob(32), 'unix', 'unix_bytes', randomblob(32), '/fixture/a', \
                   'catalog_rejected', randomblob(32), 0, randomblob(32), randomblob(32), \
                   randomblob(32), randomblob(32), randomblob(32), 1, 0, 0, 0, 0, 0, 0, 1\
                 )",
                [],
            )
            .unwrap();
        drop(store);

        let error = match Store::open(&path) {
            Ok(_) => panic!("malformed nonempty effect schema was accepted"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            StoreError::ImportInventorySchemaIncompatible(
                "nonempty durable inventory checkpoint schema is incompatible"
            )
        ));
    }
}

#[test]
fn v57_open_rejects_nonempty_run_or_checkpoint_ownership_shape_changes() {
    for (name, incompatible) in [
        (
            "run-owner",
            crate::schema::ddl::IMPORT_INVENTORY_CHECKPOINT_TABLES_SQL.replace(
                "          AND publication_import_revision IS NOT NULL)\n    )\n) WITHOUT ROWID;",
                "          AND publication_import_revision IS NOT NULL)\n    )\n) WITHOUT ROWID;",
            )
            .replace("publication_owner_present INTEGER NOT NULL", "publication_owner_present INTEGER"),
        ),
        (
            "checkpoint-owner",
            crate::schema::ddl::IMPORT_INVENTORY_CHECKPOINT_TABLES_SQL.replace(
                "      OR (owner_state != 'inactive' AND owner_token IS NOT NULL\n          AND length(lease_owner_id) BETWEEN 1 AND 256 AND lease_expires_at_ms IS NOT NULL)\n    ),\n",
                "    ),\n",
            ),
        ),
    ] {
        let temp = tempdir();
        let path = temp.path().join(format!("{name}.sqlite"));
        let store = Store::open(&path).unwrap();
        store
            .conn
            .execute_batch(
                "DROP TABLE import_inventory_path_effects; \
                 DROP TABLE import_inventory_checkpoints; \
                 DROP TABLE import_inventory_runs;",
            )
            .unwrap();
        store.conn.execute_batch(&incompatible).unwrap();
        insert_nonempty_inventory_checkpoint_fixture(&store.conn);
        drop(store);

        let error = match Store::open(&path) {
            Ok(_) => panic!("malformed nonempty ownership schema was accepted"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            StoreError::ImportInventorySchemaIncompatible(
                "nonempty durable inventory checkpoint schema is incompatible"
            )
        ));
    }
}
