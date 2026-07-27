use std::fs;

use ctx_history_core::CaptureProvider;
use rusqlite::{limits::Limit as SqliteLimit, Connection};
use serde_json::json;

use crate::captured_batch::sqlite_logical_rows::SqliteLogicalRowBatchProducer;
use crate::captured_batch::{
    CapturedRecordPayload, NativePosition, SourceObservation,
    CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES, CAPTURE_BATCH_MAX_PAYLOAD_BYTES,
};
use crate::provider::importer::{BoundedParserCheckpoint, CertifiedProviderCursor};
use crate::provider::sqlite::open_provider_sqlite_readonly;
use crate::provider::sqlite::with_sqlite_read_snapshot;
use crate::CaptureError;

use super::sqlite::{
    decode_trae_position, initial_trae_position, trae_source_snapshot, trae_sqlite_batch_error,
    with_trae_length_preflight, TraeRowFetcher,
};
use super::{
    with_trae_source_revalidation, TRAE_CAPTURE_REVISION, TRAE_CHAT_KEYS,
    TRAE_CHAT_ROW_LOCATOR_KIND, TRAE_CHAT_VALUE_RECORD_KIND, TRAE_FRONTIER_LOCATOR_KIND,
    TRAE_FRONTIER_RECORD_KIND, TRAE_POLICY_REVISION, TRAE_POSITION_BYTES,
    TRAE_STATE_VSCDB_SOURCE_FORMAT,
};

fn create_item_table(conn: &Connection) {
    conn.execute_batch("create table ItemTable (key text primary key, value);")
        .unwrap();
}

fn source(label: &str) -> SourceObservation {
    SourceObservation::new(
        CaptureProvider::Trae,
        TRAE_STATE_VSCDB_SOURCE_FORMAT,
        format!("trae-sqlite:{label}"),
        format!("snapshot:{label}"),
        format!("provider:trae:{label}"),
        TRAE_CAPTURE_REVISION,
        TRAE_POLICY_REVISION,
        None,
    )
    .unwrap()
}

#[test]
fn trae_length_preflight_restores_limit_after_query_error() {
    let conn = Connection::open_in_memory().unwrap();
    let lowered_limit = 64 * 1024;
    conn.set_limit(SqliteLimit::SQLITE_LIMIT_LENGTH, lowered_limit);

    let result = with_trae_length_preflight(&conn, || {
        conn.query_row::<i64, _, _>("select missing from missing_table", [], |row| row.get(0))
    });

    assert!(result.is_err());
    assert_eq!(conn.limit(SqliteLimit::SQLITE_LIMIT_LENGTH), lowered_limit);
}

#[test]
fn trae_capped_preflight_rejects_oversize_without_hydration_and_restores_limit() {
    let conn = Connection::open_in_memory().unwrap();
    create_item_table(&conn);
    conn.execute(
        "insert into ItemTable (key, value) values (?1, zeroblob(?2))",
        rusqlite::params![
            TRAE_CHAT_KEYS[0],
            i64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES).unwrap(),
        ],
    )
    .unwrap();
    let sqlite_value_limit = i32::try_from(crate::MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap();
    conn.set_limit(SqliteLimit::SQLITE_LIMIT_LENGTH, sqlite_value_limit);

    let mut fetcher = TraeRowFetcher::new(&conn, 1).unwrap();
    let row = fetcher
        .fetch(initial_trae_position().unwrap())
        .unwrap()
        .unwrap();

    assert!(matches!(
        row.record().payload(),
        CapturedRecordPayload::StructuralRejection { .. }
    ));
    assert_eq!(fetcher.hydrated_chat_values, 0);
    assert_eq!(
        conn.limit(SqliteLimit::SQLITE_LIMIT_LENGTH),
        sqlite_value_limit
    );
}

#[test]
fn empty_and_excluded_source_advances_to_a_replay_stable_frontier() {
    let conn = Connection::open_in_memory().unwrap();
    create_item_table(&conn);
    conn.execute(
        "insert into ItemTable (key, value) values (?1, ?2)",
        rusqlite::params![
            TRAE_CHAT_KEYS[0],
            json!({"list": [{"id": "empty", "messages": [{"content": " "}]}]}).to_string(),
        ],
    )
    .unwrap();
    let mut fetcher = TraeRowFetcher::new(&conn, 1).unwrap();
    let source = source("test");
    let mut producer = SqliteLogicalRowBatchProducer::new(
        source,
        initial_trae_position().unwrap(),
        move |position| fetcher.fetch(position),
    );
    let batch = producer
        .next_batch()
        .map_err(trae_sqlite_batch_error)
        .unwrap()
        .unwrap();
    assert!(batch.source_exhausted());
    let terminal = decode_trae_position(batch.range_end()).unwrap().unwrap();
    assert_eq!(usize::from(terminal.key_index), TRAE_CHAT_KEYS.len());
    assert!(producer
        .next_batch()
        .map_err(trae_sqlite_batch_error)
        .unwrap()
        .is_none());
}

