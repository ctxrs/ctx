use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventRole};
use ctx_history_store::Store;
use rusqlite::{limits::Limit, Connection};

use crate::captured_batch::{
    CapturedRecordPayload, CapturedSqliteValue, SourceObservation,
    CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES, CAPTURE_BATCH_MAX_RECORDS,
};
use crate::provider::importer::{
    provider_path_identity, provider_source_cursor_stream_for_path, CapturedBatchProjector,
    CertifiedProviderCursor, ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::provider::sqlite::open_provider_sqlite_readonly;
use crate::provider::sqlite::{sqlite_schema_fingerprint, with_sqlite_read_snapshot};
use crate::{CaptureError, ProviderAdapterContext, ProviderNormalizationResult};

use super::*;

#[derive(Default)]
struct CollectingProjectionOutput {
    normalization: ProviderNormalizationResult,
    rejections: Vec<(usize, String)>,
}

impl ProviderProjectionOutput for CollectingProjectionOutput {
    fn emit_normalization(
        &mut self,
        mut normalization: ProviderNormalizationResult,
    ) -> ProviderProjectionResult<()> {
        self.normalization.summary.merge(normalization.summary);
        self.normalization
            .captures
            .append(&mut normalization.captures);
        self.normalization
            .files_touched
            .append(&mut normalization.files_touched);
        Ok(())
    }

    fn reject_record(&mut self, line_number: usize, reason: String) {
        self.rejections.push((line_number, reason));
    }
}

fn create_lingma_table(conn: &Connection) {
    conn.execute_batch(
        "create table chat_record ( \
                session_id text not null, request_id text, chat_prompt text, summary text, \
                error_result text, gmt_create integer, extra text \
             );",
    )
    .unwrap();
}

fn insert_lingma_row(
    conn: &Connection,
    session_id: &str,
    request_id: &str,
    prompt: &str,
    summary: Option<&str>,
    error: Option<&str>,
    timestamp: Option<i64>,
) {
    conn.execute(
        "insert into chat_record ( \
                session_id, request_id, chat_prompt, summary, error_result, gmt_create, extra \
             ) values (?1, ?2, ?3, ?4, ?5, ?6, '{\"source\":\"test\"}')",
        rusqlite::params![session_id, request_id, prompt, summary, error, timestamp],
    )
    .unwrap();
}

fn test_source(revision: &str) -> SourceObservation {
    SourceObservation::new(
        CaptureProvider::Lingma,
        LINGMA_SQLITE_SOURCE_FORMAT,
        "lingma-sqlite:test",
        revision,
        "provider:lingma:test",
        LINGMA_CAPTURE_REVISION,
        LINGMA_POLICY_REVISION,
        None,
    )
    .unwrap()
}

fn test_imported_at() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-07-18T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn test_schema(conn: &Connection) -> LingmaSchema {
    LingmaSchema::detect(conn).unwrap()
}

#[test]
fn lingma_text_decoder_is_authoritative_for_utf8_and_utf16() {
    let expected = "prefix 🧪 suffix";
    assert_eq!(
        decode_lingma_sqlite_text(LingmaSqliteEncoding::Utf8, expected.as_bytes()).as_deref(),
        Some(expected)
    );
    assert!(decode_lingma_sqlite_text(LingmaSqliteEncoding::Utf8, &[b'x', 0x80]).is_none());

    for encoding in [LingmaSqliteEncoding::Utf16Le, LingmaSqliteEncoding::Utf16Be] {
        let bytes = expected
            .encode_utf16()
            .flat_map(|unit| match encoding {
                LingmaSqliteEncoding::Utf16Le => unit.to_le_bytes(),
                LingmaSqliteEncoding::Utf16Be => unit.to_be_bytes(),
                LingmaSqliteEncoding::Utf8 => unreachable!(),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            decode_lingma_sqlite_text(encoding, &bytes).as_deref(),
            Some(expected),
            "{}",
            encoding.label()
        );
        assert!(decode_lingma_sqlite_text(encoding, &[0]).is_none());
        let lone_high = match encoding {
            LingmaSqliteEncoding::Utf16Le => 0xd800_u16.to_le_bytes(),
            LingmaSqliteEncoding::Utf16Be => 0xd800_u16.to_be_bytes(),
            LingmaSqliteEncoding::Utf8 => unreachable!(),
        };
        assert!(decode_lingma_sqlite_text(encoding, &lone_high).is_none());
    }
}

#[test]
fn lingma_producer_decodes_full_utf16_rows() {
    for encoding in [LingmaSqliteEncoding::Utf16Le, LingmaSqliteEncoding::Utf16Be] {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "encoding", encoding.label())
            .unwrap();
        create_lingma_table(&conn);
        insert_lingma_row(
            &conn,
            "utf16-session-🧪",
            "utf16-request",
            "utf16 prompt 🧪",
            Some("utf16 summary 🧪"),
            None,
            Some(1),
        );
        let bound_sql = lingma_retained_text_byte_bound_sql("chat_prompt", encoding);
        let bound: i64 = conn
            .query_row(
                &format!("select {bound_sql} from chat_record where rowid = 1"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(usize::try_from(bound).unwrap() >= "utf16 prompt 🧪".len());

        let mut producer = LingmaBatchProducer::new(
            &conn,
            test_source("lingma-snapshot:utf16"),
            initial_lingma_position().unwrap(),
            test_schema(&conn),
        )
        .unwrap();
        let batch = producer.next_batch().unwrap().unwrap();
        let CapturedRecordPayload::SqliteValues(values) = batch.records()[0].payload() else {
            panic!("UTF-16 row must be captured as SQLite logical values");
        };
        assert_eq!(
            values.get(1),
            Some(&CapturedSqliteValue::Text("utf16-session-🧪".to_owned()))
        );
        assert_eq!(
            values.get(3),
            Some(&CapturedSqliteValue::Text("utf16 prompt 🧪".to_owned()))
        );
        assert_eq!(
            values.get(4),
            Some(&CapturedSqliteValue::Text("utf16 summary 🧪".to_owned()))
        );
    }
}

#[test]
fn lingma_schema_requires_the_authoritative_table_shape() {
    let conn = Connection::open_in_memory().unwrap();
    assert!(matches!(
        LingmaSchema::detect(&conn),
        Err(CaptureError::InvalidPayload(message))
            if message == "Lingma local.db is missing required chat_record table"
    ));
    conn.execute_batch(
        "create table chat_record (session_id text, request_id text, chat_prompt text);",
    )
    .unwrap();
    assert!(matches!(
        LingmaSchema::detect(&conn),
        Err(CaptureError::InvalidPayload(message))
            if message.contains("Lingma chat_record table")
                && message.contains("summary")
    ));
}

#[test]
fn lingma_position_and_locator_bytes_remain_exact() {
    assert_eq!(initial_lingma_position().unwrap().value(), [0]);
    let keyset = LingmaKeyset {
        next_ordinal: 0x0102_0304_0506_0708,
        rowid: -2,
        exhausted: true,
    };
    let position = encode_lingma_position(keyset).unwrap();
    assert_eq!(position.kind(), LINGMA_POSITION_KIND);
    assert_eq!(
        position.value(),
        [1, 1, 2, 3, 4, 5, 6, 7, 8, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe, 1,]
    );
    assert_eq!(decode_lingma_position(&position).unwrap(), Some(keyset));

    let locator = lingma_locator(-2).unwrap();
    assert_eq!(locator.kind(), LINGMA_LOCATOR_KIND);
    assert_eq!(
        locator.value(),
        [0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe]
    );
}

#[test]
fn lingma_pages_sixty_five_rows_and_replays_the_exact_keyset() {
    let conn = Connection::open_in_memory().unwrap();
    create_lingma_table(&conn);
    for index in 0..=CAPTURE_BATCH_MAX_RECORDS {
        insert_lingma_row(
            &conn,
            &format!("session-{}", index % 3),
            &format!("request-{index}"),
            &format!("prompt {index}"),
            Some("summary"),
            None,
            Some(1_700_000_000_i64.saturating_add(index as i64)),
        );
    }
    let source = test_source("lingma-snapshot:page");
    let mut producer = LingmaBatchProducer::new(
        &conn,
        source.clone(),
        initial_lingma_position().unwrap(),
        test_schema(&conn),
    )
    .unwrap();
    let first = with_sqlite_read_snapshot(&conn, || producer.next_batch())
        .unwrap()
        .unwrap();
    assert_eq!(first.records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert_eq!(first.records()[0].ordinal(), 0);
    assert_eq!(first.records()[63].ordinal(), 63);
    assert!(conn.is_autocommit());
    let replay_position = first.range_end().clone();
    let keyset = decode_lingma_position(&replay_position).unwrap().unwrap();
    assert_eq!(keyset.next_ordinal, 64);
    assert_eq!(keyset.rowid, 64);
    assert!(!keyset.exhausted);
    drop(producer);

    let mut replay_from_start = LingmaBatchProducer::new(
        &conn,
        source.clone(),
        initial_lingma_position().unwrap(),
        test_schema(&conn),
    )
    .unwrap();
    let replayed_first = with_sqlite_read_snapshot(&conn, || replay_from_start.next_batch())
        .unwrap()
        .unwrap();
    assert_eq!(replayed_first, first);
    drop(replay_from_start);

    let mut replay =
        LingmaBatchProducer::new(&conn, source, replay_position, test_schema(&conn)).unwrap();
    let second = with_sqlite_read_snapshot(&conn, || replay.next_batch())
        .unwrap()
        .unwrap();
    assert_eq!(second.records().len(), 1);
    assert_eq!(second.records()[0].ordinal(), 64);
    assert!(
        decode_lingma_position(second.range_end())
            .unwrap()
            .unwrap()
            .exhausted
    );
    assert!(with_sqlite_read_snapshot(&conn, || replay.next_batch())
        .unwrap()
        .is_none());
    assert!(conn.is_autocommit());
}

#[test]
fn lingma_long_session_uses_two_indexed_queries_per_batch_including_construction() {
    let conn = Connection::open_in_memory().unwrap();
    create_lingma_table(&conn);
    for index in 0..130_i64 {
        insert_lingma_row(
            &conn,
            "one-long-session",
            &format!("request-{index}"),
            "bounded row-local prompt",
            None,
            None,
            Some(10_000_i64.saturating_sub(index)),
        );
    }
    let mut producer = LingmaBatchProducer::new(
        &conn,
        test_source("lingma-snapshot:long-query-count"),
        initial_lingma_position().unwrap(),
        test_schema(&conn),
    )
    .unwrap();
    assert_eq!(producer.executed_queries, 0);

    let mut batches = 0_usize;
    let mut records = 0_usize;
    while let Some(batch) = producer.next_batch().unwrap() {
        batches = batches.saturating_add(1);
        records = records.saturating_add(batch.records().len());
    }
    assert_eq!(batches, 3);
    assert_eq!(records, 130);
    assert_eq!(producer.executed_queries, 6);
}

#[test]
fn lingma_blank_heavy_source_pages_native_rows_and_replays_terminal_cursor() {
    let conn = Connection::open_in_memory().unwrap();
    create_lingma_table(&conn);
    for index in 0..2_048_i64 {
        insert_lingma_row(
            &conn,
            "blank-session",
            &format!("blank-{index}"),
            "   ",
            None,
            None,
            Some(index),
        );
    }
    let operations = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&operations);
    conn.progress_handler(
        1,
        Some(move || {
            observed.fetch_add(1, Ordering::Relaxed);
            false
        }),
    );
    let mut producer = LingmaBatchProducer::new(
        &conn,
        test_source("lingma-snapshot:blank-heavy"),
        initial_lingma_position().unwrap(),
        test_schema(&conn),
    )
    .unwrap();
    let first = producer.next_batch().unwrap().unwrap();
    conn.progress_handler(0, None::<fn() -> bool>);
    assert_eq!(first.records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert!(first
        .records()
        .iter()
        .all(|record| record.record_kind().as_str() == LINGMA_SKIPPED_RECORD_KIND));
    assert_eq!(producer.executed_queries, 2);
    assert!(operations.load(Ordering::Relaxed) < 15_000);
    assert_eq!(
        decode_lingma_position(first.range_end())
            .unwrap()
            .unwrap()
            .rowid,
        64
    );

    let mut final_position = first.range_end().clone();
    let mut batch_count = 1_usize;
    while let Some(batch) = producer.next_batch().unwrap() {
        batch_count = batch_count.saturating_add(1);
        final_position = batch.range_end().clone();
        if batch.source_exhausted() {
            assert_eq!(batch.records().len(), CAPTURE_BATCH_MAX_RECORDS);
        }
    }
    assert_eq!(batch_count, 32);
    assert_eq!(producer.executed_queries, 64);
    assert!(
        decode_lingma_position(&final_position)
            .unwrap()
            .unwrap()
            .exhausted
    );

    let mut replay = LingmaBatchProducer::new(
        &conn,
        test_source("lingma-snapshot:blank-heavy"),
        final_position,
        test_schema(&conn),
    )
    .unwrap();
    assert_eq!(replay.executed_queries, 0);
    assert!(replay.next_batch().unwrap().is_none());
    assert_eq!(replay.executed_queries, 0);
}

#[test]
fn lingma_bounded_projection_uses_row_local_metadata_and_native_event_order() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let path = directory.path().join("local.db");
    let writer = Connection::open(&path).unwrap();
    create_lingma_table(&writer);
    let trailing_space_title = format!("{} tail", "x".repeat(49));
    insert_lingma_row(
        &writer,
        "session-b",
        "b-late",
        "B late title must not win",
        Some("B late summary"),
        None,
        Some(300),
    );
    insert_lingma_row(
        &writer,
        "session-a",
        "a-timed",
        "A timed title must not win",
        None,
        Some("A timed failure"),
        Some(200),
    );
    insert_lingma_row(
        &writer,
        "session-b",
        "b-first",
        &trailing_space_title,
        Some("B first summary"),
        None,
        Some(100),
    );
    insert_lingma_row(
        &writer,
        "session-a",
        "a-first",
        "  A fallback title  ",
        Some("A fallback summary"),
        None,
        None,
    );
    drop(writer);
    let context = ProviderAdapterContext {
        machine_id: "lingma-equivalence-machine".to_owned(),
        source_path: Some(path.clone()),
        source_root: None,
        imported_at: test_imported_at(),
    };
    let conn = open_provider_sqlite_readonly(&path).unwrap();
    let user_version = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    let schema_fingerprint = sqlite_schema_fingerprint(&conn).unwrap();
    let mut producer = LingmaBatchProducer::new(
        &conn,
        test_source("lingma-snapshot:equivalence"),
        initial_lingma_position().unwrap(),
        test_schema(&conn),
    )
    .unwrap();
    let mut projector = LingmaCapturedBatchProjector::new(
        context,
        path.display().to_string(),
        user_version,
        schema_fingerprint,
    );
    let mut output = CollectingProjectionOutput::default();
    while let Some(batch) = with_sqlite_read_snapshot(&conn, || producer.next_batch()).unwrap() {
        for record in batch.records() {
            projector.project_record(record, &mut output).unwrap();
        }
    }

    assert!(output.rejections.is_empty());
    assert_eq!(
        output
            .normalization
            .captures
            .iter()
            .map(|(_, capture)| capture.session.provider_session_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "session-b",
            "session-b",
            "session-a",
            "session-a",
            "session-b",
            "session-b",
            "session-a",
            "session-a",
        ]
    );
    assert_eq!(
        output
            .normalization
            .captures
            .iter()
            .map(|(_, capture)| {
                let event = capture.event.as_ref().unwrap();
                (event.role, event.metadata["body_kind"].as_str().unwrap())
            })
            .collect::<Vec<_>>(),
        vec![
            (Some(EventRole::User), "chat_prompt"),
            (Some(EventRole::Assistant), "summary"),
            (Some(EventRole::User), "chat_prompt"),
            (Some(EventRole::Assistant), "error_result"),
            (Some(EventRole::User), "chat_prompt"),
            (Some(EventRole::Assistant), "summary"),
            (Some(EventRole::User), "chat_prompt"),
            (Some(EventRole::Assistant), "summary"),
        ]
    );
    assert!(output.normalization.files_touched.is_empty());
    assert!(output.normalization.captures.iter().all(|(_, capture)| {
        capture.session.metadata.get("title").is_none()
            && capture.session.metadata.get("row_count").is_none()
    }));
}

#[test]
fn lingma_preflight_rejects_oversize_before_hydration() {
    let conn = Connection::open_in_memory().unwrap();
    create_lingma_table(&conn);
    let oversize = i64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
        .unwrap()
        .saturating_add(1);
    conn.execute(
        "insert into chat_record (session_id, request_id, chat_prompt, gmt_create) \
             values ( \
                 'oversize-session', 'oversize', \
                 cast('earliest title' || zeroblob(?1) as text), 1 \
             )",
        [oversize],
    )
    .unwrap();
    insert_lingma_row(
        &conn,
        "oversize-session",
        "later-valid",
        "later title must not replace the earliest row",
        None,
        None,
        Some(2),
    );
    let storage_class: String = conn
        .query_row(
            "select typeof(chat_prompt) from chat_record where rowid = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(storage_class, "text");
    let lowered_limit = i32::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES).unwrap();
    conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, lowered_limit);
    assert_eq!(conn.limit(Limit::SQLITE_LIMIT_LENGTH), lowered_limit);
    let lazy_prompt_bytes: i64 = conn
        .query_row(
            "select octet_length(chat_prompt) from chat_record where rowid = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(lazy_prompt_bytes > i64::from(lowered_limit));
    let mut producer = LingmaBatchProducer::new(
        &conn,
        test_source("lingma-snapshot:oversize"),
        initial_lingma_position().unwrap(),
        test_schema(&conn),
    )
    .unwrap();
    let batch = producer.next_batch().unwrap().unwrap();

    assert_eq!(batch.records().len(), 2);
    assert!(matches!(
        batch.records()[0].payload(),
        CapturedRecordPayload::StructuralRejection { observed_bytes, .. }
            if *observed_bytes > CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES as u64
    ));
    let CapturedRecordPayload::SqliteValues(values) = batch.records()[1].payload() else {
        panic!("later Lingma row must be hydrated");
    };
    assert_eq!(lingma_values_rowid(values).unwrap(), 2);
}

#[test]
fn lingma_producer_preserves_null_and_blob_storage_behavior() {
    let conn = Connection::open_in_memory().unwrap();
    create_lingma_table(&conn);
    conn.execute(
        "insert into chat_record (session_id, request_id, chat_prompt, gmt_create) \
             values ('null-session', 'null-request', null, 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into chat_record (session_id, request_id, chat_prompt, gmt_create) \
             values ('blob-session', 'blob-request', cast(?1 as blob), 2)",
        ["  blob title  "],
    )
    .unwrap();

    let mut producer = LingmaBatchProducer::new(
        &conn,
        test_source("lingma-snapshot:null-and-blob"),
        initial_lingma_position().unwrap(),
        test_schema(&conn),
    )
    .unwrap();
    let batch = producer.next_batch().unwrap().unwrap();
    assert_eq!(batch.records().len(), 2);
    assert_eq!(
        batch.records()[0].record_kind().as_str(),
        LINGMA_SKIPPED_RECORD_KIND
    );
    let CapturedRecordPayload::SqliteValues(values) = batch.records()[1].payload() else {
        panic!("blob prompt must be captured as a logical row");
    };
    assert_eq!(lingma_values_rowid(values).unwrap(), 2);
    assert_eq!(
        values.get(3),
        Some(&CapturedSqliteValue::Text("  blob title  ".to_owned()))
    );
}

#[test]
fn lingma_malformed_text_rejects_once_and_keeps_siblings_and_cursor_progress() {
    let conn = Connection::open_in_memory().unwrap();
    create_lingma_table(&conn);
    conn.execute(
        "insert into chat_record (session_id, request_id, chat_prompt, gmt_create) \
             values ('malformed-session', 'malformed-request', ?1, 1)",
        [vec![b'x', 0x80]],
    )
    .unwrap();
    insert_lingma_row(
        &conn,
        "valid-session",
        "valid-request",
        "valid sibling",
        None,
        None,
        Some(2),
    );
    let mut producer = LingmaBatchProducer::new(
        &conn,
        test_source("lingma-snapshot:malformed-sibling"),
        initial_lingma_position().unwrap(),
        test_schema(&conn),
    )
    .unwrap();
    let batch = producer.next_batch().unwrap().unwrap();
    assert_eq!(batch.records().len(), 2);
    assert!(batch.source_exhausted());
    assert_eq!(batch.records()[1].ordinal(), 1);
    assert!(
        decode_lingma_position(batch.range_end())
            .unwrap()
            .unwrap()
            .exhausted
    );

    let mut projector = LingmaCapturedBatchProjector::new(
        ProviderAdapterContext {
            machine_id: "malformed-sibling-machine".to_owned(),
            source_path: None,
            source_root: None,
            imported_at: test_imported_at(),
        },
        "local.db".to_owned(),
        0,
        "test-schema".to_owned(),
    );
    let mut output = CollectingProjectionOutput::default();
    for record in batch.records() {
        projector.project_record(record, &mut output).unwrap();
    }
    assert_eq!(output.rejections.len(), 1);
    assert_eq!(output.rejections[0].0, 1);
    assert_eq!(output.normalization.captures.len(), 1);
    assert_eq!(
        output.normalization.captures[0]
            .1
            .session
            .provider_session_id,
        "valid-session"
    );
    assert!(producer.next_batch().unwrap().is_none());
}

#[test]
fn lingma_rank_resume_is_indexed_and_near_tail_work_is_bounded() {
    let conn = Connection::open_in_memory().unwrap();
    create_lingma_table(&conn);
    for index in 0..2_048_i64 {
        insert_lingma_row(
            &conn,
            "large-session",
            &format!("request-{index}"),
            "bounded prompt",
            None,
            None,
            Some(index),
        );
    }
    let start = encode_lingma_position(LingmaKeyset {
        next_ordinal: 2_047,
        rowid: 2_047,
        exhausted: false,
    })
    .unwrap();
    let operations = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&operations);
    conn.progress_handler(
        1,
        Some(move || {
            observed.fetch_add(1, Ordering::Relaxed);
            false
        }),
    );
    let mut producer = LingmaBatchProducer::new(
        &conn,
        test_source("lingma-snapshot:near-tail"),
        start,
        test_schema(&conn),
    )
    .unwrap();
    let plan = conn
        .prepare(&format!(
            "explain query plan {}",
            lingma_candidate_sql(true, LingmaSqliteEncoding::Utf8)
        ))
        .unwrap()
        .query_map([2_047_i64], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
        .join(" | ");
    assert!(
        plan.contains("SEARCH") && plan.contains("rowid>?"),
        "{plan}"
    );
    assert!(!plan.contains("USE TEMP B-TREE"), "{plan}");

    let after = decode_lingma_position(&producer.current_position).unwrap();
    let tail = producer.candidates(after).unwrap();
    conn.progress_handler(0, None::<fn() -> bool>);
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].rowid, 2_048);
    assert_eq!(producer.executed_queries, 1);
    assert!(operations.load(Ordering::Relaxed) < 5_000);
}

#[test]
fn lingma_terminal_cursor_skips_source_preparation_on_noop() {
    let conn = Connection::open_in_memory().unwrap();
    create_lingma_table(&conn);
    insert_lingma_row(
        &conn,
        "terminal-session",
        "terminal-request",
        "terminal prompt",
        None,
        None,
        Some(1),
    );
    let mut producer = LingmaBatchProducer::new(
        &conn,
        test_source("lingma-snapshot:terminal"),
        initial_lingma_position().unwrap(),
        test_schema(&conn),
    )
    .unwrap();
    let batch = producer.next_batch().unwrap().unwrap();
    let terminal = batch.range_end().clone();
    assert!(
        decode_lingma_position(&terminal)
            .unwrap()
            .unwrap()
            .exhausted
    );
    drop(producer);
    let mut noop = LingmaBatchProducer::new(
        &conn,
        test_source("lingma-snapshot:terminal"),
        terminal,
        test_schema(&conn),
    )
    .unwrap();
    assert!(noop.next_batch().unwrap().is_none());
    let temp_tables: i64 = conn
        .query_row("select count(*) from sqlite_temp_master", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(temp_tables, 0);
}

#[test]
fn lingma_public_route_publishes_one_exact_cursor_and_noops() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("local.db");
    let writer = Connection::open(&path).unwrap();
    create_lingma_table(&writer);
    insert_lingma_row(
        &writer,
        "public-session",
        "public-request",
        "public route",
        Some("public answer"),
        None,
        Some(1),
    );
    drop(writer);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let options = crate::LingmaSqliteImportOptions {
        machine_id: "lingma-public-route".to_owned(),
        source_path: Some(temp.path().join("logical-lingma-source")),
        imported_at: test_imported_at(),
        history_record_id: None,
        capture_work_limit: crate::CaptureWorkLimit::Drain,
        inventory_observation_token: None,
    };
    let first = crate::import_lingma_sqlite(&path, &mut store, options.clone()).unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert!(first.imported > 0);
    let cursor_path = provider_path_identity(&fs::canonicalize(&path).unwrap()).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Lingma,
        LINGMA_SQLITE_SOURCE_FORMAT,
        &cursor_path,
    );
    let cursor = store
        .get_sync_cursor(None, &options.machine_id, &stream)
        .unwrap()
        .unwrap();
    assert!(CertifiedProviderCursor::decode_if_certified(&cursor.cursor)
        .unwrap()
        .unwrap()
        .native_position()
        .value()
        .last()
        .is_some_and(|flag| *flag == 1));

    let second = crate::import_lingma_sqlite(&path, &mut store, options.clone()).unwrap();
    assert_eq!(second.imported, 0);
    assert_eq!(second.failed, 0);
    assert_eq!(
        store
            .get_sync_cursor(None, &options.machine_id, &stream)
            .unwrap()
            .unwrap(),
        cursor
    );
    assert!(store
        .session_by_external_session(CaptureProvider::Lingma, "public-session")
        .unwrap()
        .is_some());
    let capture_source = store
        .capture_source_by_external_session(CaptureProvider::Lingma, "public-session")
        .unwrap()
        .unwrap();
    assert_eq!(
        capture_source.descriptor.raw_source_path,
        options
            .source_path
            .as_ref()
            .map(|source_path| source_path.display().to_string())
    );
}

