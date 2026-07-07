use std::{collections::BTreeSet, fs};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    new_id, AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, EntityTimestamps,
    Event, EventRole, EventType, Fidelity, RedactionState, Session, SessionHistoryArchive,
    SessionStatus, SyncMetadata, SyncState, Visibility,
};
use rusqlite::{params, Connection};
use uuid::Uuid;

use crate::schema::ddl::{table_exists, table_has_column, CREATE_TABLES_SQL};
use crate::schema::fts::FTS_TABLES_SQL;
use crate::schema::indexes::INDEXES_SQL;
use crate::schema::migrations::{
    rebuild_capture_sources_provider_check, rebuild_catalog_sessions_provider_check,
};
use crate::{Store, SCHEMA_VERSION};

fn tempdir() -> tempfile::TempDir {
    let root = std::env::var_os("TEST_TMPDIR")
        .map(|path| std::path::PathBuf::from(path).join("test-data"))
        .unwrap_or_else(|| std::env::current_dir().unwrap().join("target/test-data"));
    fs::create_dir_all(&root).unwrap();
    tempfile::Builder::new()
        .prefix("ctx-history-store-schema-")
        .tempdir_in(root)
        .unwrap()
}

fn fixed_time() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-06-23T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn timestamps() -> EntityTimestamps {
    EntityTimestamps {
        created_at: fixed_time(),
        updated_at: fixed_time(),
    }
}

fn sync_metadata() -> SyncMetadata {
    SyncMetadata {
        visibility: Visibility::LocalOnly,
        fidelity: Fidelity::Imported,
        sync_state: SyncState::LocalOnly,
        sync_version: 0,
        deleted_at: None,
        metadata: serde_json::json!({}),
    }
}