#[test]
fn shared_producer_preserves_trae_batch_boundaries_positions_locators_and_order() {
    let conn = Connection::open_in_memory().unwrap();
    create_item_table(&conn);
    let padding = CAPTURE_BATCH_MAX_PAYLOAD_BYTES / 2 + 4 * 1024;
    let first_value = json!({
        "padding": "a".repeat(padding),
        "list": [{"id": "first", "messages": [{"id": "first-message", "content": "first"}]}],
    })
    .to_string();
    let second_value = json!({
        "padding": "b".repeat(padding),
        "list": [{"id": "second", "messages": [{"id": "second-message", "content": "second"}]}],
    })
    .to_string();
    assert!(first_value.len() + second_value.len() > CAPTURE_BATCH_MAX_PAYLOAD_BYTES);

    // SQLite insertion order is deliberately opposite the provider's stable key order.
    conn.execute(
        "insert into ItemTable (key, value) values (?1, ?2)",
        rusqlite::params![TRAE_CHAT_KEYS[1], second_value],
    )
    .unwrap();
    conn.execute(
        "insert into ItemTable (key, value) values (?1, ?2)",
        rusqlite::params![TRAE_CHAT_KEYS[0], first_value],
    )
    .unwrap();

    let initial = initial_trae_position().unwrap();
    let mut fetcher = TraeRowFetcher::new(&conn, 1).unwrap();
    let mut producer = SqliteLogicalRowBatchProducer::new(
        source("shared-parity"),
        initial.clone(),
        move |position| fetcher.fetch(position),
    );
    let first = producer
        .next_batch()
        .map_err(trae_sqlite_batch_error)
        .unwrap()
        .unwrap();
    assert_eq!(first.range_before(), &initial);
    assert_eq!(
        first.source().source_format(),
        TRAE_STATE_VSCDB_SOURCE_FORMAT
    );
    assert_eq!(first.source().capture_revision(), TRAE_CAPTURE_REVISION);
    assert_eq!(first.source().policy_revision(), TRAE_POLICY_REVISION);
    assert!(!first.source_exhausted());
    assert_eq!(first.records().len(), 1);
    let first_record = &first.records()[0];
    assert_eq!(first_record.ordinal(), 0);
    assert_eq!(
        first_record.record_kind().as_str(),
        TRAE_CHAT_VALUE_RECORD_KIND
    );
    assert_eq!(first_record.locator().kind(), TRAE_CHAT_ROW_LOCATOR_KIND);
    assert_eq!(first_record.locator().value(), 0_u16.to_be_bytes());
    assert!(matches!(
        first_record.payload(),
        CapturedRecordPayload::NativeBytes(bytes) if bytes.as_slice() == first_value.as_bytes()
    ));
    let first_end = decode_trae_position(first.range_end()).unwrap().unwrap();
    assert_eq!(first_end.key_index, 1);
    assert_eq!(first_end.next_ordinal, 1);

    let second = producer
        .next_batch()
        .map_err(trae_sqlite_batch_error)
        .unwrap()
        .unwrap();
    assert_eq!(second.range_before(), first.range_end());
    assert!(second.source_exhausted());
    assert_eq!(second.records().len(), 2);
    let second_record = &second.records()[0];
    assert_eq!(second_record.ordinal(), 1);
    assert_eq!(
        second_record.record_kind().as_str(),
        TRAE_CHAT_VALUE_RECORD_KIND
    );
    assert_eq!(second_record.locator().kind(), TRAE_CHAT_ROW_LOCATOR_KIND);
    assert_eq!(second_record.locator().value(), 1_u16.to_be_bytes());
    assert!(matches!(
        second_record.payload(),
        CapturedRecordPayload::NativeBytes(bytes) if bytes.as_slice() == second_value.as_bytes()
    ));

    let frontier = &second.records()[1];
    assert_eq!(frontier.ordinal(), 2);
    assert_eq!(frontier.record_kind().as_str(), TRAE_FRONTIER_RECORD_KIND);
    assert_eq!(frontier.locator().kind(), TRAE_FRONTIER_LOCATOR_KIND);
    let mut expected_frontier_locator = Vec::new();
    expected_frontier_locator
        .extend_from_slice(&u16::try_from(TRAE_CHAT_KEYS.len()).unwrap().to_be_bytes());
    expected_frontier_locator.extend_from_slice(&0_u32.to_be_bytes());
    expected_frontier_locator.extend_from_slice(&0_u32.to_be_bytes());
    assert_eq!(frontier.locator().value(), expected_frontier_locator);
    assert!(matches!(
        frontier.payload(),
        CapturedRecordPayload::NativeBytes(bytes) if bytes.is_empty()
    ));
    let terminal = decode_trae_position(second.range_end()).unwrap().unwrap();
    assert_eq!(usize::from(terminal.key_index), TRAE_CHAT_KEYS.len());
    assert_eq!(terminal.next_ordinal, 3);
    assert!(producer
        .next_batch()
        .map_err(trae_sqlite_batch_error)
        .unwrap()
        .is_none());
}

