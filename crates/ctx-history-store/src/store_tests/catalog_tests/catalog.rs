#[allow(unused_imports)]
use super::*;

pub(crate) type CatalogSessionCheckpointRow = (
    String,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

pub(crate) fn tempdir() -> tempfile::TempDir {
    let root = std::env::current_dir().unwrap().join("target/test-data");
    fs::create_dir_all(&root).unwrap();
    tempfile::Builder::new()
        .prefix("ctx-history-store-catalog-")
        .tempdir_in(root)
        .unwrap()
}

#[test]
pub(crate) fn catalog_sessions_count_indexed_and_stale_rows() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = timestamp_ms(fixed_time());
    store
        .upsert_catalog_sessions(&[catalog_session(
            "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
            "codex-session-1",
            cataloged_at_ms,
        )])
        .unwrap();

    let counts = store.catalog_session_counts().unwrap();
    assert_eq!(counts.total, 1);
    assert_eq!(counts.indexed, 0);
    assert_eq!(counts.stale, 0);
    assert_eq!(counts.pending, 1);
    assert_eq!(counts.failed, 0);
    assert_eq!(
        store
            .catalog_source_stale_session_count(
                CaptureProvider::Codex,
                "/home/user/.codex/sessions"
            )
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .list_pending_catalog_sessions(CaptureProvider::Codex, "/home/user/.codex/sessions")
            .unwrap()
            .len(),
        1
    );

    store
        .upsert_session(&imported_session("codex-session-1"))
        .unwrap();
    store
        .mark_catalog_source_indexed(
            CaptureProvider::Codex,
            CatalogSourceIndexUpdate {
                source_root: "/home/user/.codex/sessions",
                source_path: "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
                file_size_bytes: 42,
                file_modified_at_ms: cataloged_at_ms,
                file_sha256: None,
                event_count: Some(3),
                indexed_at_ms: cataloged_at_ms + 10,
            },
        )
        .unwrap();
    let counts = store.catalog_session_counts().unwrap();
    assert_eq!(counts.indexed, 1);
    assert_eq!(counts.pending, 0);

    store
        .mark_catalog_source_stale(
            CaptureProvider::Codex,
            "/home/user/.codex/sessions",
            cataloged_at_ms + 1,
        )
        .unwrap();
    let counts = store.catalog_session_counts().unwrap();
    assert_eq!(counts.total, 0);
    assert_eq!(counts.indexed, 0);
    assert_eq!(counts.stale, 1);
    assert_eq!(counts.pending, 0);
    assert_eq!(
        store
            .catalog_source_stale_session_count(
                CaptureProvider::Codex,
                "/home/user/.codex/sessions"
            )
            .unwrap(),
        1
    );
}

#[test]
pub(crate) fn catalog_import_planning_requires_current_index_state_and_matching_session() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = timestamp_ms(fixed_time());
    store
        .upsert_catalog_sessions(&[catalog_session(
            "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
            "codex-session-1",
            cataloged_at_ms,
        )])
        .unwrap();
    store
        .mark_catalog_source_indexed(
            CaptureProvider::Codex,
            CatalogSourceIndexUpdate {
                source_root: "/home/user/.codex/sessions",
                source_path: "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
                file_size_bytes: 42,
                file_modified_at_ms: cataloged_at_ms,
                file_sha256: None,
                event_count: Some(3),
                indexed_at_ms: cataloged_at_ms + 10,
            },
        )
        .unwrap();

    let pending = store
        .list_pending_catalog_sessions(CaptureProvider::Codex, "/home/user/.codex/sessions")
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(store.catalog_session_counts().unwrap().indexed, 0);

    store
        .upsert_session(&imported_session("codex-session-1"))
        .unwrap();
    let pending = store
        .list_pending_catalog_sessions(CaptureProvider::Codex, "/home/user/.codex/sessions")
        .unwrap();
    assert!(pending.is_empty());
    let counts = store.catalog_session_counts().unwrap();
    assert_eq!(counts.indexed, 1);
    assert_eq!(counts.pending, 0);
}

