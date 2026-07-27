use crate::provider::importer::{provider_source_root_identity, provider_source_session_uuid};
use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::Path;
use uuid::Uuid;

pub(in crate::tests) fn delete_event_and_downgrade_provider_policy_cursor(
    database: &Path,
    store: &Store,
    machine_id: &str,
    stream: &str,
    event_id: Uuid,
) -> u64 {
    let cursor = store
        .get_sync_cursor(None, machine_id, stream)
        .unwrap()
        .expect("provider cursor exists after initial import");
    let mut encoded: Value = serde_json::from_str(&cursor.cursor).unwrap();
    let current_policy = encoded["o"]
        .as_u64()
        .expect("certified provider cursor has a policy revision");
    assert!(current_policy > 0);
    encoded["o"] = json!(current_policy - 1);

    let connection = Connection::open(database).unwrap();
    assert_eq!(
        connection
            .execute("DELETE FROM events WHERE id = ?1", [event_id.to_string()])
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .execute(
                "UPDATE sync_cursors SET cursor = ?1 WHERE device_id = ?2 AND stream = ?3",
                rusqlite::params![serde_json::to_string(&encoded).unwrap(), machine_id, stream],
            )
            .unwrap(),
        1
    );
    current_policy
}

pub(in crate::tests) fn only_provider_cursor_stream(database: &Path, machine_id: &str) -> String {
    let connection = Connection::open(database).unwrap();
    let mut statement = connection
        .prepare("SELECT stream FROM sync_cursors WHERE device_id = ?1 ORDER BY stream")
        .unwrap();
    let streams = statement
        .query_map([machine_id], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        streams.len(),
        1,
        "expected one provider cursor: {streams:?}"
    );
    streams.into_iter().next().unwrap()
}

pub(in crate::tests) fn assert_provider_policy_cursor_restored(
    store: &Store,
    machine_id: &str,
    stream: &str,
    expected_policy: u64,
) {
    let cursor = store
        .get_sync_cursor(None, machine_id, stream)
        .unwrap()
        .expect("provider cursor exists after repair");
    let encoded: Value = serde_json::from_str(&cursor.cursor).unwrap();
    assert_eq!(encoded["o"].as_u64(), Some(expected_policy));
}

pub(in crate::tests) fn provider_import_session_id_for_path(
    provider: CaptureProvider,
    source_format: &str,
    source_path: &Path,
    provider_session_id: &str,
) -> Uuid {
    let source_path = source_path.display().to_string();
    let source_identity = provider_source_root_identity(provider, source_format, &source_path);
    provider_source_session_uuid(&source_identity, provider_session_id)
}

pub(in crate::tests) fn stored_provider_session_id(
    store: &Store,
    provider: CaptureProvider,
    provider_session_id: &str,
) -> Uuid {
    let sessions = store
        .sessions_by_external_session_limited(provider, provider_session_id, 10)
        .unwrap();
    assert_eq!(
        sessions.len(),
        1,
        "expected exactly one stored session for {}/{}",
        provider.as_str(),
        provider_session_id
    );
    sessions[0].id
}
