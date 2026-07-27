use std::{
    fs,
    path::{Path, PathBuf},
};

use ctx_history_core::new_id;
use rusqlite::{params, Connection};
use uuid::Uuid;

use super::fixtures::tempdir;
use crate::schema::ddl::{table_exists, table_has_column, CREATE_TABLES_SQL};
use crate::schema::fts::FTS_TABLES_SQL;
use crate::schema::indexes::INDEXES_SQL;
use crate::{Store, StoreError, FINAL_SCHEMA_IDENTITY, SCHEMA_VERSION};

fn legacy_history_record_sql(sql: &str) -> String {
    sql.replace("history_record_links", "work_record_links")
        .replace("history_record_tags", "work_record_tags")
        .replace("history_records", "work_records")
        .replace("history_record_id", "work_record_id")
}

fn seed_artifact_blob(
    store: &Store,
    object_dir: &Path,
    artifact_id: Uuid,
    content: &[u8],
) -> PathBuf {
    let blob_hash = crate::object_store::sha256_hex(content);
    let path = object_dir.join(&blob_hash[..2]).join(&blob_hash);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, content).unwrap();
    store
        .conn
        .execute(
            "INSERT INTO artifacts
             (id, kind, blob_hash, blob_path, byte_size, preview_text,
              created_at_ms, updated_at_ms, fidelity)
             VALUES (?1, 'stdout', ?2, ?3, ?4, ?5, 1, 1, 'imported')",
            params![
                artifact_id.to_string(),
                blob_hash,
                crate::object_store::object_relative_path(&blob_hash),
                content.len() as i64,
                String::from_utf8_lossy(content),
            ],
        )
        .unwrap();
    path
}

#[test]
fn same_version_v3_identity_purges_unreleased_locators_and_advances_to_final() {
    let temp = tempdir();
    let path = temp.path().join("verified-content.sqlite");
    let before = {
        let store = Store::open(&path).unwrap();
        store
            .conn
            .execute(
                "INSERT INTO events
                 (id, seq, event_type, occurred_at_ms, metadata_json)
                 VALUES (?1, 1, 'message', 1, ?2)",
                params![
                    Uuid::new_v4().to_string(),
                    serde_json::json!({
                        "complete_content_locator_v1": {"path": "/private"},
                        "result_content_locator_v1": {"path": "/private"},
                        "complete_content_body_sha256": "a".repeat(64),
                        "preserved": true
                    })
                    .to_string(),
                ],
            )
            .unwrap();
        store
            .activate_projection_journal(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .unwrap();
        let snapshot = store.projection_journal_snapshot(None).unwrap();
        store
            .conn
            .execute(
                "UPDATE ctx_store_schema_identity SET schema_identity = ?1 WHERE singleton = 1",
                ["ctx-store-schema-47-final-v3"],
            )
            .unwrap();
        snapshot
    };

    let store = Store::open(&path).unwrap();
    let after = store.projection_journal_snapshot(None).unwrap();
    let (identity, metadata): (String, String) = store
        .conn
        .query_row(
            "SELECT i.schema_identity, e.metadata_json
             FROM ctx_store_schema_identity i CROSS JOIN events e
             WHERE i.singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(identity, FINAL_SCHEMA_IDENTITY);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&metadata).unwrap(),
        serde_json::json!({"preserved": true})
    );
    assert_eq!(after.frozen_through, before.frozen_through);
    assert_eq!(after.records.len(), before.records.len());
    assert_eq!(
        after
            .records
            .iter()
            .map(|record| (record.stable_entity_id, record.entity_revision))
            .collect::<Vec<_>>(),
        before
            .records
            .iter()
            .map(|record| (record.stable_entity_id, record.entity_revision))
            .collect::<Vec<_>>()
    );
}

