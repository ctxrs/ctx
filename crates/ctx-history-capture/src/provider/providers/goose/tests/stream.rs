use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use rusqlite::{limits::Limit as SqliteLimit, Connection};

use crate::captured_batch::sqlite_logical_rows::SqliteLogicalRowBatchProducer;
use crate::captured_batch::{
    CapturedRecordPayload, CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES, CAPTURE_BATCH_MAX_RECORDS,
};
use crate::provider::sqlite::with_sqlite_read_snapshot;
use crate::MAX_PROVIDER_SQLITE_VALUE_BYTES;

use super::super::{
    position::{decode_goose_position, initial_goose_position, GooseCapturePhase},
    schema::{
        goose_message_columns, goose_message_expressions, GOOSE_MESSAGE_RECORD_KIND,
        GOOSE_MESSAGE_VALUE_COUNT, GOOSE_SESSION_RECORD_KIND, GOOSE_SESSION_VALUE_COUNT,
    },
    stream::{
        goose_message_candidate_sql, goose_retained_length_expr, goose_sqlite_batch_error,
        GooseRowFetcher,
    },
};
use super::{create_goose_tables, insert_message, insert_session, test_source};

#[test]
fn goose_batches_resume_exactly_across_session_and_message_phases() {
    let conn = Connection::open_in_memory().unwrap();
    create_goose_tables(&conn);
    insert_session(&conn, "with-messages");
    insert_session(&conn, "empty-session");
    for index in 1..=CAPTURE_BATCH_MAX_RECORDS + 1 {
        insert_message(
            &conn,
            i64::try_from(index).unwrap(),
            "with-messages",
            &format!("message {index}"),
        );
    }
    let mut fetcher = GooseRowFetcher::new(&conn).unwrap();
    let source = test_source("goose-snapshot:batch-resume");
    let mut producer = SqliteLogicalRowBatchProducer::new(
        source.clone(),
        initial_goose_position().unwrap(),
        move |position| fetcher.fetch(position),
    );

    let first = with_sqlite_read_snapshot(&conn, || {
        producer.next_batch().map_err(goose_sqlite_batch_error)
    })
    .unwrap()
    .unwrap();
    assert_eq!(first.records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert!(first.records()[..2]
        .iter()
        .all(|record| record.record_kind().as_str() == GOOSE_SESSION_RECORD_KIND));
    assert!(conn.is_autocommit());
    let first_end = decode_goose_position(first.range_end()).unwrap().unwrap();
    assert_eq!(first_end.phase, GooseCapturePhase::Messages);
    assert_eq!(first_end.next_ordinal, CAPTURE_BATCH_MAX_RECORDS as u64);
    assert_eq!(first_end.rowid, CAPTURE_BATCH_MAX_RECORDS as i64 - 2);

    let mut replay_fetcher = GooseRowFetcher::new(&conn).unwrap();
    let mut replay =
        SqliteLogicalRowBatchProducer::new(source, first.range_end().clone(), move |position| {
            replay_fetcher.fetch(position)
        });
    let second = with_sqlite_read_snapshot(&conn, || {
        replay.next_batch().map_err(goose_sqlite_batch_error)
    })
    .unwrap()
    .unwrap();
    assert_eq!(second.records().len(), 3);
    assert_eq!(
        second.records()[0].ordinal(),
        CAPTURE_BATCH_MAX_RECORDS as u64
    );
    assert_eq!(
        second.records()[0].record_kind().as_str(),
        GOOSE_MESSAGE_RECORD_KIND
    );
    assert_eq!(
        second.records()[1].record_kind().as_str(),
        GOOSE_MESSAGE_RECORD_KIND
    );
    assert_eq!(
        second.records()[2].record_kind().as_str(),
        GOOSE_MESSAGE_RECORD_KIND
    );
    let second_end = decode_goose_position(second.range_end()).unwrap().unwrap();
    assert_eq!(second_end.phase, GooseCapturePhase::Messages);
    assert_eq!(
        second_end.next_ordinal,
        CAPTURE_BATCH_MAX_RECORDS as u64 + 3
    );
    assert!(conn.is_autocommit());
    assert!(with_sqlite_read_snapshot(&conn, || {
        replay.next_batch().map_err(goose_sqlite_batch_error)
    })
    .unwrap()
    .is_none());
    assert!(conn.is_autocommit());
}

#[test]
fn goose_preflight_rejects_oversize_child_before_payload_hydration() {
    let conn = Connection::open_in_memory().unwrap();
    create_goose_tables(&conn);
    insert_session(&conn, "oversize-session");
    conn.execute(
        "insert into messages (
            id, message_id, session_id, role, content_json, created_timestamp
         ) values (1, 'oversize', 'oversize-session', 'user', zeroblob(?1), 1)",
        [i64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES).unwrap()],
    )
    .unwrap();
    let production_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap();
    conn.set_limit(SqliteLimit::SQLITE_LIMIT_LENGTH, production_limit);
    let mut fetcher = GooseRowFetcher::new(&conn).unwrap();
    let mut producer = SqliteLogicalRowBatchProducer::new(
        test_source("goose-snapshot:oversize"),
        initial_goose_position().unwrap(),
        move |position| fetcher.fetch(position),
    );

    let batch = producer
        .next_batch()
        .map_err(goose_sqlite_batch_error)
        .unwrap()
        .unwrap();

    assert_eq!(batch.records().len(), 2);
    assert!(matches!(
        batch.records()[1].payload(),
        CapturedRecordPayload::StructuralRejection { .. }
    ));
    assert_eq!(
        conn.limit(SqliteLimit::SQLITE_LIMIT_LENGTH),
        production_limit
    );
}

