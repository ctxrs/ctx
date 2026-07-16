#[test]
fn catalog_schema_includes_import_state_columns() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let schema = store.schema().unwrap();
    assert!(schema.contains("indexed_at_ms INTEGER"));
    assert!(schema.contains("indexed_file_size_bytes INTEGER"));
    assert!(schema.contains("indexed_file_modified_at_ms INTEGER"));
    assert!(schema.contains("indexed_status TEXT NOT NULL DEFAULT 'pending'"));
    assert!(schema.contains("indexed_error TEXT"));
    assert!(schema.contains("indexed_event_count INTEGER"));
    assert!(schema.contains("last_imported_at_ms INTEGER"));
    assert!(schema.contains("last_imported_file_size_bytes INTEGER"));
    assert!(schema.contains("last_imported_file_modified_at_ms INTEGER"));
    assert!(schema.contains("last_imported_file_sha256 TEXT"));
    assert!(schema.contains("last_imported_event_count INTEGER"));
    assert!(schema.contains("CREATE TABLE import_inventory_generations"));
}

#[test]
fn raw_sql_query_reads_stable_views() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let schema = store.schema().unwrap();
    for view in [
        "CREATE VIEW ctx_sessions",
        "CREATE VIEW ctx_events",
        "CREATE VIEW ctx_files_touched",
        "CREATE VIEW ctx_sources",
    ] {
        assert!(schema.contains(view), "schema missing {view}");
    }

    let result = store
        .raw_sql_query(
            "SELECT COUNT(*) AS session_count FROM ctx_sessions",
            RawSqlOptions::default(),
        )
        .unwrap();
    assert_eq!(result.columns[0].name, "session_count");
    assert_eq!(result.returned_rows, 1);
    assert_eq!(result.rows[0][0], RawSqlValue::Integer(0));
}

#[test]
fn ctx_files_touched_resolves_session_from_source_id() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let record_id = "018f45d0-0000-7000-8000-000000080001";
    let source_id = "018f45d0-0000-7000-8000-000000080002";
    let session_id = "018f45d0-0000-7000-8000-000000080003";
    let touch_id = "018f45d0-0000-7000-8000-000000080004";
    let detached_source_id = "018f45d0-0000-7000-8000-000000080005";
    let detached_touch_id = "018f45d0-0000-7000-8000-000000080006";

    store
        .conn
        .execute(
            r#"
            INSERT INTO history_records
            (id, title, last_activity_at_ms, created_at_ms, updated_at_ms, body, created_at, updated_at)
            VALUES (?1, 'Touched file view record', 1, 1, 1, '', '', '')
            "#,
            [record_id],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO capture_sources
            (id, kind, provider, machine_id, raw_source_path, external_session_id, started_at_ms, fidelity)
            VALUES (?1, 'provider_import', 'codex', 'test-machine', '/tmp/session.jsonl', 'codex-session-1', 1, 'imported')
            "#,
            [source_id],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO capture_sources
            (id, kind, provider, machine_id, raw_source_path, external_session_id, started_at_ms, fidelity)
            VALUES (?1, 'provider_import', 'opencode', 'test-machine', '/tmp/opencode.db', 'opencode-session-1', 1, 'imported')
            "#,
            [detached_source_id],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO sessions
            (
                id, history_record_id, capture_source_id, provider, external_session_id,
                agent_type, is_primary, status, fidelity, started_at_ms, created_at_ms, updated_at_ms
            )
            VALUES (?1, ?2, ?3, 'codex', 'codex-session-1', 'primary', 1, 'imported', 'imported', 1, 1, 1)
            "#,
            params![session_id, record_id, source_id],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO files_touched
            (id, source_id, path, change_kind, confidence, created_at_ms, updated_at_ms, fidelity)
            VALUES (?1, ?2, 'src/main.rs', 'modified', 'explicit', 1, 1, 'imported')
            "#,
            params![touch_id, source_id],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO files_touched
            (id, source_id, path, change_kind, confidence, created_at_ms, updated_at_ms, fidelity)
            VALUES (?1, ?2, 'detached.rs', 'modified', 'explicit', 1, 1, 'imported')
            "#,
            params![detached_touch_id, detached_source_id],
        )
        .unwrap();

    let result = store
        .raw_sql_query(
            "SELECT provider, provider_session_id, ctx_session_id, history_record_id FROM ctx_files_touched WHERE path = 'src/main.rs'",
            RawSqlOptions::default(),
        )
        .unwrap();
    assert_eq!(result.returned_rows, 1);
    assert_eq!(
        result.rows[0][0],
        RawSqlValue::Text {
            value: "codex".to_owned(),
            bytes: 5,
            truncated: false,
        }
    );
    assert_eq!(
        result.rows[0][1],
        RawSqlValue::Text {
            value: "codex-session-1".to_owned(),
            bytes: 15,
            truncated: false,
        }
    );
    assert_eq!(
        result.rows[0][2],
        RawSqlValue::Text {
            value: session_id.to_owned(),
            bytes: session_id.len(),
            truncated: false,
        }
    );
    assert_eq!(
        result.rows[0][3],
        RawSqlValue::Text {
            value: record_id.to_owned(),
            bytes: record_id.len(),
            truncated: false,
        }
    );

    let detached = store
        .raw_sql_query(
            "SELECT provider, provider_session_id, ctx_session_id, history_record_id FROM ctx_files_touched WHERE path = 'detached.rs'",
            RawSqlOptions::default(),
        )
        .unwrap();
    assert_eq!(detached.returned_rows, 1);
    assert_eq!(
        detached.rows[0][0],
        RawSqlValue::Text {
            value: "opencode".to_owned(),
            bytes: 8,
            truncated: false,
        }
    );
    assert_eq!(
        detached.rows[0][1],
        RawSqlValue::Text {
            value: "opencode-session-1".to_owned(),
            bytes: 18,
            truncated: false,
        }
    );
    assert_eq!(detached.rows[0][2], RawSqlValue::Null);
    assert_eq!(detached.rows[0][3], RawSqlValue::Null);
}

