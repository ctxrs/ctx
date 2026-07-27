use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use rusqlite::limits::Limit as SqliteLimit;

use super::super::{KIRO_CAPTURE_REVISION, KIRO_POLICY_REVISION};
use super::*;
use crate::captured_batch::{
    sqlite_logical_rows::SqliteLogicalRowBatchProducer, CAPTURE_BATCH_MAX_RECORDS,
};
use crate::provider::sqlite::with_sqlite_read_snapshot;

fn create_kiro_tables(conn: &Connection) {
    conn.execute_batch(
        "create table conversations_v2 (
            key text not null,
            conversation_id text not null,
            value text not null,
            created_at integer,
            updated_at integer
        );
        create table conversations (
            key text not null,
            value text not null
        );",
    )
    .unwrap();
}

fn test_source() -> SourceObservation {
    SourceObservation::new(
        CaptureProvider::KiroCli,
        KIRO_SQLITE_SOURCE_FORMAT,
        "kiro-sqlite:test",
        "kiro-snapshot:test",
        "provider:kiro:test",
        KIRO_CAPTURE_REVISION,
        KIRO_POLICY_REVISION,
        None,
    )
    .unwrap()
}

fn produce_kiro_batches(conn: &Connection) -> Vec<CapturedBatch> {
    let tables = kiro_conversation_tables(conn).unwrap();
    let mut fetcher = KiroRowFetcher::new(conn, tables).unwrap();
    let mut producer = SqliteLogicalRowBatchProducer::new(
        test_source(),
        initial_kiro_position().unwrap(),
        move |position| fetcher.fetch(position),
    );
    let mut batches = Vec::new();
    while let Some(batch) = producer
        .next_batch()
        .map_err(kiro_sqlite_batch_error)
        .unwrap()
    {
        batches.push(batch);
    }
    batches
}

#[test]
fn kiro_batches_sixty_four_rows_and_releases_each_read_snapshot() {
    let conn = Connection::open_in_memory().unwrap();
    create_kiro_tables(&conn);
    for index in 1..=CAPTURE_BATCH_MAX_RECORDS {
        conn.execute(
            "insert into conversations_v2 (
                rowid, key, conversation_id, value, created_at, updated_at
             ) values (?1, ?2, ?3, '{}', ?4, ?4)",
            rusqlite::params![
                index as i64,
                format!("/workspace/v2-{index}"),
                format!("v2-{index}"),
                index as i64,
            ],
        )
        .unwrap();
    }
    conn.execute(
        "insert into conversations (rowid, key, value) values (7, '/workspace/legacy', '{}')",
        [],
    )
    .unwrap();

    let tables = kiro_conversation_tables(&conn).unwrap();
    let mut fetcher = KiroRowFetcher::new(&conn, tables).unwrap();
    let mut producer = SqliteLogicalRowBatchProducer::new(
        test_source(),
        initial_kiro_position().unwrap(),
        move |position| fetcher.fetch(position),
    );

    let first = with_sqlite_read_snapshot(&conn, || {
        producer.next_batch().map_err(kiro_sqlite_batch_error)
    })
    .unwrap()
    .unwrap();
    assert_eq!(first.records().len(), CAPTURE_BATCH_MAX_RECORDS);
    let first_end = decode_kiro_position(first.range_end()).unwrap().unwrap();
    assert_eq!(first_end.phase, KiroConversationPhase::V2);
    assert_eq!(first_end.next_ordinal, CAPTURE_BATCH_MAX_RECORDS as u64);
    assert_eq!(first_end.rowid, CAPTURE_BATCH_MAX_RECORDS as i64);
    assert!(conn.is_autocommit());

    let second = with_sqlite_read_snapshot(&conn, || {
        producer.next_batch().map_err(kiro_sqlite_batch_error)
    })
    .unwrap()
    .unwrap();
    assert_eq!(second.records().len(), 1);
    let second_end = decode_kiro_position(second.range_end()).unwrap().unwrap();
    assert_eq!(second_end.phase, KiroConversationPhase::Legacy);
    assert_eq!(
        second_end.next_ordinal,
        CAPTURE_BATCH_MAX_RECORDS as u64 + 1
    );
    assert_eq!(second_end.rowid, 7);
    assert!(conn.is_autocommit());

    let exhausted = with_sqlite_read_snapshot(&conn, || {
        producer.next_batch().map_err(kiro_sqlite_batch_error)
    })
    .unwrap();
    assert!(exhausted.is_none());
    assert!(conn.is_autocommit());
}