#[test]
fn lingma_blank_only_public_route_certifies_terminal_cursor_and_replays() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("blank-only-local.db");
    let writer = Connection::open(&path).unwrap();
    create_lingma_table(&writer);
    for index in 0..70_i64 {
        insert_lingma_row(
            &writer,
            "blank-only-session",
            &format!("blank-only-{index}"),
            " \t ",
            None,
            None,
            Some(index),
        );
    }
    drop(writer);
    let mut store = Store::open(temp.path().join("blank-only-store.sqlite")).unwrap();
    let options = crate::LingmaSqliteImportOptions {
        machine_id: "lingma-blank-only-route".to_owned(),
        source_path: Some(path.clone()),
        imported_at: test_imported_at(),
        history_record_id: None,
        capture_work_limit: crate::CaptureWorkLimit::Drain,
        inventory_observation_token: None,
    };
    let first = crate::import_lingma_sqlite(&path, &mut store, options.clone()).unwrap();
    assert_eq!(first.imported, 0);
    assert_eq!(first.failed, 0);
    let cursor_path = provider_path_identity(&fs::canonicalize(&path).unwrap()).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Lingma,
        LINGMA_SQLITE_SOURCE_FORMAT,
        &cursor_path,
    );
    let cursor = store
        .get_sync_cursor(None, &options.machine_id, &stream)
        .unwrap()
        .unwrap();
    let certified = CertifiedProviderCursor::decode_if_certified(&cursor.cursor)
        .unwrap()
        .unwrap();
    let terminal = decode_lingma_position(certified.native_position())
        .unwrap()
        .unwrap();
    assert_eq!(terminal.next_ordinal, 70);
    assert_eq!(terminal.rowid, 70);
    assert!(terminal.exhausted);

    let second = crate::import_lingma_sqlite(&path, &mut store, options).unwrap();
    assert_eq!(second.imported, 0);
    assert_eq!(second.failed, 0);
    assert_eq!(
        store
            .get_sync_cursor(None, "lingma-blank-only-route", &stream)
            .unwrap()
            .unwrap(),
        cursor
    );
    assert!(store
        .session_by_external_session(CaptureProvider::Lingma, "blank-only-session")
        .unwrap()
        .is_none());
}

