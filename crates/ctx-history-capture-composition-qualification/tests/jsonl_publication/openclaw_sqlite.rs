use std::{fs, path::Path};

use ctx_history_core::{CaptureProvider, CoreRecord};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use ctx_history_provider_openclaw_sqlite::test_support::set_before_openclaw_sqlite_terminal_revalidation_hook;
use rusqlite::{params, Connection};
use serde_json::json;

use crate::{
    ProviderCatalogSupport, ProviderImportSupport, ProviderSourceKind, ProviderSourceStatus,
};
use ctx_history_capture_composition::*;

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

fn database_path(root: &Path) -> std::path::PathBuf {
    root.join("agents/main/agent/openclaw-agent.sqlite")
}

fn create_database(path: &Path) -> Connection {
    fs::create_dir_all(path.parent().expect("OpenClaw database parent")).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(ctx_history_openclaw_schema::test_support::OPENCLAW_AGENT_V17_MINIMAL_SCHEMA)
        .unwrap();
    connection
        .execute(
            "INSERT INTO schema_meta\
               (meta_key, role, schema_version, agent_id, app_version, created_at, updated_at)\
             VALUES ('primary', 'agent', 17, 'main', 'test', 1, 1)",
            [],
        )
        .unwrap();
    connection
}

fn insert_active(
    connection: &Connection,
    session_id: &str,
    seq: i64,
    position: i64,
    event_id: &str,
    body: &str,
) {
    connection
        .execute(
            "INSERT OR IGNORE INTO session_windows\
               (session_id, session_key, reason, session_scope, created_at, updated_at,\
                session_entry_provenance, acp_owned)\
             VALUES (?1, ?2, 'initial', 'conversation', 1000, 1000, 0, 0)",
            params![session_id, format!("agent:main:{session_id}")],
        )
        .unwrap();
    let event = json!({
        "type": "message",
        "id": event_id,
        "timestamp": 1_700_000_000_000_i64 + seq,
        "message": {"role": "assistant", "content": body}
    });
    connection
        .execute(
            "INSERT INTO transcript_events (session_id, seq, event_json, created_at)\
             VALUES (?1, ?2, ?3, ?4)",
            params![
                session_id,
                seq,
                event.to_string(),
                1_700_000_000_000_i64 + seq
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO transcript_event_identities\
               (session_id, event_id, seq, event_type, parent_id, message_idempotency_key, created_at)\
             VALUES (?1, ?2, ?3, 'message', NULL, NULL, ?4)",
            params![session_id, event_id, seq, 1_700_000_000_000_i64 + seq],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO session_transcript_active_events\
               (session_id, active_position, event_seq, message_position)\
             VALUES (?1, ?2, ?3, ?2)",
            params![session_id, position, seq],
        )
        .unwrap();
    refresh_index_state(connection, session_id, seq, event_id);
}

fn refresh_index_state(connection: &Connection, session_id: &str, seq: i64, event_id: &str) {
    connection
        .execute(
            r#"INSERT INTO session_transcript_index_state
               (session_id, indexed_seq, leaf_event_id, needs_rebuild, active_event_count,
                  active_message_count, updated_at)
               VALUES (
                 ?1, ?2, ?3, 0,
                 (SELECT count(*) FROM session_transcript_active_events WHERE session_id = ?1),
                 (SELECT count(*) FROM session_transcript_active_events
                   WHERE session_id = ?1 AND message_position IS NOT NULL),
                 1
               )
               ON CONFLICT(session_id) DO UPDATE SET
                 indexed_seq = excluded.indexed_seq,
                 leaf_event_id = excluded.leaf_event_id,
                 needs_rebuild = 0,
                 active_event_count = (SELECT count(*) FROM session_transcript_active_events
                                        WHERE session_id = excluded.session_id),
                 active_message_count = (SELECT count(*) FROM session_transcript_active_events
                                          WHERE session_id = excluded.session_id
                                            AND message_position IS NOT NULL),
                 updated_at = excluded.updated_at"#,
            params![session_id, seq, event_id],
        )
        .unwrap();
}

fn rewrite_active(connection: &Connection, session_id: &str, seq: i64, event_id: &str, body: &str) {
    let event = json!({
        "type": "message",
        "id": event_id,
        "timestamp": 1_700_000_000_000_i64 + seq,
        "message": {"role": "assistant", "content": body}
    });
    connection
        .execute(
            "UPDATE transcript_events SET event_json = ?3 WHERE session_id = ?1 AND seq = ?2",
            params![session_id, seq, event.to_string()],
        )
        .unwrap();
}

fn registered_registry(data_root: &Path, database: &Path) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route_with_data_root(
        &mut registry,
        ProviderSource {
            provider: CaptureProvider::OpenClaw,
            path: database.to_path_buf(),
            exists: true,
            source_format: "openclaw_agent_sqlite",
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
            route_provenance: Default::default(),
        },
        SourceBackedRouteSelection::Automatic,
        data_root,
    )
    .unwrap();
    registry
}

