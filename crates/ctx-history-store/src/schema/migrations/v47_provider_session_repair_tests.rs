// Companion end-to-end coverage for the disposable v47 repair module.
use std::fs;

use ctx_history_core::{new_id, CaptureProvider};
use rusqlite::{params, Connection};

use crate::schema::ddl::CREATE_TABLES_SQL;
use crate::schema::fts::FTS_TABLES_SQL;
use crate::schema::indexes::INDEXES_SQL;
use crate::{EventSearchBulkMaintenanceOutcome, Store};

fn tempdir() -> tempfile::TempDir {
    let root = std::env::var_os("TEST_TMPDIR")
        .map(|path| std::path::PathBuf::from(path).join("test-data"))
        .unwrap_or_else(|| std::env::current_dir().unwrap().join("target/test-data"));
    fs::create_dir_all(&root).unwrap();
    tempfile::Builder::new()
        .prefix("ctx-provider-session-identity-")
        .tempdir_in(root)
        .unwrap()
}

#[test]
fn schema_v47_repairs_provider_sessions_and_preserves_newer_state_and_id_aliases() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    let old_source_id = new_id();
    let new_source_id = new_id();
    let moved_source_id = new_id();
    let other_source_id = new_id();
    let old_session_id = new_id();
    let duplicate_session_id = new_id();
    let moved_session_id = new_id();
    let other_session_id = new_id();
    let parent_session_id = new_id();
    let old_event_id = new_id();
    let duplicate_event_id = new_id();
    let moved_event_id = new_id();
    let appended_event_id = new_id();
    let other_event_id = new_id();
    let file_touch_id = new_id();
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(CREATE_TABLES_SQL).unwrap();
        conn.execute_batch(FTS_TABLES_SQL).unwrap();
        conn.execute_batch(INDEXES_SQL).unwrap();
        conn.execute_batch("DROP TABLE event_aliases; DROP TABLE session_aliases;")
            .unwrap();
        for (id, path, source_format, source_identity) in [
            (old_source_id, "/tmp/codex/session.jsonl", None, None),
            (
                new_source_id,
                "/tmp/codex/session.jsonl",
                Some("codex_session_jsonl_tree"),
                Some("source-identity"),
            ),
            (
                moved_source_id,
                "/tmp/codex/moved/session.jsonl",
                Some("codex_session_jsonl_tree"),
                Some("source-identity"),
            ),
            (
                other_source_id,
                "/tmp/codex/copied/session.jsonl",
                Some("codex_session_jsonl_tree"),
                Some("other-source-identity"),
            ),
        ] {
            conn.execute(
                r#"
                INSERT INTO capture_sources
                (id, kind, provider, machine_id, raw_source_path, source_format,
                 source_root, source_identity, external_session_id, started_at_ms, fidelity)
                VALUES (?1, 'provider_import', 'codex', 'test-machine', ?2, ?3,
                        ?2, ?4, 'shared-provider-id', 0, 'imported')
                "#,
                params![id.to_string(), path, source_format, source_identity],
            )
            .unwrap();
        }
        for (
            id,
            source_id,
            external_session_id,
            parent_id,
            root_id,
            created_at_ms,
            updated_at_ms,
            generation,
        ) in [
            (
                parent_session_id,
                new_source_id,
                "parent-provider-id",
                None,
                None,
                0,
                0,
                "parent",
            ),
            (
                old_session_id,
                old_source_id,
                "shared-provider-id",
                None,
                None,
                1,
                1,
                "old",
            ),
            (
                duplicate_session_id,
                new_source_id,
                "shared-provider-id",
                Some(parent_session_id),
                Some(parent_session_id),
                2,
                5,
                "new",
            ),
            (
                moved_session_id,
                moved_source_id,
                "shared-provider-id",
                None,
                None,
                3,
                3,
                "moved-path",
            ),
            (
                other_session_id,
                other_source_id,
                "shared-provider-id",
                None,
                None,
                4,
                4,
                "other-path",
            ),
        ] {
            conn.execute(
                r#"
                INSERT INTO sessions
                (id, parent_session_id, root_session_id, capture_source_id, provider,
                 external_session_id, agent_type, is_primary, status, fidelity,
                 started_at_ms, created_at_ms, updated_at_ms, metadata_json)
                VALUES (?1, ?2, ?3, ?4, 'codex', ?5, 'primary',
                        1, 'imported', 'imported', 0, ?6, ?7,
                        json_object('generation', ?8))
                "#,
                params![
                    id.to_string(),
                    parent_id.map(|id| id.to_string()),
                    root_id.map(|id| id.to_string()),
                    source_id.to_string(),
                    external_session_id,
                    created_at_ms,
                    updated_at_ms,
                    generation,
                ],
            )
            .unwrap();
        }
        for (
            id,
            seq,
            session_id,
            source_id,
            provider_index,
            provider_hash,
            dedupe_key,
            search_text,
        ) in [
            (
                old_event_id,
                1,
                old_session_id,
                old_source_id,
                0,
                "event-0",
                "provider:codex:shared-provider-id:0:event-0",
                "canonical event searchable text",
            ),
            (
                duplicate_event_id,
                2,
                duplicate_session_id,
                new_source_id,
                0,
                "event-0",
                "provider-source:new-source:0:event-0",
                "duplicate event searchable text",
            ),
            (
                appended_event_id,
                3,
                duplicate_session_id,
                new_source_id,
                1,
                "event-1",
                "provider-source:new-source:1:event-1",
                "appended event searchable text",
            ),
            (
                moved_event_id,
                4,
                moved_session_id,
                moved_source_id,
                0,
                "event-0",
                "provider-source:moved-source:0:event-0",
                "moved event searchable text",
            ),
            (
                other_event_id,
                5,
                other_session_id,
                other_source_id,
                0,
                "other-event-0",
                "provider-source:other-source:0:other-event-0",
                "unrelated stored payload",
            ),
        ] {
            conn.execute(
                r#"
                INSERT INTO events
                (id, seq, session_id, event_type, role, occurred_at_ms,
                 capture_source_id, payload_json, dedupe_key, fidelity, metadata_json)
                VALUES (?1, ?2, ?3, 'message', 'assistant', ?2, ?4,
                        json_object('text', ?8), ?7,
                        'imported', json_object(
                            'provider_event_index', ?5,
                            'provider_event_hash', ?6
                        ))
                "#,
                params![
                    id.to_string(),
                    seq,
                    session_id.to_string(),
                    source_id.to_string(),
                    provider_index,
                    provider_hash,
                    dedupe_key,
                    search_text,
                ],
            )
            .unwrap();
        }
        for (event_id, session_id, preview) in [
            (old_event_id, old_session_id, "stale canonical projection"),
            (
                duplicate_event_id,
                duplicate_session_id,
                "stale duplicate projection",
            ),
            (
                appended_event_id,
                duplicate_session_id,
                "stale appended projection",
            ),
            (moved_event_id, moved_session_id, "stale moved projection"),
            (
                other_event_id,
                other_session_id,
                "unrelated projection must remain untouched",
            ),
        ] {
            conn.execute(
                r#"
                INSERT INTO event_search
                (event_id, session_id, role, preview_text, rank_bucket)
                VALUES (?1, ?2, 'assistant', ?3, 'message')
                "#,
                params![event_id.to_string(), session_id.to_string(), preview],
            )
            .unwrap();
            conn.execute(
                r#"
                INSERT INTO event_search_scriptgram
                (event_id, session_id, role, token_text, rank_bucket)
                VALUES (?1, ?2, 'assistant', ?3, 'message')
                "#,
                params![event_id.to_string(), session_id.to_string(), preview],
            )
            .unwrap();
            conn.execute(
                r#"
                INSERT INTO event_search_lookup
                (event_id, session_id, role, preview_text, rank_bucket)
                VALUES (?1, ?2, 'assistant', ?3, 'message')
                "#,
                params![event_id.to_string(), session_id.to_string(), preview],
            )
            .unwrap();
        }
        conn.execute(
            r#"
            INSERT INTO files_touched
            (id, event_id, path, confidence, created_at_ms, updated_at_ms, fidelity)
            VALUES (?1, ?2, 'src/lib.rs', 'explicit', 0, 0, 'imported')
            "#,
            params![file_touch_id.to_string(), duplicate_event_id.to_string()],
        )
        .unwrap();
        conn.execute_batch("PRAGMA user_version = 46;").unwrap();
    }

    let store = Store::open(&path).unwrap();
    let sessions = store.list_sessions().unwrap();
    assert_eq!(sessions.len(), 3, "unexpected sessions: {sessions:?}");
    assert_eq!(
        store.get_session(old_session_id).unwrap().capture_source_id,
        Some(new_source_id)
    );
    assert_eq!(
        store.get_session(duplicate_session_id).unwrap().id,
        old_session_id
    );
    assert_eq!(
        store.get_session(moved_session_id).unwrap().id,
        old_session_id
    );
    let repaired = store.get_session(old_session_id).unwrap();
    assert_eq!(repaired.parent_session_id, Some(parent_session_id));
    assert_eq!(repaired.root_session_id, Some(parent_session_id));
    assert_eq!(repaired.sync.metadata["generation"], "new");
    assert_eq!(
        store.get_event(duplicate_event_id).unwrap().id,
        old_event_id
    );
    assert_eq!(store.get_event(moved_event_id).unwrap().id, old_event_id);
    assert_eq!(
        store.get_event(appended_event_id).unwrap().session_id,
        Some(old_session_id)
    );
    assert_eq!(store.events_for_session(old_session_id).unwrap().len(), 2);
    assert!(store.event_search_projection_needs_backfill().unwrap());
    loop {
        match store.refresh_search_index().unwrap() {
            EventSearchBulkMaintenanceOutcome::Complete => break,
            EventSearchBulkMaintenanceOutcome::Pending => {}
        }
    }
    for projection_table in [
        "event_search",
        "event_search_scriptgram",
        "event_search_lookup",
    ] {
        assert_eq!(
            store
                .conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {projection_table} WHERE event_id IN (?1, ?2)"),
                    params![duplicate_event_id.to_string(), moved_event_id.to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0,
            "obsolete aliases remained in {projection_table}"
        );
    }
    for (event_id, expected_session_id, expected_preview) in [
        (
            old_event_id,
            old_session_id,
            "canonical event searchable text",
        ),
        (
            appended_event_id,
            old_session_id,
            "appended event searchable text",
        ),
    ] {
        assert_eq!(
            store
                .conn
                .query_row(
                    "SELECT session_id, preview_text FROM event_search WHERE event_id = ?1",
                    [event_id.to_string()],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .unwrap(),
            (expected_session_id.to_string(), expected_preview.to_owned())
        );
        assert_eq!(
            store
                .conn
                .query_row(
                    "SELECT session_id, preview_text FROM event_search_lookup WHERE event_id = ?1",
                    [event_id.to_string()],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .unwrap(),
            (expected_session_id.to_string(), expected_preview.to_owned())
        );
    }
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT preview_text FROM event_search WHERE event_id = ?1",
                [other_event_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "unrelated projection must remain untouched"
    );
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT token_text FROM event_search_scriptgram WHERE event_id = ?1",
                [other_event_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "unrelated projection must remain untouched"
    );
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT preview_text FROM event_search_lookup WHERE event_id = ?1",
                [other_event_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "unrelated projection must remain untouched"
    );
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT event_id FROM files_touched WHERE id = ?1",
                [file_touch_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        old_event_id.to_string()
    );
    assert_eq!(
        store
            .conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );

    let duplicate_insert = store.conn.execute(
        r#"
        INSERT INTO sessions
        (id, capture_source_id, provider, external_session_id, agent_type,
         is_primary, status, fidelity, started_at_ms, created_at_ms, updated_at_ms)
        VALUES (?1, ?2, 'codex', 'shared-provider-id', 'primary',
                1, 'imported', 'imported', 0, 4, 4)
        "#,
        params![new_id().to_string(), old_source_id.to_string()],
    );
    assert!(duplicate_insert
        .unwrap_err()
        .to_string()
        .contains("duplicate provider session"));
    assert_eq!(
        store
            .sessions_by_external_session_limited(CaptureProvider::Codex, "shared-provider-id", 10,)
            .unwrap()
            .len(),
        2,
        "the different raw source path must remain distinct"
    );
    drop(store);

    let reopened = Store::open(&path).unwrap();
    assert_eq!(reopened.list_sessions().unwrap().len(), 3);
    assert_eq!(
        reopened.get_session(duplicate_session_id).unwrap().id,
        old_session_id
    );
    assert_eq!(
        reopened.get_session(moved_session_id).unwrap().id,
        old_session_id
    );
}
