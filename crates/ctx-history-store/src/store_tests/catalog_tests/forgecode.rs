#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn schema_v17_adds_jsonl_provider_checks() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        let legacy_sql = CREATE_TABLES_SQL.replace(
            ", 'qwen_code', 'kimi_code_cli', 'forgecode', 'deepagents', 'mistral_vibe'",
            "",
        );
        conn.execute_batch(&legacy_sql).unwrap();
        conn.execute_batch(INDEXES_SQL).unwrap();
        conn.execute_batch("PRAGMA user_version = 16;").unwrap();
    }

    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    for (provider, source_format) in [
        ("qwen_code", "qwen_code_chat_jsonl"),
        ("kimi_code_cli", "kimi_code_cli_wire_jsonl"),
    ] {
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
                VALUES (?1, ?2, ?3, '/tmp/provider', 'primary', 1, 0, 0)
                "#,
                params![format!("/tmp/provider/{provider}.jsonl"), provider, source_format],
            )
            .unwrap();
        store
            .conn
            .execute(
                r#"
                INSERT INTO source_import_files
                (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms)
                VALUES (?1, ?2, '/tmp/provider', ?3, 1, 0, 0)
                "#,
                params![
                    provider,
                    source_format,
                    format!("/tmp/provider/{provider}.jsonl")
                ],
            )
            .unwrap();
    }
}

#[test]
pub(crate) fn schema_v22_adds_forgecode_provider_checks() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        let legacy_sql = CREATE_TABLES_SQL.replace(", 'forgecode'", "");
        conn.execute_batch(&legacy_sql).unwrap();
        conn.execute_batch(INDEXES_SQL).unwrap();
        conn.execute_batch("PRAGMA user_version = 21;").unwrap();
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
            VALUES (?1, 'provider_import', 'forgecode', 'test-machine', 0, 'imported')
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
            VALUES ('/tmp/forge/.forge.db', 'forgecode', 'forgecode_sqlite', '/tmp/forge', 'primary', 1, 0, 0)
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
            VALUES ('forgecode', 'forgecode_sqlite', '/tmp/forge', '/tmp/forge/.forge.db', 1, 0, 0)
            "#,
            [],
        )
        .unwrap();
}