fn records(index_root: &Path) -> Vec<CoreRecord> {
    let index = VerifiedIndex::open_pinned(index_root).unwrap();
    let mut records = index
        .manifest()
        .sources
        .iter()
        .flat_map(|source| {
            index
                .core_source_event_page(source.observation().source(), None, 128)
                .unwrap()
                .items
                .into_iter()
                .map(|item| item.core_record)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    records.sort_by_key(|record| record.event_sequence);
    records
}

fn refresh(
    index_root: &Path,
    registry: &SourceBackedProviderRegistry,
) -> SourceBackedRefreshReceipt {
    refresh_source_backed_generation(index_root, registry, writer_options()).unwrap()
}

#[test]
fn registered_adapter_replaces_append_rewrite_active_delete_and_database_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("ctx-data");
    let index_root = temp.path().join("index");
    let database = database_path(&temp.path().join("provider"));
    let connection = create_database(&database);
    insert_active(&connection, "session-a", 1, 0, "event-a", "cold body");
    let registry = registered_registry(&data_root, &database);

    let cold_receipt = refresh(&index_root, &registry);
    let cold = records(&index_root);
    assert_eq!(cold.len(), 1);

    let unchanged = refresh(&index_root, &registry);
    assert_eq!(
        unchanged.commit.generation_id,
        cold_receipt.commit.generation_id
    );
    assert_eq!(records(&index_root), cold);

    insert_active(&connection, "session-a", 2, 1, "event-b", "append body");
    refresh(&index_root, &registry);
    let appended = records(&index_root);
    assert_eq!(appended.len(), 2);
    assert_eq!(cold[0].session_id, appended[0].session_id);
    assert_eq!(cold[0].event_id, appended[0].event_id);

    rewrite_active(&connection, "session-a", 1, "event-a", "rewritten body");
    refresh(&index_root, &registry);
    let rewritten = records(&index_root);
    assert_eq!(rewritten[0].event_id, cold[0].event_id);
    assert_eq!(rewritten[0].content.meaningful_text(), "rewritten body");

    connection
        .execute(
            "DELETE FROM session_transcript_active_events WHERE session_id = 'session-a' AND event_seq = 1",
            [],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE session_transcript_active_events SET active_position = 0, message_position = 0 \
             WHERE session_id = 'session-a' AND event_seq = 2",
            [],
        )
        .unwrap();
    refresh_index_state(&connection, "session-a", 2, "event-b");
    refresh(&index_root, &registry);
    let deleted = records(&index_root);
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].content.meaningful_text(), "append body");

    drop(connection);
    fs::rename(&database, database.with_extension("retired.sqlite")).unwrap();
    let replacement = create_database(&database);
    insert_active(
        &replacement,
        "replacement-session",
        7,
        0,
        "replacement-event",
        "replacement body",
    );
    refresh(&index_root, &registry);
    let replaced = records(&index_root);
    assert_eq!(replaced.len(), 1);
    assert_eq!(replaced[0].content.meaningful_text(), "replacement body");
    assert_ne!(replaced[0].session_id, deleted[0].session_id);
}

#[test]
fn registered_adapter_terminal_revalidation_rejects_concurrent_change_and_retry_converges() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("ctx-data");
    let index_root = temp.path().join("index");
    let database = database_path(&temp.path().join("provider"));
    let connection = create_database(&database);
    insert_active(&connection, "session-a", 1, 0, "event-a", "stable body");
    let registry = registered_registry(&data_root, &database);
    let cold = refresh(&index_root, &registry);

    let changing_database = database.clone();
    set_before_openclaw_sqlite_terminal_revalidation_hook(move || {
        let connection = Connection::open(changing_database).unwrap();
        rewrite_active(
            &connection,
            "session-a",
            1,
            "event-a",
            "changed during scan",
        );
    });
    let failed = refresh(&index_root, &registry);
    assert_eq!(failed.failed_routes.len(), 1);
    assert_eq!(
        failed.failed_routes[0].class,
        SourceBackedSourceFailureClass::SourceChanged
    );
    assert_eq!(
        VerifiedIndex::open_pinned(&index_root)
            .unwrap()
            .generation_id(),
        cold.commit.generation_id
    );
    assert_eq!(
        records(&index_root)[0].content.meaningful_text(),
        "stable body"
    );

    refresh(&index_root, &registry);
    assert_eq!(
        records(&index_root)[0].content.meaningful_text(),
        "changed during scan"
    );
}
