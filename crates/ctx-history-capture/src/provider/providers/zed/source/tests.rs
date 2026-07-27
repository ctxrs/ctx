use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use ctx_history_core::CaptureProvider;
use rusqlite::limits::Limit as SqliteLimit;

use super::*;
use crate::captured_batch::sqlite_logical_rows::SqliteLogicalRowBatchProducer;
use crate::captured_batch::{CapturedRecordPayload, SourceObservation, StructuralRejectionKind};
use crate::ZED_THREADS_SQLITE_SOURCE_FORMAT;

fn create_threads_schema(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE threads(\
            id TEXT PRIMARY KEY, parent_id TEXT, folder_paths TEXT, folder_paths_order TEXT,\
            summary TEXT NOT NULL, updated_at TEXT NOT NULL, data_type TEXT NOT NULL,\
            data BLOB NOT NULL, created_at TEXT\
         );",
    )
    .unwrap();
}

#[test]
fn zed_cursor_round_trips_exact_rowid_and_ordinal() {
    let encoded = encode_zed_position(ZedKeyset {
        phase: ZedCapturePhase::Rows,
        next_ordinal: 65,
        rowid: -7,
    })
    .unwrap();
    assert_eq!(encoded.value().len(), ZED_POSITION_BYTES);
    let decoded = decode_zed_position(&encoded).unwrap().unwrap();
    assert_eq!(decoded.phase, ZedCapturePhase::Rows);
    assert_eq!(decoded.next_ordinal, 65);
    assert_eq!(decoded.rowid, -7);
    assert!(decode_zed_position(&NativePosition::new("other-position", vec![0]).unwrap()).is_err());
}

#[test]
fn zed_length_preflight_restores_limit_after_query_error() {
    let conn = Connection::open_in_memory().unwrap();
    let lowered_limit = 64 * 1024;
    conn.set_limit(SqliteLimit::SQLITE_LIMIT_LENGTH, lowered_limit);

    let result = with_zed_length_preflight(&conn, || {
        conn.query_row::<i64, _, _>("select missing from missing_table", [], |row| row.get(0))
    });

    assert!(result.is_err());
    assert_eq!(conn.limit(SqliteLimit::SQLITE_LIMIT_LENGTH), lowered_limit);
}

#[test]
fn zed_fetcher_rejects_oversize_wrong_class_and_preserves_sibling() {
    let conn = Connection::open_in_memory().unwrap();
    create_threads_schema(&conn);
    let oversize_text = "x".repeat(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES + 1);
    conn.execute(
        "INSERT INTO threads(id, summary, updated_at, data_type, data) \
         VALUES ('thread-oversize', 'summary', '2026-07-18T12:00:00Z', 'json', ?1)",
        [&oversize_text],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO threads(id, summary, updated_at, data_type, data) \
         VALUES ('thread-z-healthy', 'summary', '2026-07-18T12:00:00Z', 'json', x'7b7d')",
        [],
    )
    .unwrap();
    let sqlite_value_limit = 64 * 1024;
    conn.set_limit(SqliteLimit::SQLITE_LIMIT_LENGTH, sqlite_value_limit);
    let columns = zed_thread_columns(&conn).unwrap();
    let mut fetcher = ZedRowFetcher::new(
        &conn,
        &columns,
        ProviderRecordKind::new(super::super::ZED_RECORD_KIND).unwrap(),
    )
    .unwrap();
    let source = SourceObservation::new(
        CaptureProvider::Zed,
        ZED_THREADS_SQLITE_SOURCE_FORMAT,
        "zed-test-source",
        "zed-test-revision",
        "zed-test-stream",
        super::super::ZED_CAPTURE_REVISION,
        super::super::ZED_POLICY_REVISION,
        None,
    )
    .unwrap();
    let mut producer = SqliteLogicalRowBatchProducer::new(
        source,
        initial_zed_position().unwrap(),
        move |position| fetcher.fetch(position),
    );
    let batch = producer.next_batch().unwrap().unwrap();
    assert_eq!(batch.records().len(), 2);
    assert!(matches!(
        batch.records()[0].payload(),
        CapturedRecordPayload::StructuralRejection {
            kind: StructuralRejectionKind::OversizeRecord,
            ..
        }
    ));
    assert!(matches!(
        batch.records()[1].payload(),
        CapturedRecordPayload::SqliteValues(_)
    ));
    assert_eq!(
        conn.limit(SqliteLimit::SQLITE_LIMIT_LENGTH),
        sqlite_value_limit,
        "integer-only size preflight must restore the hydration cap"
    );
    assert_eq!(
        decode_zed_position(batch.range_end())
            .unwrap()
            .unwrap()
            .phase,
        ZedCapturePhase::Exhausted
    );
}