#[test]
fn raw_sql_query_rejects_writes_parameters_and_multiple_statements() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();

    assert!(matches!(
        store
            .raw_sql_query("", RawSqlOptions::default())
            .unwrap_err(),
        StoreError::RawSqlEmpty
    ));
    assert!(matches!(
        store
            .raw_sql_query("SELECT ?1", RawSqlOptions::default())
            .unwrap_err(),
        StoreError::RawSqlHasParameters
    ));
    assert!(matches!(
        store
            .raw_sql_query("CREATE TABLE nope(x INTEGER)", RawSqlOptions::default())
            .unwrap_err(),
        StoreError::RawSqlNotReadOnly
    ));
    assert!(matches!(
        store
            .raw_sql_query("SELECT 1; SELECT 2", RawSqlOptions::default())
            .unwrap_err(),
        StoreError::Sql(rusqlite::Error::MultipleStatement)
    ));
}

#[test]
fn raw_sql_query_caps_rows_and_values() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let result = store
        .raw_sql_query(
            "SELECT 'abcdef' AS text_value, X'01020304' AS blob_value UNION ALL SELECT 'ghijkl', X'05060708'",
            RawSqlOptions {
                max_rows: 1,
                max_value_bytes: 3,
                ..RawSqlOptions::default()
            },
        )
        .unwrap();
    assert_eq!(result.returned_rows, 1);
    assert_eq!(result.columns[0].name, "text_value");
    assert_eq!(result.columns[1].name, "blob_value");
    assert_eq!(
        result.rows[0][0],
        RawSqlValue::Text {
            value: "abc".to_owned(),
            bytes: 6,
            truncated: true,
        }
    );
    assert_eq!(
        result.rows[0][1],
        RawSqlValue::Blob {
            bytes: 4,
            preview_hex: "010203".to_owned(),
            truncated: true,
        }
    );
    assert!(result.truncated.rows);
    assert!(result.truncated.values);
}

