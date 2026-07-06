#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn schema_v24_adds_deepagents_mux_and_lingma_provider_checks() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        let legacy_sql = CREATE_TABLES_SQL
            .replace(", 'deepagents'", "")
            .replace(", 'mux'", "")
            .replace(", 'lingma'", "");
        conn.execute_batch(&legacy_sql).unwrap();
        conn.execute_batch(INDEXES_SQL).unwrap();
        conn.execute_batch("PRAGMA user_version = 23;").unwrap();
    }

    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    for provider in ["deepagents", "mux", "lingma"] {
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
    }

    for (source_path, provider, source_format, source_root) in [
        (
            "/tmp/deepagents/sessions.db",
            "deepagents",
            "deepagents_sessions_sqlite",
            "/tmp/deepagents",
        ),
        (
            "/tmp/mux/chat.jsonl",
            "mux",
            "mux_session_jsonl",
            "/tmp/mux",
        ),
        (
            "/tmp/lingma/local.db",
            "lingma",
            "lingma_sqlite",
            "/tmp/lingma/local.db",
        ),
    ] {
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
    }

    for (provider, source_format, source_root, source_path) in [
        (
            "deepagents",
            "deepagents_sessions_sqlite",
            "/tmp/deepagents",
            "/tmp/deepagents/sessions.db",
        ),
        (
            "mux",
            "mux_session_jsonl",
            "/tmp/mux",
            "/tmp/mux/chat.jsonl",
        ),
        (
            "lingma",
            "lingma_sqlite",
            "/tmp/lingma/local.db",
            "/tmp/lingma/local.db",
        ),
    ] {
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
}