#[test]
fn kiro_keyset_resumes_exactly_across_table_phases() {
    let conn = Connection::open_in_memory().unwrap();
    create_kiro_tables(&conn);
    conn.execute_batch(
        "insert into conversations_v2 values ('/v2-1', 'v2-1', '{}', 1, 1);
         insert into conversations_v2 values ('/v2-2', 'v2-2', '{}', 2, 2);
         insert into conversations values ('/legacy', '{}');",
    )
    .unwrap();
    let tables = kiro_conversation_tables(&conn).unwrap();
    let mut fetcher = KiroRowFetcher::new(&conn, tables).unwrap();

    let first = fetcher
        .fetch(initial_kiro_position().unwrap())
        .unwrap()
        .unwrap();
    let first_position = first.next_position().clone();
    let first_keyset = decode_kiro_position(&first_position).unwrap().unwrap();
    assert_eq!(first_keyset.phase, KiroConversationPhase::V2);
    assert_eq!(first_keyset.next_ordinal, 1);
    assert_eq!(first_keyset.rowid, 1);

    let second = fetcher.fetch(first_position).unwrap().unwrap();
    let second_position = second.next_position().clone();
    let second_keyset = decode_kiro_position(&second_position).unwrap().unwrap();
    assert_eq!(second_keyset.phase, KiroConversationPhase::V2);
    assert_eq!(second_keyset.next_ordinal, 2);
    assert_eq!(second_keyset.rowid, 2);

    let legacy = fetcher.fetch(second_position).unwrap().unwrap();
    let legacy_keyset = decode_kiro_position(legacy.next_position())
        .unwrap()
        .unwrap();
    assert_eq!(legacy_keyset.phase, KiroConversationPhase::Legacy);
    assert_eq!(legacy_keyset.next_ordinal, 3);
    assert_eq!(legacy_keyset.rowid, 1);
    assert!(fetcher
        .fetch(legacy.next_position().clone())
        .unwrap()
        .is_none());
}

#[test]
fn kiro_next_rowid_seeks_are_indexed_and_near_tail_work_is_bounded() {
    let conn = Connection::open_in_memory().unwrap();
    create_kiro_tables(&conn);
    let transaction = conn.unchecked_transaction().unwrap();
    for rowid in 1..=2_048_i64 {
        transaction
            .execute(
                "insert into conversations_v2 (
                    rowid, key, conversation_id, value, created_at, updated_at
                 ) values (?1, ?2, ?3, '{}', ?1, ?1)",
                rusqlite::params![
                    rowid,
                    format!("/workspace/v2-{rowid}"),
                    format!("v2-{rowid}"),
                ],
            )
            .unwrap();
        transaction
            .execute(
                "insert into conversations (rowid, key, value) values (?1, ?2, '{}')",
                rusqlite::params![rowid, format!("/workspace/legacy-{rowid}")],
            )
            .unwrap();
    }
    transaction.commit().unwrap();

    for (select_sql, table) in [
        (KIRO_V2_CANDIDATE_SELECT_SQL, "conversations_v2"),
        (KIRO_LEGACY_CANDIDATE_SELECT_SQL, "conversations"),
    ] {
        let sql = kiro_candidate_sql(select_sql, KiroCandidateSeek::Next);
        let plan = conn
            .prepare(&format!("explain query plan {sql}"))
            .unwrap()
            .query_map([2_047_i64], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
            .join(" | ");
        assert!(plan.contains(&format!("SEARCH {table}")), "{plan}");
        assert!(plan.contains("rowid>?"), "{plan}");
        assert!(!plan.contains("SCAN"), "{plan}");
        assert!(!plan.contains("USE TEMP B-TREE"), "{plan}");
    }

    let tables = kiro_conversation_tables(&conn).unwrap();
    let mut fetcher = KiroRowFetcher::new(&conn, tables).unwrap();
    let operations = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&operations);
    conn.progress_handler(
        1,
        Some(move || {
            observed.fetch_add(1, Ordering::Relaxed);
            false
        }),
    );
    for (phase, ordinal) in [
        (KiroConversationPhase::V2, 2_047_u64),
        (KiroConversationPhase::Legacy, 4_095_u64),
    ] {
        operations.store(0, Ordering::Relaxed);
        let tail = fetcher
            .fetch(
                encode_kiro_position(KiroKeyset {
                    phase,
                    next_ordinal: ordinal,
                    rowid: 2_047,
                })
                .unwrap(),
            )
            .unwrap()
            .unwrap();
        let end = decode_kiro_position(tail.next_position()).unwrap().unwrap();
        assert_eq!(end.phase, phase);
        assert_eq!(end.rowid, 2_048);
        let phase_operations = operations.load(Ordering::Relaxed);
        assert!(
            phase_operations < 2_000,
            "Kiro {phase:?} near-tail seek used {phase_operations} SQLite VM operations"
        );
    }
    conn.progress_handler(0, None::<fn() -> bool>);
}