#[test]
fn lingma_interleaved_append_matches_one_shot_store_without_aggregate_metadata() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("interleaved-local.db");
    let writer = Connection::open(&path).unwrap();
    create_lingma_table(&writer);
    for index in 0..65_i64 {
        insert_lingma_row(
            &writer,
            if index % 2 == 0 {
                "session-a"
            } else {
                "session-b"
            },
            &format!("request-{index}"),
            &format!("prompt {index}"),
            Some(&format!("summary {index}")),
            None,
            Some(10_000_i64.saturating_sub(index)),
        );
    }
    drop(writer);
    let options = crate::LingmaSqliteImportOptions {
        machine_id: "lingma-interleaved-parity".to_owned(),
        source_path: Some(path.clone()),
        imported_at: test_imported_at(),
        history_record_id: None,
        capture_work_limit: crate::CaptureWorkLimit::Drain,
        inventory_observation_token: None,
    };
    let mut appended = Store::open(temp.path().join("appended.sqlite")).unwrap();
    let initial = crate::import_lingma_sqlite(&path, &mut appended, options.clone()).unwrap();
    assert_eq!(initial.failed, 0, "{:?}", initial.failures);

    let writer = Connection::open(&path).unwrap();
    for index in 65..70_i64 {
        insert_lingma_row(
            &writer,
            if index % 2 == 0 {
                "session-a"
            } else {
                "session-b"
            },
            &format!("request-{index}"),
            &format!("prompt {index}"),
            Some(&format!("summary {index}")),
            None,
            Some(10_000_i64.saturating_sub(index)),
        );
    }
    drop(writer);
    let refreshed = crate::import_lingma_sqlite(&path, &mut appended, options.clone()).unwrap();
    assert_eq!(refreshed.failed, 0, "{:?}", refreshed.failures);

    let mut one_shot = Store::open(temp.path().join("one-shot.sqlite")).unwrap();
    let fresh = crate::import_lingma_sqlite(&path, &mut one_shot, options).unwrap();
    assert_eq!(fresh.failed, 0, "{:?}", fresh.failures);
    for external_id in ["session-a", "session-b"] {
        let appended_session = appended
            .session_by_external_session(CaptureProvider::Lingma, external_id)
            .unwrap()
            .unwrap();
        let one_shot_session = one_shot
            .session_by_external_session(CaptureProvider::Lingma, external_id)
            .unwrap()
            .unwrap();
        assert_eq!(appended_session.id, one_shot_session.id);
        assert_eq!(appended_session.started_at, one_shot_session.started_at);
        assert_eq!(appended_session.ended_at, one_shot_session.ended_at);
        assert_eq!(
            appended_session.sync.metadata["metadata"],
            one_shot_session.sync.metadata["metadata"]
        );
        assert!(appended_session.sync.metadata["metadata"]
            .get("title")
            .is_none());
        assert!(appended_session.sync.metadata["metadata"]
            .get("row_count")
            .is_none());
        assert_eq!(
            appended.events_for_session(appended_session.id).unwrap(),
            one_shot.events_for_session(one_shot_session.id).unwrap()
        );
    }
}

