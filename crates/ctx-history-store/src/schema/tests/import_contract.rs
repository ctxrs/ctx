#[test]
fn real_schema_v45_fixture_migrates_import_state_through_v49() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(include_str!("../fixtures/schema_v45.sql"))
            .unwrap();
        conn.execute_batch(
            r#"
            INSERT INTO catalog_sessions
            (source_path, provider, source_format, source_root, external_session_id,
             agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms,
             indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms,
             indexed_status, last_imported_at_ms, last_imported_file_size_bytes,
             last_imported_file_modified_at_ms, last_imported_event_count)
            VALUES
            ('/missing/v45-indexed.jsonl', 'codex', 'codex_session_jsonl', '/missing/v45',
             'v45-indexed', 'primary', 21, 31, 41, 51, 21, 31, 'indexed', 51, 21, 31, 2);

            INSERT INTO source_import_files
            (provider, source_format, source_root, source_path, file_size_bytes,
             file_modified_at_ms, observed_at_ms, indexed_at_ms, indexed_status,
             indexed_error)
            VALUES
            ('claude', 'claude_projects_jsonl_tree', '/missing/v45-claude',
             '/missing/v45-claude/failed.jsonl', 22, 32, 42, 52, 'failed',
             'legacy transient failure');

            PRAGMA user_version = 45;
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
    let indexed: (String, i64, Option<i64>, Option<i64>) = store
        .conn
        .query_row(
            "SELECT indexed_status, import_revision, indexed_import_revision, last_imported_file_size_bytes FROM catalog_sessions WHERE source_path = '/missing/v45-indexed.jsonl'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(indexed, ("indexed".to_owned(), 1, Some(1), Some(21)));
    let pending = store
        .list_pending_source_import_files(CaptureProvider::Claude, "/missing/v45-claude")
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].import_revision, 1);
    let indexed_revision: Option<i64> = store
        .conn
        .query_row(
            "SELECT indexed_import_revision FROM source_import_files WHERE source_path = '/missing/v45-claude/failed.jsonl'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(indexed_revision, None);
    let generations: Vec<(String, String, String, i64, i64)> = store
        .conn
        .prepare(
            "SELECT provider, source_root, inventory_family, current_generation, completed_generation FROM import_inventory_generations ORDER BY inventory_family, provider, source_root",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        generations,
        vec![
            (
                "codex".to_owned(),
                "/missing/v45".to_owned(),
                "catalog_sessions".to_owned(),
                1,
                1,
            ),
            (
                "claude".to_owned(),
                "/missing/v45-claude".to_owned(),
                "source_import_files".to_owned(),
                1,
                1,
            ),
        ]
    );
}

#[test]
fn schema_v48_grandfathers_rows_through_v49_and_retries_v46_failures_without_source_reads() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        let v46_sql = CREATE_TABLES_SQL
            .replace(
                "CREATE TABLE IF NOT EXISTS import_inventory_generations (\n    provider TEXT NOT NULL,\n    source_root TEXT NOT NULL,\n    inventory_family TEXT NOT NULL CHECK (inventory_family IN ('catalog_sessions', 'source_import_files')),\n    current_generation INTEGER NOT NULL CHECK (current_generation > 0),\n    completed_generation INTEGER NOT NULL DEFAULT 0 CHECK (completed_generation >= 0 AND completed_generation <= current_generation),\n    PRIMARY KEY (provider, source_root, inventory_family)\n);\n\n",
                "",
            )
            .replace(
                "    import_revision INTEGER NOT NULL DEFAULT 1 CHECK (import_revision > 0),\n",
                "",
            )
            .replace(
                "    indexed_import_revision INTEGER CHECK (indexed_import_revision > 0),\n",
                "",
            )
            .replace(
                "CHECK (indexed_status IN ('pending', 'indexed', 'completed_with_rejections', 'rejected', 'failed'))",
                "CHECK (indexed_status IN ('pending', 'indexed', 'failed'))",
            );
        conn.execute_batch(&v46_sql).unwrap();
        conn.execute_batch(INDEXES_SQL).unwrap();
        conn.execute_batch(
            r#"
            INSERT INTO catalog_sessions
            (source_path, provider, source_format, source_root, external_session_id,
             agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms,
             indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms,
             indexed_status, indexed_error)
            VALUES
            ('/missing/indexed.jsonl', 'codex', 'codex_session_jsonl', '/missing',
             'indexed-session', 'primary', 10, 20, 30, 40, 10, 20, 'indexed', NULL),
            ('/missing/pr75-failed.jsonl', 'codex', 'codex_session_jsonl', '/missing',
             'failed-session', 'primary', 11, 21, 31, 41, NULL, NULL, 'failed',
             'full import failed for one or more sessions');

            INSERT INTO source_import_files
            (provider, source_format, source_root, source_path, file_size_bytes,
             file_modified_at_ms, observed_at_ms, indexed_at_ms,
             indexed_file_size_bytes, indexed_file_modified_at_ms, indexed_status,
             indexed_error)
            VALUES
            ('claude', 'claude_projects_jsonl_tree', '/missing/claude',
             '/missing/claude/indexed.jsonl', 12, 22, 32, 42, 12, 22, 'indexed', NULL),
            ('antigravity', 'antigravity_cli_transcript_jsonl_tree', '/missing/agy',
             '/missing/agy/rejected.jsonl', 13, 23, 33, 43, NULL, NULL, 'failed',
             'provider import reported 1 failure(s)');

            PRAGMA user_version = 46;
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

    let catalog_rows = store
        .conn
        .prepare(
            "SELECT source_path, indexed_status, import_revision, indexed_import_revision FROM catalog_sessions ORDER BY source_path",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        catalog_rows,
        vec![
            (
                "/missing/indexed.jsonl".to_owned(),
                "indexed".to_owned(),
                1,
                Some(1),
            ),
            (
                "/missing/pr75-failed.jsonl".to_owned(),
                "failed".to_owned(),
                1,
                None,
            ),
        ]
    );
    let failed_catalog = store
        .list_pending_catalog_sessions(CaptureProvider::Codex, "/missing")
        .unwrap();
    assert!(failed_catalog
        .iter()
        .any(|row| row.source_path == "/missing/pr75-failed.jsonl"));

    let file_rows = store
        .conn
        .prepare(
            "SELECT source_path, indexed_status, import_revision, indexed_import_revision FROM source_import_files ORDER BY source_path",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        file_rows
            .iter()
            .find(|row| row.0.ends_with("claude/indexed.jsonl"))
            .unwrap()
            .3,
        Some(1)
    );
    assert_eq!(
        file_rows
            .iter()
            .find(|row| row.0.ends_with("agy/rejected.jsonl"))
            .unwrap()
            .3,
        None
    );
    assert_eq!(
        store
            .list_pending_source_import_files(CaptureProvider::Antigravity, "/missing/agy")
            .unwrap()
            .len(),
        1
    );
    let generation_count: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM import_inventory_generations WHERE current_generation = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(generation_count, 3);
}

