use std::collections::BTreeSet;

use ctx_history_core::{
    new_id, CaptureProvider, CaptureSource, CaptureSourceDescriptor, Event, EventRole, EventType,
};
use rusqlite::{params, Connection};

use super::fixtures::{
    archive_with_source, archive_with_source_and_session, fixed_time, imported_session,
    provider_archive_session, provider_archive_source, provider_archive_source_with_root,
    sync_metadata, tempdir,
};
use crate::schema::ddl::{table_has_column, CREATE_TABLES_SQL};
use crate::{Store, SCHEMA_VERSION};

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