#[test]
fn shared_producer_batch_scope_revalidates_the_trae_source_after_capture() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("state.vscdb");
    let writer = Connection::open(&path).unwrap();
    create_item_table(&writer);
    writer
        .execute(
            "insert into ItemTable (key, value) values (?1, ?2)",
            rusqlite::params![TRAE_CHAT_KEYS[0], json!({"list": []}).to_string()],
        )
        .unwrap();
    drop(writer);

    let snapshot = trae_source_snapshot(&path).unwrap();
    let conn = open_provider_sqlite_readonly(&path).unwrap();
    let mut fetcher = TraeRowFetcher::new(&conn, 1).unwrap();
    let mut producer = SqliteLogicalRowBatchProducer::new(
        source("revalidation"),
        initial_trae_position().unwrap(),
        move |position| fetcher.fetch(position),
    );
    let original_permissions = fs::metadata(&path).unwrap().permissions();
    let result = with_trae_source_revalidation(&snapshot, &path, || {
        let batch = with_sqlite_read_snapshot(&conn, || {
            producer.next_batch().map_err(trae_sqlite_batch_error)
        })?;
        let mut changed_permissions = original_permissions.clone();
        changed_permissions.set_readonly(!changed_permissions.readonly());
        fs::set_permissions(&path, changed_permissions)?;
        Ok(batch)
    });
    fs::set_permissions(&path, original_permissions).unwrap();

    assert!(matches!(
        result,
        Err(CaptureError::SourceChangedDuringCapture)
    ));
}

#[test]
fn legacy_v2_mid_key_and_done_cursors_reset_but_v3_terminal_replays_exactly() {
    fn legacy_v2_position(
        key_index: u16,
        session_index: u32,
        message_index: u32,
        next_ordinal: u64,
    ) -> NativePosition {
        let mut value = Vec::with_capacity(TRAE_POSITION_BYTES);
        value.push(1);
        value.extend_from_slice(&key_index.to_be_bytes());
        value.extend_from_slice(&session_index.to_be_bytes());
        value.extend_from_slice(&message_index.to_be_bytes());
        value.extend_from_slice(&next_ordinal.to_be_bytes());
        NativePosition::new("trae-itemtable-message-keyset-v1", value).unwrap()
    }

    for legacy in [
        legacy_v2_position(0, 3, 64, 64),
        legacy_v2_position(u16::try_from(TRAE_CHAT_KEYS.len()).unwrap(), 0, 0, 65),
    ] {
        let certified = CertifiedProviderCursor::new(
            "snapshot:test",
            2,
            TRAE_POLICY_REVISION,
            legacy.clone(),
            BoundedParserCheckpoint::from_serializable(&()).unwrap(),
        )
        .unwrap();
        assert!(!certified.matches_revisions(
            "snapshot:test",
            TRAE_CAPTURE_REVISION,
            TRAE_POLICY_REVISION,
        ));
        assert!(decode_trae_position(&legacy).is_err());
    }

    let conn = Connection::open_in_memory().unwrap();
    create_item_table(&conn);
    let mut first = TraeRowFetcher::new(&conn, 1).unwrap();
    let frontier = first
        .fetch(initial_trae_position().unwrap())
        .unwrap()
        .unwrap();
    let terminal = frontier.next_position().clone();
    let decoded = decode_trae_position(&terminal).unwrap().unwrap();
    assert_eq!(usize::from(decoded.key_index), TRAE_CHAT_KEYS.len());
    assert!(first.fetch(terminal.clone()).unwrap().is_none());

    let mut replay = TraeRowFetcher::new(&conn, 1).unwrap();
    assert!(replay.fetch(terminal).unwrap().is_none());
    assert_eq!(replay.candidate_queries, 0);
    assert_eq!(replay.hydrated_chat_values, 0);
}
