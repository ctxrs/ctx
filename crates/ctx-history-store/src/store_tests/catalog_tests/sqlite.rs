#[allow(unused_imports)]
use super::*;

pub(crate) fn assert_sql_conversion_error<T: std::fmt::Debug>(result: Result<T>) {
    assert!(
        matches!(result, Err(StoreError::Sql(_))),
        "expected sqlite conversion error, got {result:?}"
    );
}

#[test]
pub(crate) fn events_for_session_window_returns_bounded_neighbors() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let session = imported_session("window-session");
    store.upsert_session(&session).unwrap();
    let events = (0..10)
        .map(|index| {
            let event = session_event(session.id, index);
            store.upsert_event(&event).unwrap();
            event
        })
        .collect::<Vec<_>>();

    let middle = store
        .events_for_session_window(&events[5], 2, 3)
        .unwrap()
        .into_iter()
        .map(|event| event.seq)
        .collect::<Vec<_>>();
    assert_eq!(middle, vec![3, 4, 5, 6, 7, 8]);

    let first = store
        .events_for_session_window(&events[0], 50, 1)
        .unwrap()
        .into_iter()
        .map(|event| event.seq)
        .collect::<Vec<_>>();
    assert_eq!(first, vec![0, 1]);

    let last = store
        .events_for_session_window(&events[9], 1, 50)
        .unwrap()
        .into_iter()
        .map(|event| event.seq)
        .collect::<Vec<_>>();
    assert_eq!(last, vec![8, 9]);
}

#[test]
pub(crate) fn sessions_by_external_session_limited_caps_ambiguity_scan() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    for index in 0..5 {
        let mut session = imported_session("shared-provider-session");
        session.started_at = fixed_time() + chrono::Duration::seconds(index);
        store.upsert_session(&session).unwrap();
    }

    let matches = store
        .sessions_by_external_session_limited(CaptureProvider::Codex, "shared-provider-session", 2)
        .unwrap();

    assert_eq!(matches.len(), 2);
    assert_eq!(
        matches[0].external_session_id.as_deref(),
        Some("shared-provider-session")
    );
}

#[test]
pub(crate) fn search_index_optimize_is_safe_on_initialized_store() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    store.optimize_search_index().unwrap();
}