fn imported_session(external_session_id: &str) -> Session {
    Session {
        id: new_id(),
        history_record_id: None,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: None,
        provider: CaptureProvider::Codex,
        external_session_id: Some(external_session_id.into()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("primary".into()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: fixed_time(),
        ended_at: None,
        timestamps: timestamps(),
        sync: sync_metadata(),
    }
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

fn legacy_history_record_sql(sql: &str) -> String {
    sql.replace("history_record_links", "work_record_links")
        .replace("history_record_tags", "work_record_tags")
        .replace("history_records", "work_records")
        .replace("history_record_id", "work_record_id")
}

#[test]
fn provider_check_constraints_accept_supported_providers() {
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

#[test]
fn archive_import_allows_multiple_capture_sources_for_same_provider_session() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let external_session_id = "provider-session-1";
    let first_source = provider_archive_source(
        "018f45d0-0000-7000-8000-000000080001",
        external_session_id,
        "/tmp/provider/first.jsonl",
    );
    let second_source = provider_archive_source(
        "018f45d0-0000-7000-8000-000000080002",
        external_session_id,
        "/tmp/provider/second.jsonl",
    );

    store
        .import_archive(&archive_with_source(first_source.clone()), false)
        .unwrap();
    store
        .import_archive(&archive_with_source(second_source.clone()), false)
        .unwrap();

    let sources = store.list_capture_sources().unwrap();
    assert_eq!(sources.len(), 2);
    assert_eq!(
        sources
            .iter()
            .map(|source| source.id)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([first_source.id, second_source.id])
    );
    assert!(sources.iter().all(
        |source| source.descriptor.external_session_id.as_deref() == Some(external_session_id)
    ));
}

#[test]
fn archive_import_allows_source_scoped_sessions_for_same_provider_session() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let external_session_id = "provider-session-1";
    let first_source = provider_archive_source_with_root(
        "018f45d0-0000-7000-8000-000000080011",
        external_session_id,
        "/tmp/provider/first/session.jsonl",
        "/tmp/provider/first",
    );
    let second_source = provider_archive_source_with_root(
        "018f45d0-0000-7000-8000-000000080012",
        external_session_id,
        "/tmp/provider/second/session.jsonl",
        "/tmp/provider/second",
    );
    let first_session = provider_archive_session(
        "018f45d0-0000-7000-8000-000000080013",
        first_source.id,
        external_session_id,
    );
    let second_session = provider_archive_session(
        "018f45d0-0000-7000-8000-000000080014",
        second_source.id,
        external_session_id,
    );

    store
        .import_archive(
            &archive_with_source_and_session(first_source, first_session),
            false,
        )
        .unwrap();
    store
        .import_archive(
            &archive_with_source_and_session(second_source, second_session),
            false,
        )
        .unwrap();

    let sessions = store.list_sessions().unwrap();
    assert_eq!(sessions.len(), 2);
    assert!(sessions
        .iter()
        .all(|session| session.external_session_id.as_deref() == Some(external_session_id)));
}

#[test]
fn archive_import_rejects_duplicate_provider_session_in_same_source() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let external_session_id = "provider-session-1";
    let source = provider_archive_source_with_root(
        "018f45d0-0000-7000-8000-000000080021",
        external_session_id,
        "/tmp/provider/source/session.jsonl",
        "/tmp/provider/source",
    );
    let first_session = provider_archive_session(
        "018f45d0-0000-7000-8000-000000080022",
        source.id,
        external_session_id,
    );
    let second_session = provider_archive_session(
        "018f45d0-0000-7000-8000-000000080023",
        source.id,
        external_session_id,
    );

    store
        .import_archive(
            &archive_with_source_and_session(source.clone(), first_session),
            false,
        )
        .unwrap();
    let error = store
        .import_archive(
            &archive_with_source_and_session(source, second_session.clone()),
            false,
        )
        .unwrap_err();
    assert!(
        matches!(error, crate::StoreError::ImportConflict { kind: "session", id } if id == second_session.id),
        "expected same-source session conflict, got {error:?}"
    );
}

fn archive_with_source(source: CaptureSource) -> SessionHistoryArchive {
    SessionHistoryArchive {
        capture_sources: vec![source],
        ..SessionHistoryArchive::default()
    }
}

fn archive_with_source_and_session(
    source: CaptureSource,
    session: Session,
) -> SessionHistoryArchive {
    SessionHistoryArchive {
        capture_sources: vec![source],
        sessions: vec![session],
        ..SessionHistoryArchive::default()
    }
}

fn provider_archive_source(
    id: &str,
    external_session_id: &str,
    raw_source_path: &str,
) -> CaptureSource {
    provider_archive_source_with_root(id, external_session_id, raw_source_path, "/repo")
}

fn provider_archive_source_with_root(
    id: &str,
    external_session_id: &str,
    raw_source_path: &str,
    source_root: &str,
) -> CaptureSource {
    CaptureSource {
        id: Uuid::parse_str(id).unwrap(),
        descriptor: CaptureSourceDescriptor {
            kind: ctx_history_core::CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Claude,
            machine_id: "test-machine".to_owned(),
            process_id: None,
            cwd: Some("/repo".to_owned()),
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some("claude_projects_jsonl_tree".to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: None,
            external_session_id: Some(external_session_id.to_owned()),
        },
        started_at: fixed_time(),
        ended_at: None,
        sync: sync_metadata(),
    }
}

fn provider_archive_session(id: &str, source_id: Uuid, external_session_id: &str) -> Session {
    Session {
        id: Uuid::parse_str(id).unwrap(),
        provider: CaptureProvider::Claude,
        capture_source_id: Some(source_id),
        external_session_id: Some(external_session_id.to_owned()),
        ..imported_session(external_session_id)
    }
}

#[test]
fn schema_v16_rebuilds_provider_checks_with_referenced_sources_and_indexes() {
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
                source_format: Some("codex_session_jsonl".to_owned()),
                source_root: Some("/home/user/.codex/sessions".to_owned()),
                source_identity: None,
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

#[test]
fn schema_v43_adds_capture_source_identity_columns() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    let source_id = new_id();
    {
        let conn = Connection::open(&path).unwrap();
        let legacy_sql = CREATE_TABLES_SQL.replace(
            "    source_format TEXT,\n    source_root TEXT,\n    source_identity TEXT,\n",
            "",
        );
        conn.execute_batch(&legacy_sql).unwrap();
        conn.execute(
            r#"
            INSERT INTO capture_sources
            (id, kind, provider, machine_id, raw_source_path, external_session_id, started_at_ms, fidelity)
            VALUES (?1, 'provider_import', 'codex', 'test-machine', '/home/user/.codex/sessions/root.jsonl', 'root-local-session', 1, 'imported')
            "#,
            params![source_id.to_string()],
        )
        .unwrap();
        conn.execute_batch("PRAGMA user_version = 42;").unwrap();
    }

    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    for column in ["source_format", "source_root", "source_identity"] {
        assert!(table_has_column(&store.conn, "capture_sources", column).unwrap());
    }
    let source = store.get_capture_source(source_id).unwrap();
    assert_eq!(
        source.descriptor.source_root.as_deref(),
        Some("/home/user/.codex/sessions/root.jsonl")
    );
    let exists: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_capture_sources_provider_source_identity'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(exists, 1);
}

#[test]
fn schema_v17_adds_jsonl_provider_checks() {
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
fn schema_v18_adds_codebuddy_provider_checks() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        let legacy_sql = CREATE_TABLES_SQL.replace(", 'codebuddy'", "");
        conn.execute_batch(&legacy_sql).unwrap();
        conn.execute_batch(INDEXES_SQL).unwrap();
        conn.execute_batch("PRAGMA user_version = 17;").unwrap();
    }

    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    let provider = "codebuddy";
    let source_format = "codebuddy_history_json";
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
            params![
                format!("/tmp/provider/{provider}/session/index.json"),
                provider,
                source_format
            ],
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
                format!("/tmp/provider/{provider}/session/index.json")
            ],
        )
        .unwrap();
}

