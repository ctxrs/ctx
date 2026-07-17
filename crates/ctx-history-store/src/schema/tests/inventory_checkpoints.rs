#[test]
fn fresh_v57_inventory_checkpoint_schema_has_no_mirrored_directory_queue() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
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
}

#[test]
fn schema_v56_adds_only_empty_inventory_checkpoint_state_and_preserves_corpus_rootpage() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    let store = Store::open(&path).unwrap();
    store
        .conn
        .execute(
            "INSERT INTO catalog_sessions (\
               source_path, provider, source_format, source_root, agent_type, \
               file_size_bytes, file_modified_at_ms, import_revision, cataloged_at_ms, \
               is_stale, metadata_json\
             ) VALUES (?1, 'codex', 'codex_session_jsonl', ?2, 'primary', \
                       1, 1, 1, 1, 0, '{}')",
            params!["/tmp/session.jsonl", "/tmp/codex"],
        )
        .unwrap();
    let rootpage_before: i64 = store
        .conn
        .query_row(
            "SELECT rootpage FROM sqlite_schema WHERE type = 'table' AND name = 'catalog_sessions'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    store
        .conn
        .execute_batch(
            "DROP TABLE import_inventory_path_effects; \
             DROP TABLE import_inventory_checkpoints; \
             DROP TABLE import_inventory_runs; \
             PRAGMA user_version = 56;",
        )
        .unwrap();
    drop(store);

    let reopened = Store::open(&path).unwrap();
    let rootpage_after: i64 = reopened
        .conn
        .query_row(
            "SELECT rootpage FROM sqlite_schema WHERE type = 'table' AND name = 'catalog_sessions'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rootpage_after, rootpage_before);
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
fn v57_open_rejects_a_checkpoint_table_without_the_source_generation_key() {
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

    let error = match Store::open(&path) {
        Ok(_) => panic!("checkpoint table without source-generation key was accepted"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        StoreError::ImportInventorySchemaIncompatible(
            "durable inventory checkpoint schema shape is incompatible"
        )
    ));
}