#[test]
fn kiro_length_preflight_restores_limit_after_query_error() {
    let conn = Connection::open_in_memory().unwrap();
    let lowered_limit = 64 * 1024;
    conn.set_limit(SqliteLimit::SQLITE_LIMIT_LENGTH, lowered_limit);

    let result = with_kiro_length_preflight(&conn, || {
        conn.query_row::<i64, _, _>("select missing from missing_table", [], |row| row.get(0))
    });

    assert!(result.is_err());
    assert_eq!(conn.limit(SqliteLimit::SQLITE_LIMIT_LENGTH), lowered_limit);
}

#[test]
fn kiro_preflight_under_capped_connection_rejects_oversize_and_preserves_progress() {
    let conn = Connection::open_in_memory().unwrap();
    create_kiro_tables(&conn);
    conn.execute(
        "insert into conversations_v2 (
            rowid, key, conversation_id, value, created_at, updated_at
         ) values (1, '/oversize', 'oversize', '{}', zeroblob(?1), 1)",
        [i64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES).unwrap()],
    )
    .unwrap();
    conn.execute_batch(
        "insert into conversations_v2 (
            rowid, key, conversation_id, value, created_at, updated_at
         ) values (2, '/healthy-v2', 'healthy-v2', '{}', 2, 2);
         insert into conversations (rowid, key, value)
         values (7, '/healthy-legacy', '{}');",
    )
    .unwrap();

    let expected = produce_kiro_batches(&conn);
    let lowered_limit = 64 * 1024;
    conn.set_limit(SqliteLimit::SQLITE_LIMIT_LENGTH, lowered_limit);
    let actual = produce_kiro_batches(&conn);

    assert_eq!(actual, expected);
    assert_eq!(conn.limit(SqliteLimit::SQLITE_LIMIT_LENGTH), lowered_limit);
    assert_eq!(actual.len(), 1);
    let records = actual[0].records();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0].ordinal(), 0);
    assert_eq!(records[1].ordinal(), 1);
    assert_eq!(records[2].ordinal(), 2);
    assert!(matches!(
        records[0].payload(),
        CapturedRecordPayload::StructuralRejection { .. }
    ));
    let CapturedRecordPayload::SqliteValues(v2_values) = records[1].payload() else {
        panic!("healthy Kiro v2 sibling was not captured as SQLite values");
    };
    assert_eq!(
        decode_kiro_conversation(records[1].record_kind().as_str(), v2_values)
            .unwrap()
            .key,
        "/healthy-v2"
    );
    let CapturedRecordPayload::SqliteValues(legacy_values) = records[2].payload() else {
        panic!("healthy Kiro legacy sibling was not captured as SQLite values");
    };
    assert_eq!(
        decode_kiro_conversation(records[2].record_kind().as_str(), legacy_values)
            .unwrap()
            .key,
        "/healthy-legacy"
    );

    let end = decode_kiro_position(actual[0].range_end())
        .unwrap()
        .unwrap();
    assert_eq!(end.phase, KiroConversationPhase::Legacy);
    assert_eq!(end.next_ordinal, 3);
    assert_eq!(end.rowid, 7);
}