#[test]
fn schema_v19_adds_zed_provider_checks() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        let legacy_sql = CREATE_TABLES_SQL.replace(", 'zed'", "");
        conn.execute_batch(&legacy_sql).unwrap();
        conn.execute_batch(INDEXES_SQL).unwrap();
        conn.execute_batch("PRAGMA user_version = 18;").unwrap();
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
            VALUES (?1, 'provider_import', 'zed', 'test-machine', 0, 'imported')
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
            VALUES ('/tmp/zed/threads.db', 'zed', 'zed_threads_sqlite', '/tmp/zed/threads.db', 'primary', 1, 0, 0)
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
            VALUES ('zed', 'zed_threads_sqlite', '/tmp/zed/threads.db', '/tmp/zed/threads.db', 1, 0, 0)
            "#,
            [],
        )
        .unwrap();
}

#[test]
fn schema_v20_adds_kiro_cli_provider_checks() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        let legacy_sql = CREATE_TABLES_SQL.replace(", 'kiro_cli'", "");
        conn.execute_batch(&legacy_sql).unwrap();
        conn.execute_batch(INDEXES_SQL).unwrap();
        conn.execute_batch("PRAGMA user_version = 19;").unwrap();
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
            VALUES (?1, 'provider_import', 'kiro_cli', 'test-machine', 0, 'imported')
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
            VALUES ('/tmp/kiro/data.sqlite3', 'kiro_cli', 'kiro_cli_sqlite', '/tmp/kiro', 'primary', 1, 0, 0)
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
            VALUES ('kiro_cli', 'kiro_cli_sqlite', '/tmp/kiro', '/tmp/kiro/data.sqlite3', 1, 0, 0)
            "#,
            [],
        )
        .unwrap();
}

#[test]
fn schema_v22_adds_forgecode_provider_checks() {
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

#[test]
fn schema_v23_adds_mistral_vibe_provider_checks() {
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

#[test]
fn schema_v24_adds_deepagents_mux_and_lingma_provider_checks() {
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

#[test]
fn schema_v25_adds_rovodev_provider_checks() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        let legacy_sql = CREATE_TABLES_SQL.replace(", 'rovodev'", "");
        conn.execute_batch(&legacy_sql).unwrap();
        conn.execute_batch(INDEXES_SQL).unwrap();
        conn.execute_batch("PRAGMA user_version = 24;").unwrap();
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
            VALUES (?1, 'provider_import', 'rovodev', 'test-machine', 0, 'imported')
            "#,
            params![new_id().to_string()],
        )
        .unwrap();

    let source_path = "/tmp/rovodev/sessions/session/session_context.json";
    let provider = "rovodev";
    let source_format = "rovodev_session_json";
    let source_root = "/tmp/rovodev/sessions";
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
#[test]
fn schema_v27_adds_windsurf_provider_checks() {
    assert_provider_migration_accepts(
        26,
        "windsurf",
        "windsurf_cascade_hook_transcript_jsonl",
        "/tmp/windsurf/transcripts",
        "/tmp/windsurf/transcripts/trajectory.jsonl",
    );
}
#[test]
fn schema_v30_adds_auggie_provider_checks() {
    assert_provider_migration_accepts(
        29,
        "auggie",
        "auggie_session_json",
        "/tmp/augment/sessions",
        "/tmp/augment/sessions/session.json",
    );
}

#[test]
fn schema_v31_adds_firebender_provider_checks() {
    assert_provider_migration_accepts(
        30,
        "firebender",
        "firebender_chat_history_sqlite",
        "/tmp/project/.idea/firebender/chat_history.db",
        "/tmp/project/.idea/firebender/chat_history.db",
    );
}
#[test]
fn schema_v35_adds_trae_provider_checks() {
    assert_provider_migration_accepts(
        34,
        "trae",
        "trae_state_vscdb",
        "/tmp/Trae/User/workspaceStorage",
        "/tmp/Trae/User/workspaceStorage/workspace/state.vscdb",
    );
}

#[test]
fn schema_v36_adds_warp_provider_checks() {
    assert_provider_migration_accepts(
        35,
        "warp",
        "warp_sqlite",
        "/tmp/warp-terminal",
        "/tmp/warp-terminal/warp.sqlite",
    );
}

#[test]
fn schema_v37_adds_qoder_provider_checks() {
    assert_provider_migration_accepts(
        36,
        "qoder",
        "qoder_transcript_jsonl_tree",
        "/tmp/qoder/projects",
        "/tmp/qoder/projects/workspace/transcript/session.jsonl",
    );
}
#[test]
fn schema_v40_adds_junie_provider_checks() {
    assert_provider_migration_accepts(
        39,
        "junie",
        "junie_session_events_jsonl_tree",
        "/tmp/junie/sessions",
        "/tmp/junie/sessions/session-260607-100000-acme/events.jsonl",
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