#[test]
fn zed_native_id_keyset_is_indexed_and_near_tail_work_is_bounded() {
    let conn = Connection::open_in_memory().unwrap();
    create_threads_schema(&conn);
    let tx = conn.unchecked_transaction().unwrap();
    for index in 0..2_048_i64 {
        tx.execute(
            "INSERT INTO threads(id, summary, updated_at, data_type, data) \
             VALUES (?1, 'summary', '2026-07-18T12:00:00Z', 'json', x'7b7d')",
            [format!("thread-{index:04}")],
        )
        .unwrap();
    }
    tx.commit().unwrap();

    let plan = conn
        .prepare(&format!(
            "explain query plan {}",
            zed_next_candidate_sql("0", "0")
        ))
        .unwrap()
        .query_map([2_047_i64], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
        .join(" | ");
    assert!(plan.contains("SEARCH threads USING"), "{plan}");
    assert!(plan.contains("(id>?)"), "{plan}");
    assert!(!plan.contains("USE TEMP B-TREE"), "{plan}");

    let operations = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&operations);
    conn.progress_handler(
        1,
        Some(move || {
            observed.fetch_add(1, Ordering::Relaxed);
            false
        }),
    );
    let columns = zed_thread_columns(&conn).unwrap();
    let mut fetcher = ZedRowFetcher::new(
        &conn,
        &columns,
        ProviderRecordKind::new(super::super::ZED_RECORD_KIND).unwrap(),
    )
    .unwrap();
    let start = encode_zed_position(ZedKeyset {
        phase: ZedCapturePhase::Rows,
        next_ordinal: 2_047,
        rowid: 2_047,
    })
    .unwrap();
    let tail = fetcher.fetch(start).unwrap().unwrap();
    assert_eq!(tail.ordinal(), 2_047);
    assert_eq!(
        decode_zed_position(tail.next_position())
            .unwrap()
            .unwrap()
            .phase,
        ZedCapturePhase::Exhausted
    );
    let terminal_operations = operations.load(Ordering::Relaxed);
    assert!(
        terminal_operations < 2_000,
        "near-tail lookup revisited too much source state: {terminal_operations}"
    );
    assert!(fetcher
        .fetch(tail.next_position().clone())
        .unwrap()
        .is_none());
    assert_eq!(operations.load(Ordering::Relaxed), terminal_operations);
    conn.progress_handler(0, None::<fn() -> bool>);
}

#[test]
fn zed_native_id_resume_is_exact_when_rowids_run_backward() {
    let conn = Connection::open_in_memory().unwrap();
    create_threads_schema(&conn);
    for id in ["thread-b", "thread-a"] {
        conn.execute(
            "INSERT INTO threads(id, summary, updated_at, data_type, data) \
             VALUES (?1, 'summary', '2026-07-18T12:00:00Z', 'json', x'7b7d')",
            [id],
        )
        .unwrap();
    }
    let columns = zed_thread_columns(&conn).unwrap();
    let mut fetcher = ZedRowFetcher::new(
        &conn,
        &columns,
        ProviderRecordKind::new(super::super::ZED_RECORD_KIND).unwrap(),
    )
    .unwrap();
    let first = fetcher
        .fetch(initial_zed_position().unwrap())
        .unwrap()
        .unwrap();
    let first_position = first.next_position().clone();
    let first_keyset = decode_zed_position(&first_position).unwrap().unwrap();
    assert_eq!(first_keyset.rowid, 2);
    assert_eq!(first_keyset.next_ordinal, 1);
    assert_eq!(first_keyset.phase, ZedCapturePhase::Rows);

    let second = fetcher.fetch(first_position).unwrap().unwrap();
    let second_keyset = decode_zed_position(second.next_position())
        .unwrap()
        .unwrap();
    assert_eq!(second_keyset.rowid, 1);
    assert_eq!(second_keyset.next_ordinal, 2);
    assert_eq!(second_keyset.phase, ZedCapturePhase::Exhausted);
}

#[test]
fn zed_checkpoint_is_fixed_size_for_large_native_ordering_key() {
    let conn = Connection::open_in_memory().unwrap();
    create_threads_schema(&conn);
    let large_id = "z".repeat(200 * 1024);
    conn.execute(
        "INSERT INTO threads(id, summary, updated_at, data_type, data) \
         VALUES (?1, 'summary', '2026-07-18T12:00:00Z', 'json', x'7b7d')",
        [&large_id],
    )
    .unwrap();
    let columns = zed_thread_columns(&conn).unwrap();
    let mut fetcher = ZedRowFetcher::new(
        &conn,
        &columns,
        ProviderRecordKind::new(super::super::ZED_RECORD_KIND).unwrap(),
    )
    .unwrap();
    let row = fetcher
        .fetch(initial_zed_position().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(row.next_position().value().len(), ZED_POSITION_BYTES);
    assert!(!row
        .next_position()
        .value()
        .windows(large_id.len())
        .any(|window| window == large_id.as_bytes()));
}

#[test]
fn zed_requires_native_unique_id_index() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE threads(\
            id TEXT NOT NULL, summary TEXT NOT NULL, updated_at TEXT NOT NULL,\
            data_type TEXT NOT NULL, data BLOB NOT NULL\
         );",
    )
    .unwrap();
    let columns = zed_thread_columns(&conn).unwrap();
    let error = ZedRowFetcher::new(
        &conn,
        &columns,
        ProviderRecordKind::new(super::super::ZED_RECORD_KIND).unwrap(),
    )
    .err()
    .unwrap();
    assert!(matches!(
        error,
        CaptureError::InvalidPayload(ref message)
            if message.contains("unique ascending BINARY index on (id)")
    ));
}