#[test]
fn kiro_capped_connection_rejects_malformed_storage_classes_and_keeps_siblings() {
    let conn = Connection::open_in_memory().unwrap();
    create_kiro_tables(&conn);
    conn.execute_batch(
        "insert into conversations_v2 values (
             zeroblob(1), 'malformed-key', '{}', 1, 1
         );
         insert into conversations_v2 values (
             '/malformed-conversation', zeroblob(1), '{}', 2, 2
         );
         insert into conversations_v2 values (
             '/malformed-value', 'malformed-value', zeroblob(1), 3, 3
         );
         insert into conversations_v2 values (
             '/malformed-created', 'malformed-created', '{}', zeroblob(1), 4
         );
         insert into conversations_v2 values (
             '/malformed-updated', 'malformed-updated', '{}', 5, 'not-an-integer'
         );
         insert into conversations_v2 values (
             '/healthy-v2', 'healthy-v2', '{}', 6, 6
         );
         insert into conversations values (zeroblob(1), '{}');
         insert into conversations values ('/malformed-legacy-value', zeroblob(1));
         insert into conversations values ('/healthy-legacy', '{}');",
    )
    .unwrap();
    let lowered_limit = 64 * 1024;
    conn.set_limit(SqliteLimit::SQLITE_LIMIT_LENGTH, lowered_limit);

    let batches = produce_kiro_batches(&conn);

    assert_eq!(conn.limit(SqliteLimit::SQLITE_LIMIT_LENGTH), lowered_limit);
    assert_eq!(batches.len(), 1);
    let records = batches[0].records();
    assert_eq!(records.len(), 9);
    assert_eq!(
        records
            .iter()
            .map(|record| record.record_kind().as_str())
            .collect::<Vec<_>>(),
        vec![
            KIRO_REJECTED_RECORD_KIND,
            KIRO_REJECTED_RECORD_KIND,
            KIRO_REJECTED_RECORD_KIND,
            KIRO_REJECTED_RECORD_KIND,
            KIRO_REJECTED_RECORD_KIND,
            KIRO_V2_RECORD_KIND,
            KIRO_REJECTED_RECORD_KIND,
            KIRO_REJECTED_RECORD_KIND,
            KIRO_LEGACY_RECORD_KIND,
        ]
    );
    assert!(records
        .iter()
        .enumerate()
        .all(|(ordinal, record)| record.ordinal() == ordinal as u64));
    let end = decode_kiro_position(batches[0].range_end())
        .unwrap()
        .unwrap();
    assert_eq!(end.phase, KiroConversationPhase::Legacy);
    assert_eq!(end.next_ordinal, 9);
    assert_eq!(end.rowid, 3);

    let database_path = PathBuf::from("/tmp/kiro-malformed-storage.sqlite3");
    let mut projector = KiroCapturedBatchProjector {
        context: ProviderAdapterContext {
            machine_id: "kiro-malformed-storage-test".to_owned(),
            source_path: Some(database_path.clone()),
            source_root: Some(PathBuf::from("/workspace/kiro-root")),
            imported_at: DateTime::<Utc>::UNIX_EPOCH,
        },
        database_path,
        user_version: 0,
        schema_fingerprint: "kiro-schema-test".to_owned(),
    };
    let mut output = CollectingProjectionOutput::default();
    for record in records {
        projector.project_record(record, &mut output).unwrap();
    }

    assert_eq!(output.rejections.len(), 7);
    for (rejection, column) in output.rejections.iter().zip([
        "conversations_v2.key",
        "conversations_v2.conversation_id",
        "conversations_v2.value",
        "conversations_v2.created_at",
        "conversations_v2.updated_at",
        "conversations.key",
        "conversations.value",
    ]) {
        assert!(rejection.1.contains(column));
    }
    let provider_session_ids = output
        .normalizations
        .iter()
        .flat_map(|normalization| normalization.captures.iter())
        .map(|(_, capture)| capture.session.provider_session_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        provider_session_ids,
        vec!["healthy-v2", "conversations:/healthy-legacy:3"]
    );
}

#[derive(Default)]
struct CollectingProjectionOutput {
    normalizations: Vec<ProviderNormalizationResult>,
    rejections: Vec<(usize, String)>,
}

impl ProviderProjectionOutput for CollectingProjectionOutput {
    fn emit_normalization(
        &mut self,
        normalization: ProviderNormalizationResult,
    ) -> ProviderProjectionResult<()> {
        self.normalizations.push(normalization);
        Ok(())
    }

    fn reject_record(&mut self, line_number: usize, reason: String) {
        self.rejections.push((line_number, reason));
    }
}