#[test]
fn goose_rowid_keysets_are_indexed_and_alternating_parents_are_hydrated_once() {
    let conn = Connection::open_in_memory().unwrap();
    create_goose_tables(&conn);
    insert_session(&conn, "parent-a");
    insert_session(&conn, "parent-b");
    for id in 1..=256_i64 {
        let parent = if id % 2 == 0 { "parent-a" } else { "parent-b" };
        insert_message(&conn, id, parent, "bounded child-local read");
    }
    let message_columns = goose_message_columns(&conn).unwrap();
    let message_expressions = goose_message_expressions(&message_columns, "m");
    let message_lengths = goose_retained_length_expr(&message_expressions.retained);
    let next_sql = goose_message_candidate_sql(&message_lengths, true);
    let plan = conn
        .prepare(&format!("explain query plan {next_sql}"))
        .unwrap()
        .query_map([128_i64], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
        .join(" | ");
    assert!(
        plan.contains("SEARCH m USING INTEGER PRIMARY KEY (rowid>?)"),
        "{plan}"
    );
    assert!(plan.contains("SEARCH s USING"), "{plan}");
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
    let mut fetcher = GooseRowFetcher::new(&conn).unwrap();
    let mut position = initial_goose_position().unwrap();
    for _ in 0..258 {
        position = fetcher
            .fetch(position)
            .unwrap()
            .unwrap()
            .next_position()
            .clone();
    }
    conn.progress_handler(0, None::<fn() -> bool>);
    assert!(fetcher.fetch(position).unwrap().is_none());
    assert_eq!(fetcher.session_hydration_queries, 2);
    assert!(operations.load(Ordering::Relaxed) < 40_000);
}

#[test]
fn goose_large_parent_and_children_stay_child_local_and_read_parent_once() {
    let conn = Connection::open_in_memory().unwrap();
    create_goose_tables(&conn);
    insert_session(&conn, "large-parent");
    let parent_payload = "p".repeat(9 * 1024 * 1024);
    conn.execute(
        "update sessions set extension_data = ?1 where id = 'large-parent'",
        [&parent_payload],
    )
    .unwrap();
    let child_payload = "c".repeat(9 * 1024 * 1024);
    for id in 1..=3_i64 {
        insert_message(&conn, id, "large-parent", &child_payload);
    }
    let production_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap();
    conn.set_limit(SqliteLimit::SQLITE_LIMIT_LENGTH, production_limit);
    let mut fetcher = GooseRowFetcher::new(&conn).unwrap();
    let source = test_source("goose-snapshot:large-parent");
    let mut messages = 0;
    let mut sessions = 0;
    {
        let mut producer = SqliteLogicalRowBatchProducer::new(
            source,
            initial_goose_position().unwrap(),
            |position| fetcher.fetch(position),
        );
        while let Some(batch) = producer.next_batch().unwrap() {
            for record in batch.records() {
                let CapturedRecordPayload::SqliteValues(values) = record.payload() else {
                    panic!("individually representable Goose row was rejected as oversize");
                };
                match record.record_kind().as_str() {
                    GOOSE_MESSAGE_RECORD_KIND => {
                        messages += 1;
                        assert_eq!(
                            values.len(),
                            GOOSE_MESSAGE_VALUE_COUNT + 1,
                            "message record must remain child-local"
                        );
                    }
                    GOOSE_SESSION_RECORD_KIND => {
                        sessions += 1;
                        assert_eq!(values.len(), GOOSE_SESSION_VALUE_COUNT);
                    }
                    kind => panic!("unexpected Goose record kind {kind}"),
                }
            }
        }
    }
    assert_eq!(messages, 3);
    assert_eq!(sessions, 1);
    assert_eq!(fetcher.session_hydration_queries, 1);
    assert_eq!(
        conn.limit(SqliteLimit::SQLITE_LIMIT_LENGTH),
        production_limit
    );
}
