#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn schema_v23_adds_mistral_vibe_provider_checks() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        let legacy_sql = CREATE_TABLES_SQL.replace(", 'mistral_vibe'", "");
        conn.execute_batch(&legacy_sql).unwrap();
        conn.execute_batch(INDEXES_SQL).unwrap();
        conn.execute_batch("PRAGMA user_version = 22;").unwrap();
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
            VALUES (?1, 'provider_import', 'mistral_vibe', 'test-machine', 0, 'imported')
            "#,
            params![new_id().to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO catalog_sessions
            (source_path, provider, source_format, source_root, agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms)
            VALUES ('/tmp/vibe/messages.jsonl', 'mistral_vibe', 'mistral_vibe_session_jsonl', '/tmp/vibe', 'primary', 1, 0, 0)
            "#,
            [],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO source_import_files
            (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms)
            VALUES ('mistral_vibe', 'mistral_vibe_session_jsonl', '/tmp/vibe', '/tmp/vibe/messages.jsonl', 1, 0, 0)
            "#,
            [],
        )
        .unwrap();
}