#[test]
fn same_version_v2_identity_runs_source_backed_then_locator_cleanup() {
    let temp = tempdir();
    let path = temp.path().join("v2-to-v4.sqlite");
    {
        let store = Store::open(&path).unwrap();
        store
            .conn
            .execute(
                "INSERT INTO events
                 (id, seq, event_type, occurred_at_ms, metadata_json)
                 VALUES (?1, 1, 'message', 1, ?2)",
                params![
                    Uuid::new_v4().to_string(),
                    serde_json::json!({
                        "complete_content_locator_v1": {"path": "/private"},
                        "preserved": true
                    })
                    .to_string(),
                ],
            )
            .unwrap();
        store
            .conn
            .execute(
                "UPDATE ctx_store_schema_identity SET schema_identity = ?1 WHERE singleton = 1",
                ["ctx-store-schema-47-final-v2"],
            )
            .unwrap();
    }

    let store = Store::open(&path).unwrap();
    let (identity, metadata): (String, String) = store
        .conn
        .query_row(
            "SELECT i.schema_identity, e.metadata_json
             FROM ctx_store_schema_identity i CROSS JOIN events e
             WHERE i.singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(identity, FINAL_SCHEMA_IDENTITY);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&metadata).unwrap(),
        serde_json::json!({"preserved": true})
    );
}