#[test]
pub(crate) fn ctx_files_touched_resolves_session_from_source_id() {
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
pub(crate) fn row_readers_reject_negative_unsigned_columns() {
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
        redaction_state: RedactionState::LocalPreview,
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
pub(crate) fn schema_v8_migrates_legacy_history_record_table_names() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(&legacy_history_record_sql(CREATE_TABLES_SQL))
            .unwrap();
        conn.execute_batch(&legacy_history_record_sql(FTS_TABLES_SQL))
            .unwrap();
        let record_id = new_id();
        conn.execute(
            "INSERT INTO work_records (id, title, last_activity_at_ms, body, created_at, updated_at)
             VALUES (?1, 'Legacy record', 0, '', '2026-06-23T12:00:00+00:00', '2026-06-23T12:00:00+00:00')",
            [record_id.to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions
             (id, work_record_id, provider, agent_type, is_primary, status, fidelity, started_at_ms, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, 'codex', 'primary', 1, 'imported', 'partial', 0, 0, 0)",
            params![new_id().to_string(), record_id.to_string()],
        )
        .unwrap();
        conn.execute_batch("PRAGMA user_version = 7;").unwrap();
    }

    let store = Store::open(&path).unwrap();
    assert!(table_exists(&store.conn, "history_records").unwrap());
    assert!(!table_exists(&store.conn, "work_records").unwrap());
    assert!(table_exists(&store.conn, "history_record_links").unwrap());
    assert!(!table_exists(&store.conn, "work_record_links").unwrap());
    for table in ["sessions", "runs", "events", "summaries", "files_touched"] {
        assert!(table_has_column(&store.conn, table, "history_record_id").unwrap());
        assert!(!table_has_column(&store.conn, table, "work_record_id").unwrap());
    }
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
}

#[test]
pub(crate) fn schema_v12_invalidates_provider_import_indexes_for_reimport() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(CREATE_TABLES_SQL).unwrap();
        conn.execute(
            r#"
            INSERT INTO catalog_sessions
            (
                source_path, provider, source_format, source_root, external_session_id,
                agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms,
                indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms,
                indexed_status, indexed_event_count
            )
            VALUES
            (
                '/tmp/codex/session.jsonl', 'codex', 'codex_rollout_jsonl', '/tmp/codex',
                'session-1', 'primary', 10, 20, 30, 40, 10, 20, 'indexed', 5
            )
            "#,
            [],
        )
        .unwrap();
        conn.execute(
            r#"
            INSERT INTO source_import_files
            (
                provider, source_format, source_root, source_path,
                file_size_bytes, file_modified_at_ms, observed_at_ms,
                indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms,
                indexed_status
            )
            VALUES
            (
                'antigravity', 'antigravity_cli_transcript_jsonl', '/tmp/agy',
                '/tmp/agy/transcript.jsonl', 10, 20, 30, 40, 10, 20, 'indexed'
            )
            "#,
            [],
        )
        .unwrap();
        conn.execute_batch("PRAGMA user_version = 11;").unwrap();
    }

    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    let catalog_status: (String, Option<i64>, Option<i64>, Option<i64>, Option<i64>) = store
        .conn
        .query_row(
            "SELECT indexed_status, indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms, indexed_event_count FROM catalog_sessions",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .unwrap();
    assert_eq!(
        catalog_status,
        ("pending".to_owned(), None, None, None, None)
    );

    let file_status: (String, Option<i64>, Option<i64>, Option<i64>) = store
        .conn
        .query_row(
            "SELECT indexed_status, indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms FROM source_import_files",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(file_status, ("pending".to_owned(), None, None, None));
}

#[test]
pub(crate) fn schema_v16_rebuilds_provider_checks_with_referenced_sources_and_indexes() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    let source_id = new_id();
    let session_id;
    let event_id;
    {
        let store = Store::open(&path).unwrap();
        let source = CaptureSource {
            id: source_id,
            descriptor: CaptureSourceDescriptor {
                kind: ctx_history_core::CaptureSourceKind::ProviderImport,
                provider: CaptureProvider::Codex,
                machine_id: "test-machine".to_owned(),
                process_id: None,
                cwd: Some("/repo".to_owned()),
                raw_source_path: Some("/home/user/.codex/sessions/session.jsonl".to_owned()),
                external_session_id: Some("codex-session-1".to_owned()),
            },
            started_at: fixed_time(),
            ended_at: None,
            sync: sync_metadata(),
        };
        store.upsert_capture_source(&source).unwrap();

        let mut session = imported_session("codex-session-1");
        session.capture_source_id = Some(source_id);
        session_id = session.id;
        store.upsert_session(&session).unwrap();

        let event = Event {
            id: new_id(),
            seq: 0,
            history_record_id: None,
            session_id: Some(session_id),
            run_id: None,
            event_type: EventType::Message,
            role: Some(EventRole::User),
            occurred_at: fixed_time(),
            capture_source_id: Some(source_id),
            payload: serde_json::json!({"text": "migration source reference"}),
            payload_blob_id: None,
            dedupe_key: None,
            redaction_state: RedactionState::LocalPreview,
            sync: sync_metadata(),
        };
        event_id = event.id;
        store.upsert_event(&event).unwrap();
        store
            .conn
            .execute_batch("PRAGMA user_version = 14;")
            .unwrap();
    }

    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let source_refs: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sessions s JOIN events e ON e.session_id = s.id \
             WHERE s.id = ?1 AND e.id = ?2 AND s.capture_source_id = ?3 AND e.capture_source_id = ?3",
            params![session_id.to_string(), event_id.to_string(), source_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(source_refs, 1);
    for index in [
        "idx_capture_sources_external_session_id",
        "idx_catalog_sessions_provider_source_root_import",
        "idx_source_import_files_provider_source_root_import",
    ] {
        let exists: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
                [index],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1, "missing rebuilt index {index}");
    }
}

pub(crate) fn assert_provider_migration_accepts(
    legacy_version: i64,
    provider: &str,
    source_format: &str,
    source_root: &str,
    source_path: &str,
) {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        let legacy_sql = CREATE_TABLES_SQL.replace(&format!(", '{provider}'"), "");
        conn.execute_batch(&legacy_sql).unwrap();
        conn.execute_batch(INDEXES_SQL).unwrap();
        conn.execute_batch(&format!("PRAGMA user_version = {legacy_version};"))
            .unwrap();
    }

    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    store
        .conn
        .execute(
            r#"
            INSERT INTO capture_sources
            (id, kind, provider, machine_id, started_at_ms, fidelity)
            VALUES (?1, 'provider_import', ?2, 'test-machine', 0, 'imported')
            "#,
            params![new_id().to_string(), provider],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO catalog_sessions
            (source_path, provider, source_format, source_root, agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms)
            VALUES (?1, ?2, ?3, ?4, 'primary', 1, 0, 0)
            "#,
            params![source_path, provider, source_format, source_root],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO source_import_files
            (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms)
            VALUES (?1, ?2, ?3, ?4, 1, 0, 0)
            "#,
            params![provider, source_format, source_root, source_path],
        )
        .unwrap();
}