#[test]
fn lingma_releases_batch_snapshot_and_detects_source_mutation() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let path = directory.path().join("local.db");
    let writer = Connection::open(&path).unwrap();
    create_lingma_table(&writer);
    for index in 0..CAPTURE_BATCH_MAX_RECORDS {
        insert_lingma_row(
            &writer,
            "mutation-session",
            &format!("request-{index}"),
            "before mutation",
            None,
            None,
            Some(index as i64),
        );
    }
    drop(writer);
    let snapshot = lingma_source_snapshot(&path).unwrap();
    let reader = open_provider_sqlite_readonly(&path).unwrap();
    let mut producer = LingmaBatchProducer::new(
        &reader,
        test_source("lingma-snapshot:mutation"),
        initial_lingma_position().unwrap(),
        test_schema(&reader),
    )
    .unwrap();
    let first = with_sqlite_read_snapshot(&reader, || producer.next_batch())
        .unwrap()
        .unwrap();
    assert_eq!(first.records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert!(reader.is_autocommit());

    let writer = Connection::open(&path).unwrap();
    insert_lingma_row(
        &writer,
        "mutation-session",
        "after",
        "after mutation",
        None,
        None,
        Some(65),
    );
    drop(writer);
    assert!(!snapshot.revalidate(&path).unwrap());
    assert!(reader.is_autocommit());
}