#[test]
fn same_version_v4_route_backfill_is_conservative_and_preserves_journal() {
    let temp = tempdir();
    let path = temp.path().join("provider-routes.sqlite");
    let source_id = Uuid::new_v4();
    let ambiguous_source_id = Uuid::new_v4();
    let event_id = Uuid::new_v4();
    let exact_path = temp.path().join("provider/session.jsonl");
    let exact_path = exact_path.to_string_lossy().into_owned();
    let journal_before;
    {
        let store = Store::open(&path).unwrap();
        store
            .conn
            .execute(
                "INSERT INTO capture_sources
                 (id, kind, provider, machine_id, raw_source_path, source_format,
                  source_identity, external_session_id, started_at_ms, fidelity)
                 VALUES (?1, 'provider_import', 'codex', 'machine-1', ?2,
                         'codex_session_jsonl', 'canonical-1', 'session-1', 1,
                         'imported')",
                params![source_id.to_string(), exact_path],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO capture_sources
                 (id, kind, provider, machine_id, raw_source_path, source_format,
                  source_identity, external_session_id, started_at_ms, fidelity)
                 VALUES (?1, 'provider_import', 'codex', 'machine-1', ?2,
                         'codex_session_jsonl', 'canonical-2', 'session-2', 1,
                         'imported')",
                params![ambiguous_source_id.to_string(), exact_path],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO provider_source_locators
                 (provider, source_format, machine_id, locator_identity, cursor_stream,
                  canonical_source_identity, alias_group_identity, raw_source_path,
                  source_revision, is_current, is_relocation_alias, observed_at_ms)
                 VALUES ('codex', 'codex_session_jsonl', 'machine-1', 'locator-1',
                         'cursor-1', 'canonical-1', 'alias-1', ?1, 'revision-1',
                         1, 0, 1)",
                [exact_path.as_str()],
            )
            .unwrap();
        for (locator_identity, alias_group_identity) in
            [("locator-2", "alias-2"), ("locator-3", "alias-3")]
        {
            store
                .conn
                .execute(
                    "INSERT INTO provider_source_locators
                     (provider, source_format, machine_id, locator_identity, cursor_stream,
                      canonical_source_identity, alias_group_identity, raw_source_path,
                      source_revision, is_current, is_relocation_alias, observed_at_ms)
                     VALUES ('codex', 'codex_session_jsonl', 'machine-1', ?1,
                             'cursor-2', 'canonical-2', ?2, ?3, 'revision-2',
                             1, 0, 1)",
                    params![locator_identity, alias_group_identity, exact_path],
                )
                .unwrap();
        }
        store
            .conn
            .execute(
                "INSERT INTO events
                 (id, seq, event_type, occurred_at_ms, capture_source_id)
                 VALUES (?1, 1, 'tool_output', 1, ?2)",
                params![event_id.to_string(), source_id.to_string()],
            )
            .unwrap();
        store.activate_projection_journal(&"a".repeat(64)).unwrap();
        journal_before = store
            .conn
            .query_row(
                "SELECT active, generation, contract_fingerprint,
                        high_water_sequence, cumulative_digest,
                        acknowledged_sequence, acknowledged_cumulative_digest
                 FROM projection_journal_state WHERE singleton = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )
            .unwrap();
        store
            .conn
            .execute_batch(
                "DROP TABLE capture_source_provider_routes;
                 UPDATE ctx_store_schema_identity
                 SET schema_identity = 'ctx-store-schema-47-final-v4'
                 WHERE singleton = 1 AND schema_version = 47;",
            )
            .unwrap();
    }

    let store = Store::open(&path).unwrap();
    let identity: String = store
        .conn
        .query_row(
            "SELECT schema_identity FROM ctx_store_schema_identity WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(identity, FINAL_SCHEMA_IDENTITY);
    assert_eq!(
        store
            .authorized_source_route_for_event(event_id)
            .unwrap()
            .path(),
        Path::new(&exact_path)
    );
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM capture_source_provider_routes
                 WHERE capture_source_id = ?1",
                [ambiguous_source_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    let journal_after = store
        .conn
        .query_row(
            "SELECT active, generation, contract_fingerprint,
                    high_water_sequence, cumulative_digest,
                    acknowledged_sequence, acknowledged_cumulative_digest
             FROM projection_journal_state WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(journal_after, journal_before);
}

#[test]
fn schema_v8_migrates_legacy_history_record_table_names() {
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
fn schema_v12_invalidates_provider_import_indexes_for_reimport() {
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
fn schema_v14_backfills_catalog_import_checkpoints() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        let legacy_sql = CREATE_TABLES_SQL
            .replace("    last_imported_at_ms INTEGER,\n", "")
            .replace("    last_imported_file_size_bytes INTEGER,\n", "")
            .replace("    last_imported_file_modified_at_ms INTEGER,\n", "")
            .replace("    last_imported_file_sha256 TEXT,\n", "")
            .replace("    last_imported_event_count INTEGER,\n", "");
        conn.execute_batch(&legacy_sql).unwrap();
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
                'session-1', 'primary', 20, 30, 40, 50, 10, 15, 'pending', 7
            )
            "#,
            [],
        )
        .unwrap();
        conn.execute_batch("PRAGMA user_version = 13;").unwrap();
    }

    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    let checkpoint: (Option<i64>, Option<i64>, Option<i64>, Option<i64>) = store
        .conn
        .query_row(
            "SELECT last_imported_at_ms, last_imported_file_size_bytes, last_imported_file_modified_at_ms, last_imported_event_count FROM catalog_sessions",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(checkpoint, (Some(50), Some(10), Some(15), Some(7)));
}

#[test]
fn schema_v44_removes_payload_state_columns_and_rebuilds_preview_fts() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    let event_id = new_id();
    let result_event_id = new_id();
    let result_canary = "v44-legacy-result-body-canary";
    let artifact_id = new_id();
    {
        let conn = Connection::open(&path).unwrap();
        let legacy_tables = CREATE_TABLES_SQL
            .replace(
                "    preview_text TEXT,\n    created_at_ms INTEGER NOT NULL,",
                "    preview_text TEXT,\n    redaction_state TEXT NOT NULL DEFAULT 'safe_preview' CHECK (redaction_state IN ('raw', 'redacted', 'safe_preview', 'withheld')),\n    created_at_ms INTEGER NOT NULL,",
            )
            .replace(
                "    dedupe_key TEXT,\n    visibility TEXT NOT NULL DEFAULT 'local_only'",
                "    dedupe_key TEXT,\n    redaction_state TEXT NOT NULL DEFAULT 'safe_preview' CHECK (redaction_state IN ('raw', 'redacted', 'safe_preview', 'withheld')),\n    visibility TEXT NOT NULL DEFAULT 'local_only'",
            );
        conn.execute_batch(&legacy_tables).unwrap();
        conn.execute_batch(&FTS_TABLES_SQL.replace("preview_text", "safe_preview_text"))
            .unwrap();
        conn.execute_batch(INDEXES_SQL).unwrap();
        conn.execute(
            r#"
            INSERT INTO artifacts
            (id, kind, blob_hash, blob_path, byte_size, preview_text, redaction_state, created_at_ms, updated_at_ms, fidelity)
            VALUES (?1, 'json', 'hash-v44-artifact', 'objects/ha/hash-v44-artifact', 5, 'artifact preview', 'safe_preview', 1, 1, 'imported')
            "#,
            params![artifact_id.to_string()],
        )
        .unwrap();
        conn.execute(
            r#"
            INSERT INTO events
            (id, seq, event_type, role, occurred_at_ms, payload_json, redaction_state, fidelity)
            VALUES (?1, 2, 'command_output', 'tool', 2, ?2, 'safe_preview', 'imported')
            "#,
            params![
                result_event_id.to_string(),
                serde_json::json!({
                    "provider": "codex",
                    "provider_session_id": "session-1",
                    "provider_event_index": 2,
                    "provider_event_hash": "hash-2",
                    "body": {
                        "tool": "exec_command",
                        "call_id": "call-2",
                        "exit_code": 0,
                        "output_preview": result_canary,
                        "text": result_canary,
                        "output": result_canary
                    }
                })
                .to_string()
            ],
        )
        .unwrap();
        conn.execute(
            r#"
            INSERT INTO events
            (id, seq, event_type, role, occurred_at_ms, payload_json, redaction_state, fidelity)
            VALUES (?1, 1, 'message', 'assistant', 1, '{"text":"v44 migration preview"}', 'safe_preview', 'imported')
            "#,
            params![event_id.to_string()],
        )
        .unwrap();
        conn.execute(
            r#"
            INSERT INTO event_search
            (event_id, role, safe_preview_text, rank_bucket)
            VALUES (?1, 'tool', ?2, 'tool_output')
            "#,
            params![result_event_id.to_string(), result_canary],
        )
        .unwrap();
        conn.execute(
            r#"
            INSERT INTO event_search
            (event_id, role, safe_preview_text, rank_bucket)
            VALUES (?1, 'assistant', 'old preview text', 'message')
            "#,
            params![event_id.to_string()],
        )
        .unwrap();
        conn.execute_batch("PRAGMA user_version = 43;").unwrap();
    }

    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    assert!(!table_has_column(&store.conn, "events", "redaction_state").unwrap());
    assert!(!table_has_column(&store.conn, "artifacts", "redaction_state").unwrap());
    assert!(!table_has_column(&store.conn, "event_search", "safe_preview_text").unwrap());
    assert!(table_has_column(&store.conn, "event_search", "preview_text").unwrap());
    assert!(!table_has_column(&store.conn, "artifact_search", "safe_preview_text").unwrap());
    assert!(table_has_column(&store.conn, "artifact_search", "preview_text").unwrap());
    assert!(!table_has_column(&store.conn, "ctx_events", "redaction_state").unwrap());
    let preview: String = store
        .conn
        .query_row(
            "SELECT preview_text FROM event_search WHERE event_id = ?1",
            params![event_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(preview, "v44 migration preview");
    let lookup_preview: String = store
        .conn
        .query_row(
            "SELECT preview_text FROM event_search_lookup WHERE event_id = ?1",
            params![event_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(lookup_preview, "v44 migration preview");
    let compact_result: String = store
        .conn
        .query_row(
            "SELECT payload_json FROM events WHERE id = ?1",
            params![result_event_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!compact_result.contains(result_canary));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&compact_result).unwrap()["body"],
        serde_json::json!({
            "tool": "exec_command",
            "call_id": "call-2",
            "exit_code": 0
        })
    );
    assert!(store
        .search_event_hits(result_canary, 10)
        .unwrap()
        .is_empty());
}

#[test]
fn current_v47_final_v2_is_sanitized_before_final_v3_is_accepted() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    let object_dir = temp.path().join("objects");
    let event_id = new_id();
    let shared_result_event_id = new_id();
    let message_event_id = new_id();
    let result_artifact_id = new_id();
    let shared_artifact_id = new_id();
    let result_canary = "v47-final-v2-result-body-canary";
    let shared_canary = "v47-final-v2-shared-non-result-canary";
    let result_blob_path;
    let shared_blob_path;
    {
        let store = Store::open(&path).unwrap();
        result_blob_path = seed_artifact_blob(
            &store,
            &object_dir,
            result_artifact_id,
            result_canary.as_bytes(),
        );
        shared_blob_path = seed_artifact_blob(
            &store,
            &object_dir,
            shared_artifact_id,
            shared_canary.as_bytes(),
        );
        store
            .conn
            .execute(
                r#"
                INSERT INTO events
                (id, seq, event_type, role, occurred_at_ms, payload_json,
                 payload_blob_id, fidelity)
                VALUES (?1, 1, 'command_output', 'tool', 1, ?2, ?3, 'imported')
                "#,
                params![
                    event_id.to_string(),
                    serde_json::json!({
                        "provider": "codex",
                        "provider_session_id": "session-1",
                        "provider_event_index": 1,
                        "provider_event_hash": "hash-1",
                        "body": {
                            "tool": "exec_command",
                            "call_id": "call-1",
                            "exit_code": 0,
                            "result_outcome": "success",
                            "output_preview": result_canary,
                            "text": result_canary,
                            "output": result_canary
                        }
                    })
                    .to_string(),
                    result_artifact_id.to_string(),
                ],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO events
                 (id, seq, event_type, role, occurred_at_ms, payload_json,
                  payload_blob_id, fidelity)
                 VALUES (?1, 2, 'tool_output', 'tool', 2, ?2, ?3, 'imported')",
                params![
                    shared_result_event_id.to_string(),
                    serde_json::json!({"output": shared_canary}).to_string(),
                    shared_artifact_id.to_string(),
                ],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO events
                 (id, seq, event_type, role, occurred_at_ms, payload_json,
                  payload_blob_id, fidelity)
                 VALUES (?1, 3, 'message', 'user', 3, ?2, ?3, 'imported')",
                params![
                    message_event_id.to_string(),
                    serde_json::json!({"text": shared_canary}).to_string(),
                    shared_artifact_id.to_string(),
                ],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO event_search
                 (event_id, role, preview_text, rank_bucket)
                 VALUES (?1, 'tool', ?2, 'tool_output')",
                params![event_id.to_string(), result_canary],
            )
            .unwrap();
        let checkpoint = store.activate_projection_journal(&"a".repeat(64)).unwrap();
        assert!(checkpoint.position.sequence > 0);
        store
            .conn
            .execute(
                "UPDATE ctx_store_schema_identity SET schema_identity = ?1
                 WHERE singleton = 1 AND schema_version = 47",
                ["ctx-store-schema-47-final-v2"],
            )
            .unwrap();
    }

    let store = Store::open(&path).unwrap();
    let identity: String = store
        .conn
        .query_row(
            "SELECT schema_identity FROM ctx_store_schema_identity WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(identity, FINAL_SCHEMA_IDENTITY);
    let payload: String = store
        .conn
        .query_row(
            "SELECT payload_json FROM events WHERE id = ?1",
            [event_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!payload.contains(result_canary));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&payload).unwrap()["body"],
        serde_json::json!({
            "tool": "exec_command",
            "call_id": "call-1",
            "exit_code": 0,
            "result_outcome": "success"
        })
    );
    assert!(store
        .search_event_hits(result_canary, 10)
        .unwrap()
        .is_empty());
    let result_blob_id: Option<String> = store
        .conn
        .query_row(
            "SELECT payload_blob_id FROM events WHERE id = ?1",
            [event_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(result_blob_id, None);
    let shared_result_blob_id: Option<String> = store
        .conn
        .query_row(
            "SELECT payload_blob_id FROM events WHERE id = ?1",
            [shared_result_event_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(shared_result_blob_id, None);
    let message_blob_id: Option<String> = store
        .conn
        .query_row(
            "SELECT payload_blob_id FROM events WHERE id = ?1",
            [message_event_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(message_blob_id, Some(shared_artifact_id.to_string()));
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM artifacts WHERE id = ?1",
                [result_artifact_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM artifacts WHERE id = ?1",
                [shared_artifact_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    assert!(!result_blob_path.exists());
    assert!(shared_blob_path.exists());
    assert!(!table_exists(&store.conn, "ctx_source_backed_result_blob_cleanup").unwrap());
    let exported = serde_json::to_string(&store.export_archive().unwrap()).unwrap();
    assert!(!exported.contains(result_canary));
    assert!(exported.contains(shared_canary));
    assert!(!store
        .search_event_hits(shared_canary, 10)
        .unwrap()
        .is_empty());
    let journal_state: (i64, i64, i64) = store
        .conn
        .query_row(
            "SELECT active,
                    (SELECT COUNT(*) FROM projection_journal_chunks),
                    (SELECT COUNT(*) FROM projection_journal_entities)
             FROM projection_journal_state WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(journal_state, (0, 0, 0));
}

#[test]
fn released_v46_result_blob_is_removed_before_v47_identity_is_written() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    let object_dir = temp.path().join("objects");
    let event_id = new_id();
    let artifact_id = new_id();
    let canary = "released-v46-result-blob-canary";
    let blob_path;
    {
        let store = Store::open(&path).unwrap();
        blob_path = seed_artifact_blob(&store, &object_dir, artifact_id, canary.as_bytes());
        store
            .conn
            .execute(
                "INSERT INTO events
                 (id, seq, event_type, occurred_at_ms, payload_json,
                  payload_blob_id, fidelity)
                 VALUES (?1, 1, 'tool_output', 1, ?2, ?3, 'imported')",
                params![
                    event_id.to_string(),
                    serde_json::json!({"output": canary}).to_string(),
                    artifact_id.to_string(),
                ],
            )
            .unwrap();
        store
            .conn
            .execute_batch(
                "DROP TABLE projection_journal_entities;
                 DROP TABLE projection_journal_chunks;
                 DROP TABLE projection_journal_state;
                 DROP TABLE ctx_store_schema_identity;
                 PRAGMA user_version = 46;",
            )
            .unwrap();
    }

    let store = Store::open(&path).unwrap();
    let identity: String = store
        .conn
        .query_row(
            "SELECT schema_identity FROM ctx_store_schema_identity WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(identity, FINAL_SCHEMA_IDENTITY);
    let payload_blob_id: Option<String> = store
        .conn
        .query_row(
            "SELECT payload_blob_id FROM events WHERE id = ?1",
            [event_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(payload_blob_id, None);
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM artifacts WHERE id = ?1",
                [artifact_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert!(!blob_path.exists());
    let exported = serde_json::to_string(&store.export_archive().unwrap()).unwrap();
    assert!(!exported.contains(canary));
    assert!(store.search_event_hits(canary, 10).unwrap().is_empty());
}

#[test]
fn final_v2_result_blob_cleanup_rolls_back_with_payload_sanitization() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    let object_dir = temp.path().join("objects");
    let event_id = new_id();
    let artifact_id = new_id();
    let blob_path;
    {
        let store = Store::open(&path).unwrap();
        blob_path = seed_artifact_blob(
            &store,
            &object_dir,
            artifact_id,
            b"rollback-result-blob-canary",
        );
        store
            .conn
            .execute(
                "INSERT INTO events
                 (id, seq, event_type, occurred_at_ms, payload_json,
                  payload_blob_id, fidelity)
                 VALUES (?1, 1, 'command_output', 1, 'not-json', ?2, 'imported')",
                params![event_id.to_string(), artifact_id.to_string()],
            )
            .unwrap();
        store
            .conn
            .execute(
                "UPDATE ctx_store_schema_identity SET schema_identity = ?1
                 WHERE singleton = 1 AND schema_version = 47",
                ["ctx-store-schema-47-final-v2"],
            )
            .unwrap();
    }

    let error = match Store::open(&path) {
        Ok(_) => panic!("invalid result payload unexpectedly migrated"),
        Err(error) => error,
    };
    assert!(matches!(error, StoreError::Json(_)));
    let conn = Connection::open(&path).unwrap();
    let identity: String = conn
        .query_row(
            "SELECT schema_identity FROM ctx_store_schema_identity WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(identity, "ctx-store-schema-47-final-v2");
    let payload_blob_id: Option<String> = conn
        .query_row(
            "SELECT payload_blob_id FROM events WHERE id = ?1",
            [event_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(payload_blob_id, Some(artifact_id.to_string()));
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM artifacts WHERE id = ?1",
            [artifact_id.to_string()],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        1
    );
    assert!(!table_exists(&conn, "ctx_source_backed_result_blob_cleanup").unwrap());
    assert!(blob_path.exists());
}

#[test]
fn final_v3_retries_committed_result_blob_cleanup() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    let object_dir = temp.path().join("objects");
    let artifact_id = new_id();
    let blob_path;
    {
        let store = Store::open(&path).unwrap();
        blob_path = seed_artifact_blob(
            &store,
            &object_dir,
            artifact_id,
            b"pending-result-blob-canary",
        );
        let blob_hash: String = store
            .conn
            .query_row(
                "SELECT blob_hash FROM artifacts WHERE id = ?1",
                [artifact_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        store
            .conn
            .execute_batch(
                "CREATE TABLE ctx_source_backed_result_blob_cleanup (
                   blob_hash TEXT PRIMARY KEY NOT NULL
                 ) STRICT;",
            )
            .unwrap();
        store
            .conn
            .execute(
                "DELETE FROM artifacts WHERE id = ?1",
                [artifact_id.to_string()],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO ctx_source_backed_result_blob_cleanup (blob_hash) VALUES (?1)",
                [blob_hash],
            )
            .unwrap();
    }

    let store = Store::open(&path).unwrap();
    assert!(!blob_path.exists());
    assert!(!table_exists(&store.conn, "ctx_source_backed_result_blob_cleanup").unwrap());
}

#[test]
fn schema_v45_rebuilds_scriptgram_sidecars() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    let record_id = new_id().to_string();
    let event_id = new_id().to_string();
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(CREATE_TABLES_SQL).unwrap();
        conn.execute_batch(FTS_TABLES_SQL).unwrap();
        conn.execute_batch(INDEXES_SQL).unwrap();
        conn.execute(
            r#"
            INSERT INTO history_records (id, title, body, created_at, updated_at)
            VALUES (?1, 'v44 multilingual record', 'OAuth認証の検索状態を確認します。', '2026-06-23T12:00:00+00:00', '2026-06-23T12:00:00+00:00')
            "#,
            [record_id.as_str()],
        )
        .unwrap();
        conn.execute(
            r#"
            INSERT INTO events (id, seq, history_record_id, event_type, role, occurred_at_ms, payload_json)
            VALUES (?1, 1, ?2, 'message', 'user', 0, '{"text":"OAuth認証の検索状態をイベントにも残します。"}')
            "#,
            params![event_id.as_str(), record_id.as_str()],
        )
        .unwrap();
        conn.execute_batch(
            r#"
            DROP TABLE IF EXISTS ctx_history_search_scriptgram;
            DROP TABLE IF EXISTS event_search_scriptgram;
            CREATE VIRTUAL TABLE ctx_history_search_scriptgram USING fts5(record_id UNINDEXED, body);
            CREATE VIRTUAL TABLE event_search_scriptgram USING fts5(event_id UNINDEXED, token_text);
            PRAGMA user_version = 44;
            "#,
        )
        .unwrap();
    }

    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    for column in ["record_id", "token_text"] {
        assert!(table_has_column(&store.conn, "ctx_history_search_scriptgram", column).unwrap());
    }
    for column in [
        "event_id",
        "history_record_id",
        "session_id",
        "role",
        "token_text",
        "rank_bucket",
    ] {
        assert!(table_has_column(&store.conn, "event_search_scriptgram", column).unwrap());
    }

    let record_tokens: String = store
        .conn
        .query_row(
            "SELECT token_text FROM ctx_history_search_scriptgram WHERE record_id = ?1",
            [record_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(record_tokens
        .split_whitespace()
        .any(|token| token == "認証"));
    let event_tokens: String = store
        .conn
        .query_row(
            "SELECT token_text FROM event_search_scriptgram WHERE event_id = ?1",
            [event_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(event_tokens.split_whitespace().any(|token| token == "認証"));

    let record_hits = store.search_records("認証", 10).unwrap();
    assert!(record_hits
        .iter()
        .any(|record| record.id.to_string() == record_id));
    let event_hits = store.search_event_hits("認証", 10).unwrap();
    assert!(event_hits
        .iter()
        .any(|hit| hit.event_id.to_string() == event_id));
}