#[test]
fn real_schema_v49_fixture_adds_provider_file_contract_tables() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(include_str!("../fixtures/schema_v49.sql"))
            .unwrap();
        let legacy_views: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'view' AND name IN ('ctx_sessions', 'ctx_events', 'ctx_files_touched', 'ctx_sources')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(legacy_views, 4);
    }

    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let columns = store
        .conn
        .prepare("PRAGMA table_info(provider_file_checkpoints)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        columns,
        vec![
            "provider",
            "source_format",
            "source_root",
            "source_path",
            "import_revision",
            "checkpoint_version",
            "stable_file_identity",
            "committed_byte_offset",
            "committed_complete_line_count",
            "head_sha256",
            "boundary_sha256",
            "resume_state",
            "updated_at_ms",
        ]
    );
    let rows: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM provider_file_checkpoints",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0);
    let semantic_revision: i64 = store
        .conn
        .query_row(
            "SELECT current_revision FROM semantic_replacement_revision WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(semantic_revision, 0);
    for present in [
        "provider_file_checkpoints",
        "provider_file_publications",
        "semantic_replacement_revision",
    ] {
        let exists: bool = store
            .conn
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                params![present],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists);
    }

    let fresh_path = temp.path().join("fresh-v50.sqlite");
    let fresh = Store::open(&fresh_path).unwrap();
    assert_schema_object_parity(&store.conn, &fresh.conn);
}

fn schema_object_signature(conn: &Connection) -> Vec<(String, String, String)> {
    conn.prepare(
        r#"
        SELECT type, name, sql
        FROM sqlite_master
        WHERE type IN ('table', 'index', 'view')
          AND name NOT LIKE 'sqlite_%'
          AND sql IS NOT NULL
        ORDER BY type, name
        "#,
    )
    .unwrap()
    .query_map([], |row| {
        let sql: String = row.get(2)?;
        Ok((
            row.get(0)?,
            row.get(1)?,
            sql.replace('"', "")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        ))
    })
    .unwrap()
    .collect::<rusqlite::Result<Vec<_>>>()
    .unwrap()
}

fn assert_schema_object_parity(upgraded: &Connection, fresh: &Connection) {
    let upgraded = schema_object_signature(upgraded);
    let fresh = schema_object_signature(fresh);
    let mismatch = upgraded
        .iter()
        .zip(&fresh)
        .find(|(upgraded, fresh)| upgraded != fresh);
    assert!(
        mismatch.is_none() && upgraded.len() == fresh.len(),
        "schema mismatch: upgraded objects={}, fresh objects={}, first mismatch={mismatch:?}",
        upgraded.len(),
        fresh.len(),
    );
}

fn assert_provider_migration_accepts(
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