#[test]
fn kiro_projection_preserves_session_and_source_identity() {
    let database_path = PathBuf::from("/tmp/kiro-projection.sqlite3");
    let context = ProviderAdapterContext {
        machine_id: "kiro-test-machine".to_owned(),
        source_path: Some(database_path.clone()),
        source_root: Some(PathBuf::from("/workspace/kiro-root")),
        imported_at: DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
    };
    let mut projector = KiroCapturedBatchProjector {
        context,
        database_path: database_path.clone(),
        user_version: 4,
        schema_fingerprint: "kiro-schema-test".to_owned(),
    };
    let value = json!({
        "history": [{
            "user": {
                "content": {"Prompt": {"prompt": "hello from Kiro"}},
                "timestamp": 1_784_332_800_000_i64
            },
            "assistant": {
                "Response": {"content": "hello back"},
                "timestamp": 1_784_332_801_000_i64
            }
        }]
    })
    .to_string();
    let record = CapturedRecord::sqlite_logical(
        0,
        kiro_locator(KiroConversationPhase::V2, 11).unwrap(),
        ProviderRecordKind::new(KIRO_V2_RECORD_KIND).unwrap(),
        vec![
            CapturedSqliteValue::Integer(11),
            CapturedSqliteValue::Text("/workspace/project".to_owned()),
            CapturedSqliteValue::Text("kiro-session-11".to_owned()),
            CapturedSqliteValue::Text(value),
            CapturedSqliteValue::Integer(1_784_332_800_000),
            CapturedSqliteValue::Integer(1_784_332_801_000),
        ],
    )
    .unwrap();
    let mut output = CollectingProjectionOutput::default();

    projector.project_record(&record, &mut output).unwrap();

    assert!(output.rejections.is_empty());
    let captures = output
        .normalizations
        .iter()
        .flat_map(|normalization| normalization.captures.iter())
        .collect::<Vec<_>>();
    assert_eq!(captures.len(), 2);
    for (_, capture) in captures {
        assert_eq!(capture.session.provider_session_id, "kiro-session-11");
        assert_eq!(
            capture.source.raw_source_path.as_deref(),
            database_path.to_str()
        );
        assert_eq!(
            capture.source.source_root.as_deref(),
            Some("/workspace/kiro-root")
        );
    }
}

#[test]
fn kiro_projection_streams_file_touches_after_the_capture() {
    let database_path = PathBuf::from("/tmp/kiro-touch-projection.sqlite3");
    let context = ProviderAdapterContext {
        machine_id: "kiro-test-machine".to_owned(),
        source_path: Some(database_path.clone()),
        source_root: Some(PathBuf::from("/workspace/kiro-root")),
        imported_at: DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
    };
    let mut projector = KiroCapturedBatchProjector {
        context,
        database_path,
        user_version: 4,
        schema_fingerprint: "kiro-schema-test".to_owned(),
    };
    let value = json!({
        "history": [{
            "assistant": {
                "ToolUse": {
                    "tool_uses": [
                        {"name": "write", "input": {"file_path": "/workspace/first.rs"}},
                        {"name": "write", "input": {"file_path": "/workspace/second.rs"}}
                    ]
                },
                "timestamp": 1_784_332_801_000_i64
            }
        }]
    })
    .to_string();
    let record = CapturedRecord::sqlite_logical(
        0,
        kiro_locator(KiroConversationPhase::V2, 12).unwrap(),
        ProviderRecordKind::new(KIRO_V2_RECORD_KIND).unwrap(),
        vec![
            CapturedSqliteValue::Integer(12),
            CapturedSqliteValue::Text("/workspace/project".to_owned()),
            CapturedSqliteValue::Text("kiro-session-12".to_owned()),
            CapturedSqliteValue::Text(value),
            CapturedSqliteValue::Integer(1_784_332_800_000),
            CapturedSqliteValue::Integer(1_784_332_801_000),
        ],
    )
    .unwrap();
    let mut output = CollectingProjectionOutput::default();

    projector.project_record(&record, &mut output).unwrap();

    assert!(output.rejections.is_empty());
    assert_eq!(output.normalizations.len(), 2);
    assert_eq!(output.normalizations[0].captures.len(), 1);
    assert_eq!(output.normalizations[0].files_touched.len(), 1);
    assert!(output.normalizations[1].captures.is_empty());
    assert_eq!(output.normalizations[1].files_touched.len(), 1);
    let touches = output
        .normalizations
        .iter()
        .flat_map(|normalization| normalization.files_touched.iter())
        .collect::<Vec<_>>();
    assert_eq!(touches[0].1.path, "/workspace/first.rs");
    assert_eq!(touches[0].1.provider_touch_index, 1_u64 << 16);
    assert_eq!(touches[1].1.path, "/workspace/second.rs");
    assert_eq!(touches[1].1.provider_touch_index, (1_u64 << 16) | 1);
    assert!(touches
        .iter()
        .all(|(_, touch)| touch.source_root.as_deref() == Some("/workspace/kiro-root")));
}