#[test]
fn row_readers_reject_negative_unsigned_columns() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let bad_process_id = new_id();
    store
        .conn
        .execute(
            r#"
            INSERT INTO capture_sources
            (
                id, kind, provider, machine_id, process_id, cwd, raw_source_path,
                external_session_id, started_at_ms, fidelity, sync_version
            )
            VALUES (?1, 'provider_import', 'codex', 'test-machine', -1, '/repo', '/tmp/session.jsonl', 'session', 1, 'imported', 0)
            "#,
            params![bad_process_id.to_string()],
        )
        .unwrap();
    assert_sql_conversion_error(store.get_capture_source(bad_process_id));

    let bad_sync_version = new_id();
    store
        .conn
        .execute(
            r#"
            INSERT INTO capture_sources
            (
                id, kind, provider, machine_id, cwd, raw_source_path,
                external_session_id, started_at_ms, fidelity, sync_version
            )
            VALUES (?1, 'provider_import', 'codex', 'test-machine', '/repo', '/tmp/session.jsonl', 'session', 1, 'imported', -1)
            "#,
            params![bad_sync_version.to_string()],
        )
        .unwrap();
    assert_sql_conversion_error(store.get_capture_source(bad_sync_version));

    let event = Event {
        id: new_id(),
        seq: 1,
        history_record_id: None,
        session_id: None,
        run_id: None,
        event_type: EventType::Message,
        role: Some(EventRole::Assistant),
        occurred_at: fixed_time(),
        capture_source_id: None,
        payload: serde_json::json!({"text": "negative seq marker"}),
        payload_blob_id: None,
        dedupe_key: None,
        sync: sync_metadata(),
    };
    store.upsert_event(&event).unwrap();
    store
        .conn
        .execute(
            "UPDATE events SET seq = -1 WHERE id = ?1",
            params![event.id.to_string()],
        )
        .unwrap();
    assert_sql_conversion_error(store.get_event(event.id));
    assert_sql_conversion_error(store.search_event_hits("negative seq marker", 1));

    let artifact = artifact_record(new_id(), 1);
    store.upsert_artifact(&artifact).unwrap();
    store
        .conn
        .execute(
            "UPDATE artifacts SET byte_size = -1 WHERE id = ?1",
            params![artifact.id.to_string()],
        )
        .unwrap();
    assert_sql_conversion_error(store.list_artifacts());
}

#[test]
fn raw_sql_query_rejects_excessive_result_preview_budget() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let many_columns = (0..RAW_SQL_MAX_COLUMNS_CAP)
        .map(|index| format!("1 AS c{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let err = store
        .raw_sql_query(
            &format!("SELECT {many_columns}"),
            RawSqlOptions {
                max_rows: RAW_SQL_MAX_ROWS_CAP,
                max_columns: RAW_SQL_MAX_COLUMNS_CAP,
                max_value_bytes: 32,
                ..RawSqlOptions::default()
            },
        )
        .unwrap_err();
    assert!(matches!(
        err,
        StoreError::RawSqlResultBudgetTooLarge {
            max_result_bytes: RAW_SQL_MAX_RESULT_PREVIEW_BYTES,
            ..
        }
    ));
}

#[test]
fn raw_sql_query_budgets_against_actual_column_count() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let result = store
        .raw_sql_query(
            "SELECT 1",
            RawSqlOptions {
                max_rows: RAW_SQL_MAX_ROWS_CAP,
                max_columns: RAW_SQL_MAX_COLUMNS_CAP,
                max_value_bytes: 32,
                ..RawSqlOptions::default()
            },
        )
        .unwrap();
    assert_eq!(result.returned_rows, 1);
    assert_eq!(result.rows[0][0], RawSqlValue::Integer(1));
}

#[test]
fn raw_sql_query_times_out_long_running_queries() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let err = store
        .raw_sql_query(
            r#"
            WITH RECURSIVE numbers(x) AS (
                SELECT 1
                UNION ALL
                SELECT x + 1 FROM numbers WHERE x < 100000000
            )
            SELECT sum(x) FROM numbers
            "#,
            RawSqlOptions {
                timeout: Duration::from_millis(1),
                ..RawSqlOptions::default()
            },
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::RawSqlTimedOut { .. }));
}

#[test]
fn raw_sql_query_enforces_sqlite_value_length_limit() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let err = store
        .raw_sql_query(
            "SELECT length(randomblob(200000))",
            RawSqlOptions::default(),
        )
        .unwrap_err();
    assert!(matches!(
        err,
        StoreError::Sql(rusqlite::Error::SqliteFailure(error, _))
            if error.code == ErrorCode::TooBig
    ));
}