#[test]
pub(crate) fn catalog_import_mark_failed_records_error_and_remains_pending() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = timestamp_ms(fixed_time());
    store
        .upsert_catalog_sessions(&[catalog_session(
            "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
            "codex-session-1",
            cataloged_at_ms,
        )])
        .unwrap();

    let changed = store
        .mark_catalog_source_failed(
            CaptureProvider::Codex,
            "/home/user/.codex/sessions",
            "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
            "bad json",
            cataloged_at_ms + 10,
        )
        .unwrap();
    assert_eq!(changed, 1);

    let counts = store.catalog_session_counts().unwrap();
    assert_eq!(counts.failed, 1);
    assert_eq!(counts.pending, 1);
    let (status, error, indexed_at_ms): (String, Option<String>, Option<i64>) = store
        .conn
        .query_row(
            "SELECT indexed_status, indexed_error, indexed_at_ms FROM catalog_sessions WHERE source_path = ?1",
            ["/home/user/.codex/sessions/2026/06/24/rollout.jsonl"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(status, CatalogIndexedStatus::Failed.as_str());
    assert_eq!(error.as_deref(), Some("bad json"));
    assert_eq!(indexed_at_ms, Some(cataloged_at_ms + 10));
}

#[test]
pub(crate) fn catalog_upsert_invalidates_checkpoint_for_shrink_and_same_size_change() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = timestamp_ms(fixed_time());
    for (source_path, file_size_bytes) in [
        ("/home/user/.codex/sessions/2026/06/24/shrink.jsonl", 41_u64),
        (
            "/home/user/.codex/sessions/2026/06/24/same-size.jsonl",
            42_u64,
        ),
    ] {
        store
            .upsert_catalog_sessions(&[catalog_session(source_path, source_path, cataloged_at_ms)])
            .unwrap();
        store
            .upsert_session(&imported_session(source_path))
            .unwrap();
        store
            .mark_catalog_source_indexed(
                CaptureProvider::Codex,
                CatalogSourceIndexUpdate {
                    source_root: "/home/user/.codex/sessions",
                    source_path,
                    file_size_bytes: 42,
                    file_modified_at_ms: cataloged_at_ms,
                    file_sha256: None,
                    event_count: Some(3),
                    indexed_at_ms: cataloged_at_ms + 10,
                },
            )
            .unwrap();

        let mut changed = catalog_session(source_path, source_path, cataloged_at_ms + 1);
        changed.file_size_bytes = file_size_bytes;
        store.upsert_catalog_sessions(&[changed]).unwrap();

        let (status, indexed_size, checkpoint_size): (String, Option<i64>, Option<i64>) =
            store
                .conn
                .query_row(
                    "SELECT indexed_status, indexed_file_size_bytes, last_imported_file_size_bytes FROM catalog_sessions WHERE source_path = ?1",
                    [source_path],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
        assert_eq!(status, CatalogIndexedStatus::Pending.as_str());
        assert_eq!(indexed_size, None);
        assert_eq!(checkpoint_size, None);
    }
}

#[test]
pub(crate) fn catalog_schema_includes_import_state_columns() {
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
}

#[test]
pub(crate) fn schema_v14_backfills_catalog_import_checkpoints() {
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
pub(crate) fn provider_check_constraints_accept_supported_providers() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    rebuild_capture_sources_provider_check(&store.conn).unwrap();
    rebuild_catalog_sessions_provider_check(&store.conn).unwrap();

    let schema = store.schema().unwrap();
    let providers = [
        ("codex", "codex_rollout_jsonl"),
        ("claude", "claude_projects_jsonl"),
        ("pi", "pi_sessions_jsonl"),
        ("opencode", "opencode_sqlite"),
        ("kilo", "kilo_sqlite"),
        ("kiro_cli", "kiro_cli_sqlite"),
        ("crush", "crush_sqlite"),
        ("goose", "goose_sessions_sqlite"),
        ("antigravity", "antigravity_history"),
        ("gemini", "gemini_history"),
        ("tabnine", "tabnine_history"),
        ("cursor", "cursor_sqlite"),
        ("windsurf", "windsurf_cascade_hook_transcript_jsonl"),
        ("zed", "zed_threads_sqlite"),
        ("copilot_cli", "copilot_cli_session_events_jsonl"),
        ("factory_ai_droid", "factory_ai_droid_sessions_jsonl"),
        ("qwen_code", "qwen_code_chat_jsonl"),
        ("kimi_code_cli", "kimi_code_cli_wire_jsonl"),
        ("forgecode", "forgecode_sqlite"),
        ("deepagents", "deepagents_sessions_sqlite"),
        ("mistral_vibe", "mistral_vibe_session_jsonl"),
        ("mux", "mux_session_jsonl"),
        ("rovodev", "rovodev_session_json"),
        ("openclaw", "openclaw_session_jsonl_tree"),
        ("hermes", "hermes_state_sqlite"),
        ("nanoclaw", "nanoclaw_project"),
        ("astrbot", "astrbot_data_v4_sqlite"),
        ("shelley", "shelley_sqlite"),
        ("continue", "continue_cli_sessions_json"),
        ("openhands", "openhands_file_events"),
        ("cline", "cline_task_directory_json"),
        ("roo_code", "cline_task_directory_json"),
        ("lingma", "lingma_sqlite"),
        ("qoder", "qoder_transcript_jsonl_tree"),
        ("warp", "warp_sqlite"),
        ("codebuddy", "codebuddy_history_json"),
        ("auggie", "auggie_session_json"),
        ("firebender", "firebender_chat_history_sqlite"),
        ("junie", "junie_session_events_jsonl_tree"),
        ("trae", "trae_state_vscdb"),
        ("shell", "shell_history"),
        ("git", "git_history"),
        ("jj", "jj_history"),
        ("gh", "gh_history"),
        ("custom", "ctx_history_jsonl_v1"),
        ("unknown", "unknown"),
    ];
    for (provider, source_format) in providers {
        assert!(
            schema.contains(provider),
            "schema provider checks should include {provider}"
        );
        store
            .conn
            .execute(
                r#"
                INSERT INTO capture_sources
                (id, kind, provider, machine_id, started_at_ms, fidelity)
                VALUES (?1, 'provider_import', ?2, 'test-machine', 0, 'partial')
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
    }

    let source_count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM capture_sources", [], |row| row.get(0))
        .unwrap();
    let catalog_count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM catalog_sessions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(source_count, providers.len() as i64);
    assert_eq!(catalog_count, providers.len() as i64);
}
